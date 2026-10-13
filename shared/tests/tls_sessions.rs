use sha2::{Digest, Sha256};
use shared::{
    connection::{self, Connection},
    state::ClientState,
    tls, Bounds, Message,
};
use std::time::Duration;
use tokio::{
    net::{TcpListener, TcpStream},
    time::timeout,
};

fn bounds() -> Bounds {
    Bounds {
        left: 0,
        top: 0,
        width: 1920,
        height: 1080,
    }
}
fn identity() -> (Vec<u8>, Vec<u8>, [u8; 32]) {
    let cert = rcgen::generate_simple_self_signed(vec!["seamlesskvm.local".into()]).unwrap();
    let der = cert.cert.der().to_vec();
    let pin = Sha256::digest(&der).into();
    (der, cert.key_pair.serialize_der(), pin)
}

#[tokio::test]
async fn pinned_tls_ordered_drag_return_and_abrupt_disconnect() {
    timeout(Duration::from_secs(5), async {
        let (cert, key, pin) = identity();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let host = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            tcp.set_nodelay(true).unwrap();
            let mut tls = tokio_rustls::TlsAcceptor::from(tls::server_der(cert, key).unwrap())
                .accept(tcp)
                .await
                .unwrap();
            connection::host_handshake(&mut tls, [7; 32], bounds())
                .await
                .unwrap();
            let mut conn = Connection::new(tls);
            for message in [
                Message::Enter { epoch: 1, y: 32768 },
                Message::Key {
                    vk: 0xa2,
                    scan: 29,
                    extended: false,
                    down: true,
                },
                Message::Button {
                    button: 0,
                    down: true,
                },
                Message::Move { dx: -20, dy: 5 },
                Message::Move { dx: 21, dy: 0 },
            ] {
                conn.send(message).await.unwrap();
            }
            assert!(matches!(
                conn.incoming.recv().await.unwrap().unwrap(),
                Message::Return { epoch: 1, .. }
            ));
            conn.send(Message::Enter { epoch: 2, y: 0 }).await.unwrap();
            conn.send(Message::Button {
                button: 0,
                down: true,
            })
            .await
            .unwrap();
            // Drop TLS without ReleaseAll: the client's failure path must clear state.
        });
        let tcp = TcpStream::connect(addr).await.unwrap();
        tcp.set_nodelay(true).unwrap();
        let mut tls = tokio_rustls::TlsConnector::from(tls::client(pin))
            .connect("seamlesskvm.local".try_into().unwrap(), tcp)
            .await
            .unwrap();
        connection::client_handshake(&mut tls, [7; 32], bounds())
            .await
            .unwrap();
        let mut conn = Connection::new(tls);
        let mut state = ClientState::new(bounds());
        let mut returned = false;
        while let Some(Ok(message)) = conn.incoming.recv().await {
            if let Some(reply) = state.apply(message) {
                assert_eq!(state.held_count(), 0);
                conn.send(reply).await.unwrap();
                returned = true;
            }
        }
        assert!(returned);
        assert_eq!(state.held_count(), 1);
        state.release();
        assert_eq!(state.held_count(), 0);
        host.await.unwrap();
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn wrong_pin_rejected_before_pairing() {
    let (cert, key, _) = identity();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let host = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let _ = tokio_rustls::TlsAcceptor::from(tls::server_der(cert, key).unwrap())
            .accept(tcp)
            .await;
    });
    let tcp = TcpStream::connect(addr).await.unwrap();
    let result = tokio_rustls::TlsConnector::from(tls::client([0; 32]))
        .connect("seamlesskvm.local".try_into().unwrap(), tcp)
        .await;
    assert!(result.is_err());
    host.await.unwrap();
}

#[tokio::test]
async fn wrong_secret_never_becomes_ready() {
    let (mut host, mut client) = tokio::io::duplex(256);
    let task =
        tokio::spawn(async move { connection::host_handshake(&mut host, [1; 32], bounds()).await });
    assert!(connection::client_handshake(&mut client, [2; 32], bounds())
        .await
        .is_err());
    assert!(task.await.unwrap().is_err());
}

#[tokio::test]
async fn cancellation_does_not_discard_partial_frame() {
    use tokio::io::AsyncWriteExt;
    let (host, mut peer) = tokio::io::duplex(256);
    let mut conn = Connection::new(host);
    let mut bytes = [0; 40];
    let len = Message::Heartbeat(99).encode(&mut bytes);
    peer.write_all(&bytes[..9]).await.unwrap();
    assert!(timeout(Duration::from_millis(20), conn.incoming.recv())
        .await
        .is_err());
    peer.write_all(&bytes[9..len]).await.unwrap();
    assert!(conn.incoming.recv().await.unwrap().unwrap() == Message::Heartbeat(99));
}

#[tokio::test]
async fn partial_write_timeout_permanently_disables_writer() {
    let (host, _peer) = tokio::io::duplex(4);
    let mut conn = Connection::new(host);
    let error = conn.send(Message::Heartbeat(9)).await.unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
    // A ReleaseAll after a partial frame would corrupt the remaining stream.
    let error = timeout(Duration::from_millis(20), conn.send(Message::ReleaseAll))
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::NotConnected);
}
