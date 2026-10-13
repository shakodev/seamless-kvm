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
    mut permission: F,
) -> Result<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
    I: Injector,
    F: FnMut() -> Result<()>,
{
    let result: Result<()> = async {
        let mut tick = interval(connection::HEARTBEAT_INTERVAL);
        tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
        let mut last_rx = Instant::now();
        let mut serial = 0u64;
        loop {
            tokio::select! {
                biased;
                _ = tick.tick() => {
                    ensure!(last_rx.elapsed() < limit, "host heartbeat expired");
                    ensure!(changes.generation() == generation, "display configuration changed");
                    permission()?;
                    serial = serial.wrapping_add(1);
                    conn.send(Message::Heartbeat(serial)).await?;
                }
                _ = changes.notify.notified() => {
                    ensure!(changes.generation() == generation, "display configuration changed");
                }
                event = conn.incoming.recv() => {
                    let mut event = event;
                    for index in 0..connection::MAX_BATCH_MESSAGES {
                        ensure!(changes.generation() == generation, "display configuration changed");
                        let message = event.ok_or_else(|| anyhow::anyhow!("host disconnected"))??;
                        match message {
                            Message::Heartbeat(_) => {
                                input.flush_motion()?;
                                last_rx = Instant::now();
                            }
                            Message::Enter { .. } | Message::ReleaseAll | Message::Move { .. } |
                            Message::Key { .. } | Message::Button { .. } | Message::Scroll { .. } => {
                                if let Some(reply) = input.apply_queued(message)? { conn.send(reply).await?; }
                            }
                            _ => anyhow::bail!("unexpected host message"),
                        }
                        if index + 1 == connection::MAX_BATCH_MESSAGES {
                            break;
                        }
                        // No wait: consume only messages already available. Each
                        // delta still clamps/tests edges before hover coalescing.
                        match conn.incoming.try_recv() {
                            Ok(next) => event = Some(next),
                            Err(_) => break,
                        }
                    }
                    input.flush_motion()?;
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
    async fn queued_bursts_flush_at_budget_and_release_on_protocol_error() {
        let (client, _host) = tokio::io::duplex(4096);
        let mut conn = Connection::new(client);
        // Preload the reader's delivery queue to model a Wi-Fi burst exactly.
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
        tx.send(Ok(Message::Ready)).await.unwrap(); // invalid during a session
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
        // Entry, the first bounded burst, and the tail before failure cleanup.
        assert_eq!(motions, [(-1.0, 0), (-31.0, -30), (-41.0, -10)]);
        assert_eq!(input.held_count(), 0);
        assert_eq!(input.epoch, None);
        assert!(matches!(
            input.injector.0.last(),
            Some(crate::input::Event::Key { down: false, .. })
        ));
    }

    #[tokio::test]
    async fn a_single_movement_does_not_wait_for_the_next_report() {
        let (client, _host) = tokio::io::duplex(4096);
        let mut conn = Connection::new(client);
        let (tx, rx) = tokio::sync::mpsc::channel(4);
        conn.incoming = rx;
        tx.send(Ok(Message::Enter { epoch: 1, y: 0 }))
            .await
            .unwrap();
        tx.send(Ok(Message::Move { dx: -1, dy: 0 })).await.unwrap();
        let mut input = Input::new(Recording::default(), display(), 1.0, true);
        // Keep the sender connected but never deliver a second movement.
        assert!(tokio::time::timeout(
            Duration::from_millis(50),
            run(
                &mut conn,
                &mut input,
                Duration::from_secs(2),
                &DisplayChanges::default(),
                0,
                || Ok(())
            )
        )
        .await
        .is_err());
        assert_eq!(input.injector.0.len(), 2);
        assert!(matches!(
            input.injector.0.last(),
            Some(crate::input::Event::Motion {
                x: -2.0,
                dx: -1,
                ..
            })
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
        let permission = || {
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
