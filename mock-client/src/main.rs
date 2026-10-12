use anyhow::{ensure, Result};
use shared::{
    config,
    connection::{self, Connection},
    state::ClientState,
    Message,
};
use std::time::{Duration, Instant};
use tokio::{
    net::TcpStream,
    time::{interval, timeout, MissedTickBehavior},
};

#[tokio::main(worker_threads = 2)]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    ensure!(
        (2..=3).contains(&args.len()),
        "usage: mock-client <mock.local.toml> [--auto-return]"
    );
    let auto_return = args.get(2).map(String::as_str) == Some("--auto-return");
    ensure!(args.len() == 2 || auto_return, "unknown option");
    let config: config::ClientConfig = config::load(&args[1])?;
    config.validate()?;
    let connector = tokio_rustls::TlsConnector::from(shared::tls::client(config::secret(
        &config.certificate_sha256,
    )?));
    let secret = config::secret(&config.pairing_secret)?;
    let limit = config::timeout(config.heartbeat_timeout_ms)?;
    let shutdown = tokio::signal::ctrl_c();
    tokio::pin!(shutdown);
    println!("Mock client: simulated input only; move left past x=0 to return");
    loop {
        let connect = async {
            let tcp = TcpStream::connect(&config.host).await?;
            tcp.set_nodelay(true)?;
            let name = rustls::pki_types::ServerName::try_from("seamlesskvm.local")?;
            let mut tls = connector.connect(name, tcp).await?;
            connection::client_handshake(&mut tls, secret, config.bounds()).await?;
            Ok::<_, anyhow::Error>(tls)
        };
        let tls = tokio::select! {r=timeout(connection::HANDSHAKE_TIMEOUT,connect)=>match r{Ok(Ok(s))=>Some(s),_=>{eprintln!("Connection unavailable or authentication rejected; retrying");None}},r=&mut shutdown=>{r?;return Ok(());}};
        if let Some(tls) = tls {
            let mut conn = Connection::new(tls);
            let mut state = ClientState::new(config.bounds());
            let result = tokio::select! {r=simulate(&mut conn,&mut state,limit,auto_return)=>r,r=&mut shutdown=>{r?;state.release();return Ok(());}};
            state.release();
            if result.is_err() {
                eprintln!("Disconnected; simulated held inputs released");
            }
        }
        tokio::select! {_=tokio::time::sleep(Duration::from_millis(config.reconnect_ms))=>{},r=&mut shutdown=>{r?;return Ok(());}}
    }
}

async fn simulate<S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static>(
    conn: &mut Connection<S>,
    state: &mut ClientState,
    limit: Duration,
    auto_return: bool,
) -> Result<()> {
    println!("Connected and ready");
    let mut tick = interval(connection::HEARTBEAT_INTERVAL);
    tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut last_rx = Instant::now();
    let mut entered = None;
    let mut counter = 0u64;
    let mut last_status = Instant::now();
    loop {
        tokio::select! {
            biased;
            _=tick.tick()=>{
                ensure!(last_rx.elapsed()<limit,"heartbeat expired");counter=counter.wrapping_add(1);conn.send(Message::Heartbeat(counter)).await?;
                if auto_return && entered.is_some_and(|t:Instant|t.elapsed()>=Duration::from_secs(2)) {
                    if let Some(epoch)=state.epoch {let y=state.bounds.normalize_y(state.y);state.release();conn.send(Message::Return{epoch,y}).await?;println!("Automatic return to Windows");} entered=None;
                }
                if state.epoch.is_some()&&last_status.elapsed()>=Duration::from_secs(1) {println!("Simulated cursor: {}, {}",state.x,state.y);last_status=Instant::now();}
            }
            event=conn.incoming.recv()=>{
                let m=event.ok_or_else(||anyhow::anyhow!("peer disconnected"))??;
                match m {
                    Message::Heartbeat(_)=>last_rx=Instant::now(),
                    Message::Enter{..}=>{state.apply(m);entered=Some(Instant::now());println!("Control transferred to mock");}
                    Message::ReleaseAll=>{state.release();entered=None;println!("Simulated inputs released");}
                    Message::Move{..}|Message::Key{..}|Message::Button{..}|Message::Scroll{..}=>{
                        if let Some(reply)=state.apply(m){conn.send(reply).await?;entered=None;println!("Returned control to Windows");}
                    }
                    _=>anyhow::bail!("unexpected host message"),
                }
            }
        }
    }
}
