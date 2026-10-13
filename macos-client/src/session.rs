use crate::input::{Injector, Input};
use anyhow::{ensure, Result};
use shared::{
    connection::{self, Connection},
    Message,
};
use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};
use tokio::{
    sync::Notify,
    task::JoinSet,
    time::{interval, MissedTickBehavior},
};

/// Callback only marks a generation and wakes the session. No display queries
/// or input injection run inside a Quartz notification callback.
pub struct DisplayChanges {
    generation: AtomicU64,
    notify: Notify,
}
impl Default for DisplayChanges {
    fn default() -> Self {
        Self {
            generation: AtomicU64::new(0),
            notify: Notify::new(),
        }
    }
}
impl DisplayChanges {
    pub fn mark(&self) {
        self.generation.fetch_add(1, Ordering::Release);
        self.notify.notify_one();
    }
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }
}

/// The caller also releases on cancellation (signals). Input's Drop is the final
/// unwind guard. Display/permission failures close TLS, restoring host control.
pub async fn run<S, I, F>(
    conn: &mut Connection<S>,
    input: &mut Input<I>,
    limit: Duration,
    changes: &DisplayChanges,
    generation: u64,
    permission: F,
) -> Result<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
    I: Injector,
    F: FnMut() -> Result<()> + Send + 'static,
{
    let result: Result<()> = async {
        let mut tick = interval(connection::HEARTBEAT_INTERVAL);
        tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
        let mut last_rx = Instant::now();
        let mut serial = 0u64;
        let mut permission = Some(permission);
        let mut checks = JoinSet::new();
        let mut check_started = Instant::now();
        loop {
            tokio::select! {
                biased;
                _ = tick.tick() => {
                    ensure!(last_rx.elapsed() < limit, "host heartbeat expired");
                    ensure!(changes.generation() == generation, "display configuration changed");
                    // AXIsProcessTrustedWithOptions is a synchronous OS call.
                    // Running it on the input executor every 200 ms can pause
                    // posting and accumulate a burst. Keep at most one check
                    // in flight; a stalled check still fails the session closed.
                    if checks.is_empty() {
                        let mut check = permission.take().expect("permission checker available");
                        check_started = Instant::now();
                        checks.spawn_blocking(move || {
                            let result = check();
                            (check, result)
                        });
                    } else {
                        ensure!(check_started.elapsed() < limit, "permission check stalled");
                    }
                    serial = serial.wrapping_add(1);
                    conn.send(Message::Heartbeat(serial)).await?;
                }
                _ = changes.notify.notified() => {
                    ensure!(changes.generation() == generation, "display configuration changed");
                }
                completed = checks.join_next(), if !checks.is_empty() => {
                    let (check, result) = completed
                        .ok_or_else(|| anyhow::anyhow!("permission checker stopped"))?
                        .map_err(|_| anyhow::anyhow!("permission checker failed"))?;
                    permission = Some(check);
                    result?;
                }
                event = conn.incoming.recv() => {
                    ensure!(changes.generation() == generation, "display configuration changed");
                    let message = event.ok_or_else(|| anyhow::anyhow!("host disconnected"))??;
                    match message {
                        Message::Heartbeat(_) => last_rx = Instant::now(),
                        Message::Enter { .. } | Message::ReleaseAll | Message::Move { .. } |
                        Message::Key { .. } | Message::Button { .. } | Message::Scroll { .. } => {
                            if let Some(reply) = input.apply(message)? { conn.send(reply).await?; }
                        }
                        _ => anyhow::bail!("unexpected host message"),
                    }
                }
            }
        }
    }
    .await;
    let released = input.release();
    result.and(released)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::tests::{display, Recording};
    use shared::connection::Connection;

    #[tokio::test]
    async fn slow_permission_check_does_not_pause_mouse_posting() {
        use std::sync::{atomic::AtomicBool, Arc};
        struct Observed {
            recording: Recording,
            moved: Arc<Notify>,
        }
        impl Injector for Observed {
            fn post(&mut self, event: crate::input::Event) -> Result<()> {
                self.recording.post(event)?;
                if matches!(event, crate::input::Event::Motion { dx: -1, .. }) {
                    self.moved.notify_one();
                }
                Ok(())
            }
        }
        let moved = Arc::new(Notify::new());
        let checking = Arc::new(AtomicBool::new(false));
        let worker_checking = checking.clone();
        let (started, mut started_rx) = tokio::sync::mpsc::channel(1);
        let (resume, resume_rx) = std::sync::mpsc::channel();
        let permission = move || {
            worker_checking.store(true, Ordering::Release);
            let _ = started.try_send(());
            // Timeout ensures even a failing test cannot strand a blocking task.
            let _ = resume_rx.recv_timeout(Duration::from_secs(1));
            worker_checking.store(false, Ordering::Release);
            Ok(())
        };
        let (client, _host) = tokio::io::duplex(4096);
        let mut conn = Connection::new(client);
        let (tx, rx) = tokio::sync::mpsc::channel(4);
        conn.incoming = rx;
        let mut input = Input::new(
            Observed {
                recording: Recording::default(),
                moved: moved.clone(),
            },
            display(),
            1.0,
            true,
        );
        input.apply(Message::Enter { epoch: 1, y: 0 }).unwrap();
        let driver = async {
            started_rx.recv().await.unwrap();
            tx.send(Ok(Message::Move { dx: -1, dy: 0 })).await.unwrap();
            let posted = tokio::time::timeout(Duration::from_millis(100), moved.notified()).await;
            let still_checking = checking.load(Ordering::Acquire);
            let _ = resume.send(());
            tx.send(Ok(Message::Ready)).await.unwrap();
            assert!(posted.is_ok(), "mouse stalled behind permission check");
            assert!(
                still_checking,
                "movement only arrived after the blocking check ended"
            );
        };
        let changes = DisplayChanges::default();
        let (result, ()) = tokio::join!(
            run(
                &mut conn,
                &mut input,
                Duration::from_secs(2),
                &changes,
                0,
                permission
            ),
            driver
        );
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn stalled_permission_check_expires_despite_healthy_heartbeats() {
        use std::sync::{atomic::AtomicUsize, Arc};
        let checks = Arc::new(AtomicUsize::new(0));
        let worker_checks = checks.clone();
        let (resume, resume_rx) = std::sync::mpsc::channel();
        let permission = move || {
            worker_checks.fetch_add(1, Ordering::Relaxed);
            let _ = resume_rx.recv_timeout(Duration::from_secs(2));
            Ok(())
        };
        let (client, _host) = tokio::io::duplex(4096);
        let mut conn = Connection::new(client);
        let (tx, rx) = tokio::sync::mpsc::channel(4);
        conn.incoming = rx;
        let mut input = Input::new(Recording::default(), display(), 1.0, true);
        input.apply(Message::Enter { epoch: 1, y: 0 }).unwrap();
        input
            .apply(Message::Button {
                button: 0,
                down: true,
            })
            .unwrap();
        let heartbeats = async {
            loop {
                if tx.send(Ok(Message::Heartbeat(1))).await.is_err() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        };
        let changes = DisplayChanges::default();
        let result = tokio::select! {
            result = run(&mut conn, &mut input, Duration::from_millis(250), &changes, 0, permission) => result,
            _ = heartbeats => panic!("heartbeat sender stopped"),
        };
        let _ = resume.send(());
        assert_eq!(result.unwrap_err().to_string(), "permission check stalled");
        assert_eq!(
            checks.load(Ordering::Relaxed),
            1,
            "checks must not pile up while one is running"
        );
        assert_eq!(input.held_count(), 0);
        assert_eq!(input.epoch, None);
    }

    #[tokio::test]
    async fn queued_movements_post_individually_before_failure_cleanup() {
        let (client, _host) = tokio::io::duplex(4096);
        let mut conn = Connection::new(client);
        let (tx, rx) = tokio::sync::mpsc::channel(64);
        conn.incoming = rx;
        tx.send(Ok(Message::Enter { epoch: 1, y: 0 }))
            .await
            .unwrap();
        tx.send(Ok(Message::Key {
            vk: 16,
            scan: 42,
            extended: false,
            down: true,
        }))
        .await
        .unwrap();
        for _ in 0..40 {
            tx.send(Ok(Message::Move { dx: -1, dy: 0 })).await.unwrap();
        }
        tx.send(Ok(Message::Ready)).await.unwrap();
        let mut input = Input::new(Recording::default(), display(), 1.0, true);
        assert!(run(
            &mut conn,
            &mut input,
            Duration::from_secs(2),
            &DisplayChanges::default(),
            0,
            || Ok(())
        )
        .await
        .is_err());
        let motions: Vec<_> = input
            .injector
            .0
            .iter()
            .filter_map(|e| match e {
                crate::input::Event::Motion { x, dx, .. } => Some((*x, *dx)),
                _ => None,
            })
            .collect();
        assert_eq!(motions.len(), 41);
        assert_eq!(motions[0], (-1.0, 0));
        for (index, &(x, dx)) in motions[1..].iter().enumerate() {
            assert_eq!((x, dx), (-(index as f64) - 2.0, -1));
        }
        assert_eq!(input.held_count(), 0);
        assert_eq!(input.epoch, None);
        assert!(matches!(
            input.injector.0.last(),
            Some(crate::input::Event::Key { down: false, .. })
        ));
    }

    async fn failure(kind: u8) {
        let (client, host) = tokio::io::duplex(4096);
        let mut client = Connection::new(client);
        let mut host = Connection::new(host);
        let changes = DisplayChanges::default();
        let mut input = Input::new(Recording::default(), display(), 1.0, true);
        input.apply(Message::Enter { epoch: 1, y: 0 }).unwrap();
        input
            .apply(Message::Key {
                vk: 65,
                scan: 0x1e,
                extended: false,
                down: true,
            })
            .unwrap();
        input
            .apply(Message::Button {
                button: 0,
                down: true,
            })
            .unwrap();
        let permission = move || {
            ensure!(kind != 2, "permission revoked");
            Ok(())
        };
        match kind {
            0 => drop(host),
            1 => changes.mark(),
            3 => host.send(Message::Ready).await.unwrap(),
            _ => {}
        }
        let result = tokio::time::timeout(
            Duration::from_secs(1),
            run(
                &mut client,
                &mut input,
                Duration::from_millis(250),
                &changes,
                0,
                permission,
            ),
        )
        .await
        .unwrap();
        assert!(result.is_err());
        assert_eq!(input.held_count(), 0);
        assert_eq!(input.epoch, None);
        assert!(matches!(
            input.injector.0.last(),
            Some(crate::input::Event::Button { down: false, .. })
        ));
    }
    #[tokio::test]
    async fn disconnect_display_permission_malformed_and_timeout_release() {
        for kind in 0..5 {
            failure(kind).await;
        }
    }
}
