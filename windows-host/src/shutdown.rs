//! The GUI owns the host's stdin pipe. A byte, EOF (including GUI crash), or a
//! read error requests the same graceful shutdown as Ctrl+C. Never read stdin
//! in CLI mode. A dedicated thread avoids an uncancellable Tokio blocking task
//! keeping the runtime alive after a network/startup failure.
use anyhow::Result;
use std::io::Read;

fn watch(mut reader: impl Read, stopped: tokio::sync::oneshot::Sender<()>) {
    let mut byte = [0];
    loop {
        match reader.read(&mut byte) {
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            _ => break,
        }
    }
    let _ = stopped.send(());
}

pub async fn signal(managed: bool) -> Result<()> {
    if !managed {
        tokio::signal::ctrl_c().await?;
        return Ok(());
    }
    let (tx, rx) = tokio::sync::oneshot::channel();
    std::thread::Builder::new()
        .name("gui-lifetime".into())
        .spawn(move || watch(std::io::stdin().lock(), tx))?;
    tokio::select! {
        _ = rx => Ok(()),
        result = tokio::signal::ctrl_c() => { result?; Ok(()) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stop_byte_and_closed_parent_pipe_request_shutdown() {
        for bytes in [b"stop\n".as_slice(), b"".as_slice()] {
            let (tx, mut rx) = tokio::sync::oneshot::channel();
            watch(bytes, tx);
            assert_eq!(rx.try_recv(), Ok(()));
        }
    }

    #[test]
    fn broken_pipe_requests_shutdown() {
        struct Broken;
        impl Read for Broken {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                Err(std::io::ErrorKind::BrokenPipe.into())
            }
        }
        let (tx, mut rx) = tokio::sync::oneshot::channel();
        watch(Broken, tx);
        assert_eq!(rx.try_recv(), Ok(()));
    }
}
