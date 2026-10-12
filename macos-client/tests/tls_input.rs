//! Actual pinned TLS and the native client's session/input engine. Recording
//! replaces only Quartz posting; this does not certify physical Mac input.
use anyhow::Result;
use macos_client::{
    input::{Display, Event, Injector, Input},
    keyboard,
    session::{self, DisplayChanges},
};
use sha2::{Digest, Sha256};
use shared::{
    connection::{self, Connection},
    Bounds, Message,
};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    net::{TcpListener, TcpStream},
    time::timeout,
};

#[derive(Clone, Default)]
struct Recording(Arc<Mutex<Vec<Event>>>, Arc<tokio::sync::Notify>);
impl Injector for Recording {
    fn post(&mut self, event: Event) -> Result<()> {
        self.0.lock().unwrap().push(event);
        if matches!(
            event,
            Event::Button {
                button: 4,
                down: true,
                ..
            }
        ) {
            self.1.notify_one();
        }
        Ok(())
    }
}
fn display() -> Display {
    Display {
        id: 7,
        bounds: Bounds {
            left: -1280,
            top: 200,
            width: 1280,
            height: 720,
        },
        builtin: false,
        main: false,
    }
}

#[tokio::test]
async fn host_protocol_drives_native_engine_and_releases_after_tls_loss() {
    timeout(Duration::from_secs(5), async {
        let cert = rcgen::generate_simple_self_signed(vec!["seamlesskvm.local".into()]).unwrap();
        let pin = Sha256::digest(cert.cert.der().as_ref()).into();
        let acceptor = tokio_rustls::TlsAcceptor::from(shared::tls::server_der(
            cert.cert.der().to_vec(), cert.key_pair.serialize_der()).unwrap());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let recording = Recording::default();
        let observed = recording.1.clone();
        let host = tokio::spawn(async move {
            // Two independent sessions exercise clean admission after TLS loss.
            for epoch in [1, 2] {
                let (tcp, _) = listener.accept().await.unwrap();
                let mut tls = acceptor.accept(tcp).await.unwrap();
                let bounds = connection::host_handshake(&mut tls, [9; 32], Bounds {
                    left: 0, top: 0, width: 3840, height: 2160 }).await.unwrap();
                assert_eq!((bounds.left, bounds.top, bounds.width, bounds.height), (0, 0, 1280, 720));
                let mut conn = Connection::new(tls);
                for message in [
                    Message::Enter { epoch, y: 32768 },
                    Message::Key { vk: 0x5b, scan: 0x5b, extended: true, down: true },
                    Message::Key { vk: 65, scan: 0x1e, extended: false, down: true },
                    Message::Key { vk: 65, scan: 0x1e, extended: false, down: true },
                    Message::Button { button: 0, down: true },
                    Message::Scroll { horizontal: false, delta: 120 },
                    Message::Move { dx: 12, dy: 0 },
                    Message::Move { dx: -13, dy: 0 },
                ] { conn.send(message).await.unwrap(); }
                loop {
                    match conn.incoming.recv().await.unwrap().unwrap() {
                        Message::Return { epoch: returned, y } => {
                            assert_eq!(returned, epoch);
                            assert!(y.abs_diff(32768) < 100); break;
                        }
                        Message::Heartbeat(_) => {},
                        _ => panic!("unexpected client message"),
                    }
                }
                conn.send(Message::ReleaseAll).await.unwrap();
                conn.send(Message::Enter { epoch: epoch + 10, y: 65535 }).await.unwrap();
                conn.send(Message::Button { button: 4, down: true }).await.unwrap();
                // Confirm the native engine has a hold before cutting TLS.
                observed.notified().await;
                // Independent cleanup must work when ReleaseAll is undeliverable.
            }
        });
        for _ in 0..2 {
            let tcp = TcpStream::connect(address).await.unwrap();
            tcp.set_nodelay(true).unwrap();
            let mut tls = tokio_rustls::TlsConnector::from(shared::tls::client(pin))
                .connect("seamlesskvm.local".try_into().unwrap(), tcp).await.unwrap();
            connection::client_handshake(&mut tls, [9; 32], Bounds { left: 0, top: 0, ..display().bounds }).await.unwrap();
            let mut conn = Connection::new(tls);
            let mut input = Input::new(recording.clone(), display(), 1.0, true);
            assert!(session::run(&mut conn, &mut input, Duration::from_secs(2),
                &DisplayChanges::default(), 0, || Ok(())).await.is_err());
            assert_eq!(input.held_count(), 0);
            assert_eq!(input.epoch, None);
        }
        host.await.unwrap();
        let events = recording.0.lock().unwrap();
        assert_eq!(events.iter().filter(|e| matches!(e, Event::Key { code: 0, repeat: true, flags: keyboard::LEFT_COMMAND, .. })).count(), 2);
        assert_eq!(events.iter().filter(|e| matches!(e, Event::Motion { drag: Some(0), .. })).count(), 2);
        for button in [0, 4] {
            assert_eq!(events.iter().filter(|e| matches!(e, Event::Button { button: b, down: true, .. } if *b == button)).count(), 2);
            assert_eq!(events.iter().filter(|e| matches!(e, Event::Button { button: b, down: false, .. } if *b == button)).count(), 2);
        }
        assert_eq!(events.iter().filter(|e| matches!(e, Event::Scroll { points: 30, .. })).count(), 2);
    }).await.unwrap();
}

#[tokio::test]
async fn cancelled_session_drop_releases_held_input() {
    let recording = Recording::default();
    let mut input = Input::new(recording.clone(), display(), 1.0, true);
    input.apply(Message::Enter { epoch: 1, y: 0 }).unwrap();
    input
        .apply(Message::Key {
            vk: 0xa5,
            scan: 0x38,
            extended: true,
            down: true,
        })
        .unwrap();
    input
        .apply(Message::Button {
            button: 2,
            down: true,
        })
        .unwrap();
    let (client, _peer) = tokio::io::duplex(4096);
    let mut conn = Connection::new(client);
    assert!(timeout(
        Duration::from_millis(20),
        session::run(
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
    drop(input);
    let events = recording.0.lock().unwrap();
    assert!(matches!(
        events[events.len() - 2],
        Event::Key {
            code: 61,
            down: false,
            flags: 0,
            ..
        }
    ));
    assert!(matches!(
        events.last(),
        Some(Event::Button {
            button: 2,
            down: false,
            ..
        })
    ));
}
