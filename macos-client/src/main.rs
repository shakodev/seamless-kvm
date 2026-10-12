fn main() -> anyhow::Result<()> {
    #[cfg(target_os = "macos")]
    {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?
            .block_on(macos_client::app::run())
    }
    #[cfg(not(target_os = "macos"))]
    anyhow::bail!("macos-client requires macOS; use mock-client for simulated input")
}
