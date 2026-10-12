use crate::Bounds;
use anyhow::{ensure, Context, Result};
use serde::Deserialize;
use std::{fs, path::Path, time::Duration};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostConfig {
    pub listen: String,
    pub certificate: String,
    pub private_key: String,
    pub pairing_secret: String,
    pub monitor: MonitorConfig,
    #[serde(default = "default_timeout")]
    pub heartbeat_timeout_ms: u64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MonitorConfig {
    pub left: i32,
    pub top: i32,
    pub width: u32,
    pub height: u32,
}
impl MonitorConfig {
    pub fn bounds(&self) -> Bounds {
        Bounds {
            left: self.left,
            top: self.top,
            width: self.width,
            height: self.height,
        }
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClientConfig {
    pub host: String,
    pub certificate_sha256: String,
    pub pairing_secret: String,
    pub screen_width: u32,
    pub screen_height: u32,
    #[serde(default = "default_timeout")]
    pub heartbeat_timeout_ms: u64,
    #[serde(default = "default_retry")]
    pub reconnect_ms: u64,
}
fn default_timeout() -> u64 {
    2000
}
fn default_retry() -> u64 {
    1000
}
pub fn load<T: serde::de::DeserializeOwned>(path: impl AsRef<Path>) -> Result<T> {
    // Do not attach TOML parser errors: they may include source lines containing secrets.
    let text = fs::read_to_string(path).context("could not read configuration")?;
    toml::from_str(&text)
        .map_err(|_| anyhow::anyhow!("invalid configuration; check field names and values"))
}
pub fn secret(value: &str) -> Result<[u8; 32]> {
    hex::decode(value)
        .ok()
        .and_then(|v| v.try_into().ok())
        .context("expected 64 hexadecimal characters")
}
pub fn timeout(ms: u64) -> Result<Duration> {
    ensure!(
        (500..=30000).contains(&ms),
        "heartbeat timeout must be 500..30000 ms"
    );
    Ok(Duration::from_millis(ms))
}
impl HostConfig {
    pub fn validate(&self) -> Result<()> {
        secret(&self.pairing_secret)?;
        timeout(self.heartbeat_timeout_ms)?;
        ensure!(self.monitor.bounds().valid(), "invalid monitor bounds");
        Ok(())
    }
}
impl ClientConfig {
    pub fn bounds(&self) -> Bounds {
        Bounds {
            left: 0,
            top: 0,
            width: self.screen_width,
            height: self.screen_height,
        }
    }
    pub fn validate(&self) -> Result<()> {
        secret(&self.pairing_secret)?;
        secret(&self.certificate_sha256)?;
        timeout(self.heartbeat_timeout_ms)?;
        ensure!(self.bounds().valid(), "invalid screen bounds");
        ensure!(
            (100..=30000).contains(&self.reconnect_ms),
            "reconnect_ms must be 100..30000"
        );
        Ok(())
    }
}
