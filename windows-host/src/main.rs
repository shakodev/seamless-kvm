#[cfg(windows)]
mod native;
mod network;
mod shutdown;

use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};
use std::{fs, path::Path};

fn pair(dir: &str) -> Result<()> {
    let path = Path::new(dir);
    fs::create_dir_all(path)?;
    let cert =
        rcgen::generate_simple_self_signed(vec!["seamlesskvm.local".into(), "localhost".into()])?;
    let mut secret = [0u8; 32];
    getrandom::getrandom(&mut secret).map_err(|_| anyhow::anyhow!("random generation failed"))?;
    // create_new prevents accidental replacement of an existing trust anchor.
    use std::io::Write;
    let write_new = |name: &str, data: &[u8]| -> Result<()> {
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path.join(name))?;
        f.write_all(data)?;
        Ok(())
    };
    write_new("host-key.der", &cert.key_pair.serialize_der())?;
    write_new("host-cert.der", cert.cert.der().as_ref())?;
    let pin = hex::encode(Sha256::digest(cert.cert.der().as_ref()));
    let secret = hex::encode(secret);
    let cert_path = path
        .join("host-cert.der")
        .to_string_lossy()
        .replace('\\', "/");
    let key_path = path
        .join("host-key.der")
        .to_string_lossy()
        .replace('\\', "/");
    let host=format!("listen = \"127.0.0.1:24800\"\ncertificate = \"{cert_path}\"\nprivate_key = \"{key_path}\"\npairing_secret = \"{secret}\"\nheartbeat_timeout_ms = 2000\n\n[monitor]\nleft = 0\ntop = 0\nwidth = 1920\nheight = 1080\n");
    let client=format!("host = \"127.0.0.1:24800\"\ncertificate_sha256 = \"{pin}\"\npairing_secret = \"{secret}\"\nscreen_width = 1920\nscreen_height = 1080\nheartbeat_timeout_ms = 2000\nreconnect_ms = 1000\n");
    write_new("host.local.toml", host.as_bytes())?;
    write_new("mock.local.toml", client.as_bytes())?;
    println!("Created pairing files in {dir}. Set host monitor bounds before starting. Keep private key and pairing configurations private.");
    Ok(())
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("pair") {
        return pair(args.get(2).map(String::as_str).unwrap_or("certs"));
    }
    #[cfg(windows)]
    if args.get(1).map(String::as_str) == Some("--watchdog") {
        return native::watchdog(args.get(2).context("missing parent pid")?.parse()?);
    }
    #[cfg(windows)]
    if args.get(1).map(String::as_str) == Some("monitors") {
        for b in native::monitors()? {
            println!(
                "left = {}  top = {}  width = {}  height = {}",
                b.left, b.top, b.width, b.height
            );
        }
        return Ok(());
    }
    #[cfg(windows)]
    if args.get(1).map(String::as_str) == Some("check") {
        let config: shared::config::HostConfig =
            shared::config::load(args.get(2).context("missing host configuration")?)?;
        config.validate()?;
        shared::tls::server(&config.certificate, &config.private_key)?;
        return native::check(config.monitor.bounds());
    }
    let managed = args.get(1).map(String::as_str) == Some("--managed");
    if args.len() != if managed { 3 } else { 2 } {
        bail!("usage: windows-host <host.local.toml> | pair [directory]");
    }
    let config: shared::config::HostConfig =
        shared::config::load(&args[if managed { 2 } else { 1 }])?;
    config.validate()?;
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?
        .block_on(run(config, managed))
}

async fn run(config: shared::config::HostConfig, managed: bool) -> Result<()> {
    #[cfg(windows)]
    {
        let bridge = std::sync::Arc::new(network::Bridge::new());
        let (tx, rx) = tokio::sync::mpsc::channel(1024);
        let (commands, command_rx) = std::sync::mpsc::sync_channel(16);
        let input = native::start(config.monitor.bounds(), bridge.clone(), tx, command_rx)?;
        let result = network::serve(
            config,
            bridge.clone(),
            rx,
            commands,
            shutdown::signal(managed),
        )
        .await;
        bridge
            .stop
            .store(true, std::sync::atomic::Ordering::Release);
        bridge
            .session
            .store(0, std::sync::atomic::Ordering::Release);
        input
            .join()
            .map_err(|_| anyhow::anyhow!("input thread failed"))?;
        result
    }
    #[cfg(not(windows))]
    {
        let _ = (config, managed);
        bail!("Windows host requires Windows 11");
    }
}
