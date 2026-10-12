use anyhow::{ensure, Result};
use serde::Deserialize;
use std::time::Duration;

/// Accept the generated Stage 1 pairing file, but discover native bounds.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub host: String,
    pub certificate_sha256: String,
    pub pairing_secret: String,
    pub screen_width: Option<u32>,
    pub screen_height: Option<u32>,
    pub display_id: Option<u32>,
    #[serde(default = "yes")]
    pub win_to_command: bool,
    #[serde(default = "speed")]
    pub pointer_speed: f64,
    #[serde(default = "timeout")]
    pub heartbeat_timeout_ms: u64,
    #[serde(default = "retry")]
    pub reconnect_ms: u64,
}
fn yes() -> bool {
    true
}
fn speed() -> f64 {
    1.0
}
fn timeout() -> u64 {
    2000
}
fn retry() -> u64 {
    1000
}
impl Config {
    pub fn validate(&self) -> Result<()> {
        shared::config::secret(&self.certificate_sha256)?;
        shared::config::secret(&self.pairing_secret)?;
        shared::config::timeout(self.heartbeat_timeout_ms)?;
        ensure!(!self.host.is_empty(), "host is required");
        ensure!(
            (100..=30000).contains(&self.reconnect_ms),
            "reconnect_ms must be 100..30000"
        );
        ensure!(
            self.pointer_speed.is_finite() && (0.05..=20.0).contains(&self.pointer_speed),
            "pointer_speed must be 0.05..20 logical points per mouse count"
        );
        Ok(())
    }
    pub fn limit(&self) -> Duration {
        Duration::from_millis(self.heartbeat_timeout_ms)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn accepts_pairing_file_and_rejects_unsafe_options() {
        let text = format!("host='127.0.0.1:24800'\ncertificate_sha256='{}'\npairing_secret='{}'\nscreen_width=1920\nscreen_height=1080", "00".repeat(32), "01".repeat(32));
        let mut c: Config = toml::from_str(&text).unwrap();
        c.validate().unwrap();
        assert!(c.win_to_command);
        c.pointer_speed = f64::NAN;
        assert!(c.validate().is_err());
        assert!(toml::from_str::<Config>(&format!("{text}\nunknown=true")).is_err());
    }
}
