use crate::{
    config::Config,
    input::{select_display, Input},
    native::{self, DisplayWatcher, Quartz},
    session::{self, DisplayChanges},
};
use anyhow::{ensure, Result};
use shared::{
    connection::{self, Connection},
    Bounds,
};
use std::{sync::Arc, time::Duration};
use tokio::{net::TcpStream, time::timeout};

async fn shutdown() -> Result<()> {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut terminate = signal(SignalKind::terminate())?;
        let mut hangup = signal(SignalKind::hangup())?;
        tokio::select! { r = tokio::signal::ctrl_c() => r?, _ = terminate.recv() => {}, _ = hangup.recv() => {} }
    }
    #[cfg(not(unix))]
    tokio::signal::ctrl_c().await?;
    Ok(())
}

pub async fn run() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("displays") {
        for d in native::displays()? {
            println!(
                "id={} main={} builtin={} origin=({}, {}) size={}x{} Quartz points",
                d.id,
                d.main,
                d.builtin,
                d.bounds.left,
                d.bounds.top,
                d.bounds.width,
                d.bounds.height
            );
        }
        return Ok(());
    }
    let check = args.get(1).map(String::as_str) == Some("check");
    ensure!(
        (!check && args.len() == 2) || (check && args.len() == 3),
        "usage: macos-client <macos.local.toml> | displays | check <macos.local.toml>"
    );
    let config: Config = shared::config::load(&args[if check { 2 } else { 1 }])?;
    config.validate()?;
    native::trusted(true)?;
    let changes = Arc::new(DisplayChanges::default());
    let _watcher = DisplayWatcher::start(changes.clone())?;
    if check {
        let display = select_display(&native::displays()?, config.display_id)
            .ok_or_else(|| anyhow::anyhow!("no usable main display"))?;
        let _source = Quartz::new()?;
        println!("Accessibility and Quartz startup passed; selected display {} ({}x{} points). No input injected.", display.id, display.bounds.width, display.bounds.height);
        return Ok(());
    }
    let connector = tokio_rustls::TlsConnector::from(shared::tls::client(shared::config::secret(
        &config.certificate_sha256,
    )?));
    let secret = shared::config::secret(&config.pairing_secret)?;
    let shutdown = shutdown();
    tokio::pin!(shutdown);
    loop {
        native::trusted(false)?;
        let generation = changes.generation();
        let selected = native::displays().and_then(|displays| {
            select_display(&displays, config.display_id)
                .ok_or_else(|| anyhow::anyhow!("no usable main display"))
        });
        if let Ok(display) = selected {
            // Native injection must be available before the client sends Ready.
            let mut input = Input::new(
                Quartz::new()?,
                display,
                config.pointer_speed,
                config.win_to_command,
            );
            let connect = async {
                let tcp = TcpStream::connect(&config.host).await?;
                tcp.set_nodelay(true)?;
                let mut tls = connector
                    .connect("seamlesskvm.local".try_into()?, tcp)
                    .await?;
                native::trusted(false)?;
                ensure!(
                    changes.generation() == generation,
                    "display configuration changed"
                );
                let bounds = Bounds {
                    left: 0,
                    top: 0,
                    ..display.bounds
                };
                connection::client_handshake(&mut tls, secret, bounds).await?;
                Ok::<_, anyhow::Error>(tls)
            };
            let tls = tokio::select! {
                r = timeout(connection::HANDSHAKE_TIMEOUT, connect) => match r {
                    Ok(Ok(tls)) => Some(tls),
                    _ => { eprintln!("Host unavailable, authentication rejected, or display changed; retrying"); None }
                },
                r = &mut shutdown => { r?; return Ok(()); }
            };
            if let Some(tls) = tls {
                let mut conn = Connection::new(tls);
                println!("Connected and ready on display {}", display.id);
                let result = tokio::select! {
                    r = session::run(&mut conn, &mut input, config.limit(), &changes, generation, || native::trusted(false)) => r,
                    r = &mut shutdown => { input.release()?; r?; return Ok(()); }
                };
                input.release()?;
                // Explicitly close the session before waiting/re-enumerating.
                drop(conn);
                if result.is_err() {
                    eprintln!("Session ended; held input released; reconnecting");
                }
            }
        } else {
            eprintln!("No usable display; retrying");
        }
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_millis(config.reconnect_ms)) => {},
            r = &mut shutdown => { r?; return Ok(()); }
        }
    }
}
