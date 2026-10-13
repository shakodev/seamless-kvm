use crate::{
    protocol::{HEADER_LEN, MAX_PAYLOAD},
    read_message, write_message, Bounds, Message,
};
use std::{io, time::Duration};
use subtle::ConstantTimeEq;
use tokio::{
    io::{AsyncRead, AsyncWrite, AsyncWriteExt, BufReader, WriteHalf},
    sync::mpsc,
    task::JoinHandle,
    time::timeout,
};

pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(3);
pub const WRITE_TIMEOUT: Duration = Duration::from_millis(500);
pub const HEARTBEAT_INTERVAL: Duration = Duration::from_millis(200);
/// Bound work per wakeup so input floods cannot starve safety checks. Even the
/// largest frames fit in 1,280 plaintext bytes; no timer waits to fill a batch.
pub const MAX_BATCH_MESSAGES: usize = 32;

fn bad() -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, "handshake rejected")
}
pub async fn host_handshake<S: AsyncRead + AsyncWrite + Unpin>(
    s: &mut S,
    secret: [u8; 32],
    bounds: Bounds,
) -> io::Result<Bounds> {
    match read_message(s).await? {
        Message::Hello(actual) if bool::from(actual.ct_eq(&secret)) => {}
        _ => return Err(bad()),
    }
    write_message(s, Message::Welcome).await?;
    write_message(s, Message::Screen(bounds)).await?;
    let client = match read_message(s).await? {
        Message::Screen(b) => b,
        _ => return Err(bad()),
    };
    if read_message(s).await? != Message::Ready {
        return Err(bad());
    }
    write_message(s, Message::Ready).await?;
    Ok(client)
}
pub async fn client_handshake<S: AsyncRead + AsyncWrite + Unpin>(
    s: &mut S,
    secret: [u8; 32],
    bounds: Bounds,
) -> io::Result<Bounds> {
    write_message(s, Message::Hello(secret)).await?;
    if read_message(s).await? != Message::Welcome {
        return Err(bad());
    }
    let host = match read_message(s).await? {
        Message::Screen(b) => b,
        _ => return Err(bad()),
    };
    write_message(s, Message::Screen(bounds)).await?;
    write_message(s, Message::Ready).await?;
    if read_message(s).await? != Message::Ready {
        return Err(bad());
    }
    Ok(host)
}

/// Owns a persistent reader so select! never cancels a partial TCP frame.
pub struct Connection<S> {
    pub incoming: mpsc::Receiver<io::Result<Message>>,
    writer: WriteHalf<S>,
    reader: JoinHandle<()>,
    write_failed: bool,
}
impl<S: AsyncRead + AsyncWrite + Unpin + Send + 'static> Connection<S> {
    pub fn new(stream: S) -> Self {
        let (read, writer) = tokio::io::split(stream);
        // Read ahead only when bytes are available. Framing remains in this
        // persistent task, including buffered bytes across select cancellation.
        let mut read = BufReader::with_capacity(4096, read);
        let (tx, incoming) = mpsc::channel(256);
        let reader = tokio::spawn(async move {
            loop {
                let m = read_message(&mut read).await;
                let done = m.is_err();
                if tx.send(m).await.is_err() || done {
                    break;
                }
            }
        });
        Self {
            incoming,
            writer,
            reader,
            write_failed: false,
        }
    }
    pub async fn send(&mut self, m: Message) -> io::Result<()> {
        self.send_batch(std::slice::from_ref(&m)).await
    }
    /// Send already-available messages in one TLS write, in exact wire order.
    /// Callers must not wait to assemble a batch or combine relative deltas:
    /// per-sample clamping and edge crossings are significant on the receiver.
    pub async fn send_batch(&mut self, messages: &[Message]) -> io::Result<()> {
        if self.write_failed {
            return Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "writer already failed",
            ));
        }
        if messages.len() > MAX_BATCH_MESSAGES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "batch too large",
            ));
        }
        let mut bytes = [0; MAX_BATCH_MESSAGES * (HEADER_LEN + MAX_PAYLOAD)];
        let mut len = 0;
        for &message in messages {
            let frame = (&mut bytes[len..len + HEADER_LEN + MAX_PAYLOAD])
                .try_into()
                .unwrap();
            len += message.encode(frame);
        }
        // rustls can accept plaintext while encrypted bytes remain buffered.
        // Flush before reporting success, under the same deadline as the write.
        // Mark unusable before awaiting so cancellation cannot append a release
        // to a partially written frame or an unflushed TLS record.
        self.write_failed = true;
        let result = timeout(WRITE_TIMEOUT, async {
            self.writer.write_all(&bytes[..len]).await?;
            self.writer.flush().await
        })
        .await
        .unwrap_or_else(|_| Err(io::Error::new(io::ErrorKind::TimedOut, "write timeout")));
        if result.is_ok() {
            self.write_failed = false;
        }
        result
    }
}
impl<S> Drop for Connection<S> {
    fn drop(&mut self) {
        self.reader.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        pin::Pin,
        sync::{Arc, Mutex},
        task::{Context, Poll},
    };
    use tokio::io::ReadBuf;

    #[derive(Default)]
    struct Writes {
        bytes: Vec<u8>,
        calls: usize,
        flushes: usize,
        block_flush: bool,
        fail_flush: bool,
    }
    struct RecordingStream(Arc<Mutex<Writes>>);
    impl AsyncRead for RecordingStream {
        fn poll_read(
            self: Pin<&mut Self>,
            _: &mut Context<'_>,
            _: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            Poll::Pending
        }
    }
    impl AsyncWrite for RecordingStream {
        fn poll_write(
            self: Pin<&mut Self>,
            _: &mut Context<'_>,
            bytes: &[u8],
        ) -> Poll<io::Result<usize>> {
            let mut writes = self.0.lock().unwrap();
            writes.bytes.extend_from_slice(bytes);
            writes.calls += 1;
            Poll::Ready(Ok(bytes.len()))
        }
        fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
            let mut writes = self.0.lock().unwrap();
            writes.flushes += 1;
            if writes.block_flush {
                Poll::Pending
            } else if writes.fail_flush {
                Poll::Ready(Err(io::ErrorKind::BrokenPipe.into()))
            } else {
                Poll::Ready(Ok(()))
            }
        }
        fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    #[tokio::test]
    async fn batch_uses_one_write_without_changing_frames() {
        let writes = Arc::new(Mutex::new(Writes::default()));
        let mut conn = Connection::new(RecordingStream(writes.clone()));
        let mut messages = [Message::Move { dx: -1, dy: 2 }; MAX_BATCH_MESSAGES];
        messages[0] = Message::Enter { epoch: 7, y: 1234 };
        messages[5] = Message::Button {
            button: 0,
            down: true,
        };
        messages[8] = Message::Key {
            vk: 16,
            scan: 42,
            extended: false,
            down: true,
        };
        messages[16] = Message::Scroll {
            horizontal: false,
            delta: 120,
        };
        messages[31] = Message::ReleaseAll;
        for &message in &messages {
            conn.send(message).await.unwrap();
        }
        let single = std::mem::take(&mut *writes.lock().unwrap());
        assert_eq!(single.calls, MAX_BATCH_MESSAGES);
        assert_eq!(single.flushes, MAX_BATCH_MESSAGES);
        conn.send_batch(&messages).await.unwrap();
        let batched = std::mem::take(&mut *writes.lock().unwrap());
        assert_eq!(batched.calls, 1);
        assert_eq!(batched.flushes, 1);
        assert_eq!(batched.bytes, single.bytes);
        let mut bytes = batched.bytes.as_slice();
        for message in messages {
            assert!(read_message(&mut bytes).await.unwrap() == message);
        }
        assert!(bytes.is_empty());
    }

    #[tokio::test]
    async fn invalid_batch_does_not_write_or_poison_connection() {
        let writes = Arc::new(Mutex::new(Writes::default()));
        let mut conn = Connection::new(RecordingStream(writes.clone()));
        let error = conn
            .send_batch(&[Message::ReleaseAll; MAX_BATCH_MESSAGES + 1])
            .await
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        conn.send_batch(&[]).await.unwrap();
        assert_eq!(writes.lock().unwrap().calls, 0);
        conn.send(Message::ReleaseAll).await.unwrap();
        assert_eq!(writes.lock().unwrap().calls, 1);
    }

    #[tokio::test]
    async fn stalled_flush_times_out_and_permanently_disables_writer() {
        let writes = Arc::new(Mutex::new(Writes {
            block_flush: true,
            ..Writes::default()
        }));
        let mut conn = Connection::new(RecordingStream(writes));
        let error = conn
            .send(Message::Move { dx: -1, dy: 2 })
            .await
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert_eq!(
            conn.send(Message::ReleaseAll).await.unwrap_err().kind(),
            io::ErrorKind::NotConnected
        );
    }

    #[tokio::test]
    async fn cancelled_flush_cannot_be_followed_by_another_frame() {
        let writes = Arc::new(Mutex::new(Writes {
            block_flush: true,
            ..Writes::default()
        }));
        let mut conn = Connection::new(RecordingStream(writes.clone()));
        assert!(
            timeout(Duration::from_millis(20), conn.send(Message::ReleaseAll))
                .await
                .is_err()
        );
        writes.lock().unwrap().block_flush = false;
        assert_eq!(
            conn.send(Message::ReleaseAll).await.unwrap_err().kind(),
            io::ErrorKind::NotConnected
        );
        assert_eq!(writes.lock().unwrap().calls, 1);
    }

    #[tokio::test]
    async fn flush_error_is_a_connection_failure() {
        let writes = Arc::new(Mutex::new(Writes {
            fail_flush: true,
            ..Writes::default()
        }));
        let mut conn = Connection::new(RecordingStream(writes));
        assert_eq!(
            conn.send(Message::ReleaseAll).await.unwrap_err().kind(),
            io::ErrorKind::BrokenPipe
        );
        assert_eq!(
            conn.send(Message::ReleaseAll).await.unwrap_err().kind(),
            io::ErrorKind::NotConnected
        );
    }
}
