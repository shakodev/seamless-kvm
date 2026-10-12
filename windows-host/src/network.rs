use anyhow::{ensure, Result};
use shared::{
    config,
    connection::{self, Connection},
    Message,
};
use std::{
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::SyncSender,
        Arc,
    },
    time::{Duration, Instant},
};
use tokio::{
    net::TcpListener,
    sync::mpsc,
    time::{interval, timeout, MissedTickBehavior},
};

pub struct Bridge {
    pub session: AtomicU64,
    pub worker_tick: AtomicU64,
    pub stop: AtomicBool,
    origin: Instant,
}
impl Bridge {
    pub fn new() -> Self {
        Self {
            session: AtomicU64::new(0),
            worker_tick: AtomicU64::new(0),
            stop: AtomicBool::new(false),
            origin: Instant::now(),
        }
    }
    pub fn now(&self) -> u64 {
        self.origin.elapsed().as_millis() as u64
    }
    pub fn healthy(&self) -> bool {
        self.session.load(Ordering::Acquire) != 0
            && self
                .now()
                .saturating_sub(self.worker_tick.load(Ordering::Acquire))
                < 750
            && !self.stop.load(Ordering::Acquire)
    }
}
pub struct InputPacket {
    pub session: u64,
    pub message: Message,
}
pub enum Command {
    Return { session: u64, epoch: u64, y: u16 },
}
struct SessionGuard(Arc<Bridge>);
impl Drop for SessionGuard {
    fn drop(&mut self) {
        self.0.session.store(0, Ordering::Release);
    }
}

pub async fn serve(
    config: config::HostConfig,
    bridge: Arc<Bridge>,
    mut input: mpsc::Receiver<InputPacket>,
    commands: SyncSender<Command>,
) -> Result<()> {
    let acceptor = tokio_rustls::TlsAcceptor::from(shared::tls::server(
        &config.certificate,
        &config.private_key,
    )?);
    let listener = TcpListener::bind(&config.listen).await?;
    let secret = config::secret(&config.pairing_secret)?;
    let limit = config::timeout(config.heartbeat_timeout_ms)?;
    println!(
        "Host listening on {}; emergency: Ctrl+Alt+Shift+F12",
        config.listen
    );
    let shutdown = tokio::signal::ctrl_c();
    tokio::pin!(shutdown);
    let mut generation = 0u64;
    loop {
        if bridge.stop.load(Ordering::Acquire) {
            return Ok(());
        }
        let accepted = tokio::select! {
            r=listener.accept()=>r?,
            r=&mut shutdown=>{r?;return Ok(());},
            _=tokio::time::sleep(Duration::from_millis(100))=>continue,
        };
        let (socket, _) = accepted;
        socket.set_nodelay(true)?;
        // One admission at a time, including authentication. Extra connections are
        // dropped while the established session is running, never replacing it.
        let handshake = async {
            let mut tls = acceptor.accept(socket).await?;
            connection::host_handshake(&mut tls, secret, config.monitor.bounds()).await?;
            Ok::<_, anyhow::Error>(tls)
        };
        let tls = tokio::select! {r=timeout(connection::HANDSHAKE_TIMEOUT,handshake)=>match r {Ok(Ok(s))=>s,_=>{eprintln!("Connection authentication failed or timed out");continue;}},r=&mut shutdown=>{r?;return Ok(());}};
        if bridge.stop.load(Ordering::Acquire) {
            return Ok(());
        }
        while input.try_recv().is_ok() {}
        generation = generation.wrapping_add(1).max(1);
        bridge.worker_tick.store(bridge.now(), Ordering::Release);
        bridge.session.store(generation, Ordering::Release);
        let guard = SessionGuard(bridge.clone());
        println!("Authenticated client ready");
        let mut conn = Connection::new(tls);
        let session = run_session(
            &listener, &mut conn, &bridge, generation, &mut input, &commands, limit,
        );
        let result = tokio::select! {r=session=>r,r=&mut shutdown=>{r?;drop(guard);let _=conn.send(Message::ReleaseAll).await;return Ok(());}};
        drop(guard);
        let _ = conn.send(Message::ReleaseAll).await;
        if result.is_err() {
            eprintln!("Session ended; local control restored");
        }
    }
}

async fn run_session<S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static>(
    listener: &TcpListener,
    conn: &mut Connection<S>,
    bridge: &Bridge,
    generation: u64,
    input: &mut mpsc::Receiver<InputPacket>,
    commands: &SyncSender<Command>,
    limit: Duration,
) -> Result<()> {
    let mut timer = interval(Duration::from_millis(100));
    timer.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut last_rx = Instant::now();
    let mut last_heartbeat = Instant::now();
    let mut serial = 0u64;
    loop {
        tokio::select! {
            biased;
            _=timer.tick()=>{
                ensure!(!bridge.stop.load(Ordering::Acquire)&&bridge.session.load(Ordering::Acquire)==generation,"input worker revoked session");
                ensure!(last_rx.elapsed()<limit,"heartbeat expired");
                bridge.worker_tick.store(bridge.now(),Ordering::Release);
                if last_heartbeat.elapsed()>=connection::HEARTBEAT_INTERVAL {serial=serial.wrapping_add(1);conn.send(Message::Heartbeat(serial)).await?;last_heartbeat=Instant::now();}
            }
            event=conn.incoming.recv()=>{
                let m=event.ok_or_else(||anyhow::anyhow!("peer disconnected"))??;
                match m {
                    Message::Heartbeat(_)=>last_rx=Instant::now(),
                    Message::Return{epoch,y}=>{commands.try_send(Command::Return{session:generation,epoch,y}).map_err(|_|anyhow::anyhow!("command queue full"))?;}
                    _=>anyhow::bail!("unexpected client message"),
                }
            }
            packet=input.recv()=>{
                let p=packet.ok_or_else(||anyhow::anyhow!("input worker stopped"))?;
                if p.session==generation {conn.send(p.message).await?;}
            }
            extra=listener.accept()=>{if let Ok((s,_))=extra{drop(s);}}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn ready_bridge() -> Arc<Bridge> {
        let b = Arc::new(Bridge::new());
        b.session.store(1, Ordering::Release);
        b
    }
    #[test]
    fn ready_lease_and_guard_fail_closed() {
        let b = Arc::new(Bridge::new());
        assert!(!b.healthy());
        b.session.store(1, Ordering::Release);
        assert!(b.healthy());
        {
            let _guard = SessionGuard(b.clone());
        }
        assert!(!b.healthy());
        b.session.store(2, Ordering::Release);
        b.stop.store(true, Ordering::Release);
        assert!(!b.healthy());
    }
    #[tokio::test]
    async fn abrupt_disconnect_revokes_session() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let (host, peer) = tokio::io::duplex(1024);
        let mut conn = Connection::new(host);
        let b = ready_bridge();
        let guard = SessionGuard(b.clone());
        let (_tx, mut rx) = mpsc::channel(16);
        let (commands, _) = std::sync::mpsc::sync_channel(16);
        drop(peer);
        assert!(timeout(
            Duration::from_secs(1),
            run_session(
                &listener,
                &mut conn,
                &b,
                1,
                &mut rx,
                &commands,
                Duration::from_secs(2)
            )
        )
        .await
        .unwrap()
        .is_err());
        drop(guard);
        assert!(!b.healthy());
    }
    #[tokio::test]
    async fn silent_peer_expires_and_extra_client_cannot_replace_it() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let extra = tokio::net::TcpStream::connect(listener.local_addr().unwrap())
            .await
            .unwrap();
        let (host, _peer) = tokio::io::duplex(4096);
        let mut conn = Connection::new(host);
        let b = ready_bridge();
        let guard = SessionGuard(b.clone());
        let (_tx, mut rx) = mpsc::channel(16);
        let (commands, _) = std::sync::mpsc::sync_channel(16);
        assert!(timeout(
            Duration::from_secs(2),
            run_session(
                &listener,
                &mut conn,
                &b,
                1,
                &mut rx,
                &commands,
                Duration::from_millis(150)
            )
        )
        .await
        .unwrap()
        .is_err());
        let mut byte = [0];
        use tokio::io::AsyncReadExt;
        let mut extra = extra;
        assert_eq!(extra.read(&mut byte).await.unwrap(), 0);
        assert_eq!(b.session.load(Ordering::Acquire), 1);
        drop(guard);
        assert!(!b.healthy());
    }
    #[tokio::test]
    async fn queue_revocation_and_return_routing() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let (host, peer) = tokio::io::duplex(4096);
        let mut conn = Connection::new(host);
        let mut peer = Connection::new(peer);
        let b = ready_bridge();
        let (tx, mut rx) = mpsc::channel(16);
        let (commands, command_rx) = std::sync::mpsc::sync_channel(16);
        tx.send(InputPacket {
            session: 0,
            message: Message::ReleaseAll,
        })
        .await
        .unwrap();
        tx.send(InputPacket {
            session: 1,
            message: Message::Move { dx: 3, dy: 4 },
        })
        .await
        .unwrap();
        peer.send(Message::Return { epoch: 8, y: 4000 })
            .await
            .unwrap();
        let work = run_session(
            &listener,
            &mut conn,
            &b,
            1,
            &mut rx,
            &commands,
            Duration::from_secs(2),
        );
        let revoke = async {
            assert!(peer.incoming.recv().await.unwrap().unwrap() == Message::Move { dx: 3, dy: 4 });
            let command = timeout(Duration::from_secs(1), async {
                loop {
                    if let Ok(command) = command_rx.try_recv() {
                        break command;
                    }
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
            })
            .await
            .unwrap();
            assert!(matches!(
                command,
                Command::Return {
                    session: 1,
                    epoch: 8,
                    y: 4000
                }
            ));
            b.session.store(0, Ordering::Release);
        };
        let (result, ()) = tokio::join!(work, revoke);
        assert!(result.is_err());
    }
}
