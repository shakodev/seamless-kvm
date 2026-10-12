fn main() -> anyhow::Result<()> {
    #[cfg(target_os = "macos")]
    {
        let changes = std::sync::Arc::new(macos_client::session::DisplayChanges::default());
        let worker_changes = changes.clone();
        macos_client::native::run_main_loop(changes, move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?
                .block_on(macos_client::app::run(worker_changes))
        })
    }
    #[cfg(not(target_os = "macos"))]
    anyhow::bail!("macos-client requires macOS; use mock-client for simulated input")
}
