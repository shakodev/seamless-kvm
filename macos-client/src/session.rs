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
