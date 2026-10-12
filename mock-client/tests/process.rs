//! Real mock executable against an in-process TLS host; never installs hooks.
use sha2::{Digest, Sha256};
use shared::{
    connection::{self, Connection},
    Bounds, Message,
};
use std::{
    process::{Child, Command, Stdio},
    time::Duration,
};
use tokio::{net::TcpListener, time::timeout};

struct Mock(Child);
impl Drop for Mock {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn bounds() -> Bounds {
    Bounds {
        left: 0,
        top: 0,
        width: 1920,
        height: 1080,
    }
}

#[tokio::test]
async fn executable_auto_returns_and_reconnects_after_host_disconnect() {
    timeout(Duration::from_secs(12),async {
        let cert=rcgen::generate_simple_self_signed(vec!["seamlesskvm.local".into()]).unwrap();
        let pin=hex::encode(Sha256::digest(cert.cert.der().as_ref()));
        let acceptor=tokio_rustls::TlsAcceptor::from(shared::tls::server_der(cert.cert.der().to_vec(),cert.key_pair.serialize_der()).unwrap());
        let listener=TcpListener::bind("127.0.0.1:0").await.unwrap();
        let dir=tempfile::tempdir().unwrap();let config=dir.path().join("mock.local.toml");
        std::fs::write(&config,format!("host = \"{}\"\ncertificate_sha256 = \"{pin}\"\npairing_secret = \"{}\"\nscreen_width = 1280\nscreen_height = 720\nheartbeat_timeout_ms = 1000\nreconnect_ms = 100\n",listener.local_addr().unwrap(),hex::encode([7;32]))).unwrap();
        let mut mock=Mock(Command::new(env!("CARGO_BIN_EXE_mock-client")).arg(config).arg("--auto-return").stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap());
        for epoch in 1..=2 {
            let(tcp,_)=listener.accept().await.unwrap();let mut tls=acceptor.accept(tcp).await.unwrap();
            let client=connection::host_handshake(&mut tls,[7;32],bounds()).await.unwrap();assert_eq!(client.width,1280);
            let mut conn=Connection::new(tls);conn.send(Message::Enter{epoch,y:32768}).await.unwrap();
            conn.send(Message::Key{vk:0xa2,scan:29,extended:false,down:true}).await.unwrap();conn.send(Message::Button{button:0,down:true}).await.unwrap();
            conn.send(Message::Move{dx:20,dy:0}).await.unwrap();
            let mut ticks=tokio::time::interval(connection::HEARTBEAT_INTERVAL);
            loop {
                tokio::select!{
                    _=ticks.tick()=>conn.send(Message::Heartbeat(1)).await.unwrap(),
                    event=conn.incoming.recv()=>match event.unwrap().unwrap(){
                        Message::Return{epoch:returned,y}=>{assert_eq!(returned,epoch);assert!(y.abs_diff(32768)<100);break;}
                        Message::Heartbeat(_)=>{}
                        _=>panic!("unexpected client message"),
                    }
                }
            }
            // Leave held state in a fresh epoch, then abruptly close the host.
            conn.send(Message::Enter{epoch:epoch+100,y:0}).await.unwrap();conn.send(Message::Button{button:1,down:true}).await.unwrap();drop(conn);
        }
        assert!(mock.0.try_wait().unwrap().is_none());
    }).await.unwrap();
}
