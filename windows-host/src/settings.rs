//! Narrow GUI configuration commands. Only public connection/display fields are
//! printed; TOML diagnostics and credentials must never reach the activity log.
use anyhow::{ensure, Context, Result};
use shared::config::{HostConfig, MonitorConfig};
use std::{fs, net::SocketAddr, path::Path};

fn update(path: &Path, listen: &str, monitor: MonitorConfig) -> Result<()> {
    let _: SocketAddr = listen.parse().context("use an IP address and port")?;
    let mut config: HostConfig = shared::config::load(path)?;
    config.listen = listen.into();
    config.monitor = monitor;
    config.validate()?;
    let source = fs::read_to_string(path)?;
    let mut value: toml::Value =
        toml::from_str(&source).map_err(|_| anyhow::anyhow!("invalid configuration"))?;
    value["listen"] = toml::Value::String(config.listen);
    for (field, number) in [
        ("left", i64::from(config.monitor.left)),
        ("top", i64::from(config.monitor.top)),
        ("width", i64::from(config.monitor.width)),
        ("height", i64::from(config.monitor.height)),
    ] {
        value["monitor"][field] = toml::Value::Integer(number);
    }
    let text = toml::to_string_pretty(&value).context("could not serialize settings")?;
    // Replace through a sibling file: interruption never truncates credentials.
    let pending = path.with_extension("toml.pending");
    use std::io::Write;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&pending)
        .context(
            "could not create settings update; remove a stale .toml.pending file if present",
        )?;
    let result = (|| -> Result<()> {
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        drop(file);
        fs::rename(&pending, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&pending);
    }
    result
}

pub fn command(args: &[String]) -> Result<bool> {
    match args.get(1).map(String::as_str) {
        Some("settings") => {
            ensure!(args.len() == 3, "usage: settings <host configuration>");
            let c: HostConfig = shared::config::load(&args[2])?;
            c.validate()?;
            println!("{}", c.listen);
            println!(
                "left = {}  top = {}  width = {}  height = {}",
                c.monitor.left, c.monitor.top, c.monitor.width, c.monitor.height
            );
        }
        Some("configure") => {
            ensure!(
                args.len() == 8,
                "usage: configure <path> <IP:port> <left> <top> <width> <height>"
            );
            update(
                Path::new(&args[2]),
                &args[3],
                MonitorConfig {
                    left: args[4].parse()?,
                    top: args[5].parse()?,
                    width: args[6].parse()?,
                    height: args[7].parse()?,
                },
            )?;
            println!("Settings saved");
        }
        _ => return Ok(false),
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_preserve_credentials_and_reject_invalid_changes() {
        let dir = std::env::temp_dir().join(format!("seamless-settings-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("host.local.toml");
        let original = format!("listen='127.0.0.1:24800'\ncertificate='cert.der'\nprivate_key='key.der'\npairing_secret='{}'\n[monitor]\nleft=0\ntop=0\nwidth=1920\nheight=1080\n", "ab".repeat(32));
        fs::write(&path, &original).unwrap();
        let monitor = || MonitorConfig {
            left: -2560,
            top: 0,
            width: 2560,
            height: 1440,
        };
        assert!(update(&path, "invalid", monitor()).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), original);
        update(&path, "192.168.1.10:24800", monitor()).unwrap();
        let c: HostConfig = shared::config::load(&path).unwrap();
        assert_eq!(c.pairing_secret, "ab".repeat(32));
        assert_eq!(c.private_key, "key.der");
        assert_eq!(c.certificate, "cert.der");
        assert_eq!(c.monitor.left, -2560);
        assert_eq!(c.listen, "192.168.1.10:24800");
        fs::remove_file(path).unwrap();
        fs::remove_dir(dir).unwrap();
    }
}
