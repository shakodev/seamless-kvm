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
    // A GUI-launched host has no console. Its pipe is the shutdown authority;
    // do not require console-control handler registration in this mode.
    let _ = rx.await;
    Ok(())
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

    #[test]
    fn managed_child() {
        if std::env::var_os("SEAMLESS_TEST_MANAGED_CHILD").is_some() {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(signal(true))
                .unwrap();
        }
    }

    #[cfg(windows)]
    #[test]
    fn detached_process_stops_when_parent_pipe_closes() {
        use std::{
            os::windows::process::CommandExt,
            process::{Command, Stdio},
            time::{Duration, Instant},
        };
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "shutdown::tests::managed_child"])
            .env("SEAMLESS_TEST_MANAGED_CHILD", "1")
            .creation_flags(0x08000000) // CREATE_NO_WINDOW, matching the GUI.
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        drop(child.stdin.take());
        let started = Instant::now();
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                assert!(status.success());
                break;
            }
            if started.elapsed() > Duration::from_secs(5) {
                let _ = child.kill();
                let _ = child.wait();
                panic!("detached host did not stop on EOF");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
