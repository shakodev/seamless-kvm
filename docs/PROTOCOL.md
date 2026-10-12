# Protocol v1

TCP with `TCP_NODELAY` at both endpoints, inside rustls TLS 1.2/1.3. The client checks the exact leaf certificate's SHA-256 pin and TLS handshake signatures. Authentication uses a random 32-byte pairing secret inside TLS, compared in constant time. No discovery or unauthenticated input channel exists.

All integers are little-endian. Every frame has an 8-byte header: ASCII `SK`, version `u8` (=1), kind `u8`, payload length `u8` (0..32), and three reserved zero bytes. Unknown versions/kinds, nonzero reserved bytes, wrong lengths, noncanonical booleans, invalid key/button values, and invalid screen bounds close the connection. Frames are at most 40 bytes. Encoding and parsing use fixed stack buffers; no native Rust/Win32 structure layout is serialized.

| Kind | Message | Payload fields in order |
| --- | --- | --- |
| 1 | Hello | secret `[u8;32]` |
| 2 | Welcome | empty |
| 3 | Screen | left `i32`, top `i32`, width `u32`, height `u32` |
| 4 | Ready | empty |
| 5 | Enter | epoch `u64`, normalized y `u16` |
| 6 | Return | epoch `u64`, normalized y `u16` |
| 7 | Move | relative dx `i32`, dy `i32` |
| 8 | Key | Windows virtual key `u16` (<256), scan code `u16`, extended `u8`, down `u8` |
| 9 | Button | button `u8`, down `u8` |
| 10 | Scroll | horizontal `u8`, delta `i16` |
| 11 | Heartbeat | sender counter `u64` |
| 12 | ReleaseAll | empty |

Booleans are 0/1. Buttons 0..4 mean left, right, middle, X1, X2. Scroll preserves signed Win32 wheel units (usually 120 per detent), including partial detents. Key repeat is represented by repeated downs. Left/right modifier virtual keys and extended scan flags remain distinct. The eventual macOS client must map these physical keys to native events; this stage does not implement that mapping. Width/height are 2..32768, with nonoverflowing coordinate endpoints.

Handshake (TLS plus application exchange has a 3-second deadline): client Hello; host Welcome + Screen; client Screen + Ready; host Ready. Only then may suppression start. The listener admits one handshake/session at a time and drops extra connections while a ready session is active. Rejected authentication never grants control.

The host owns input ordering: one Win32 thread queues Enter, held-input snapshot, subsequent movement/keys/buttons/scroll, and ReleaseAll into one bounded FIFO. One network writer drains it. Each session has a local generation tag; queued events from older connections are discarded before forwarding. Native arrival order of key/button presses and releases is preserved; Raw Input supplies relative movement independently of the legacy hooks.

At entry the client clears old state, records the epoch, starts at x=0, and maps y from 0..65535 to its screen's top..bottom. Motion is clamped vertically and at the right edge. Moving to x<0 clears all held inputs and sends Return with the same epoch and normalized y. The host accepts Return only for its current epoch/session, releases confinement, places the cursor a few pixels inside its right edge, and sends ReleaseAll. Input received while the client has no active epoch is ignored, allowing already-buffered input after a return to drain safely. Every new Enter and every ReleaseAll clears held state.

Both endpoints send independent heartbeats every 200 ms and require peer heartbeats before their configured timeout; input traffic cannot keep a silent peer alive. A dedicated reader task owns partial-frame state so `select!` cancellation cannot corrupt framing. Per-frame writes have a 500 ms deadline; once a write fails/times out the stream is discarded. Failure and shutdown revoke host suppression before attempting a best-effort ReleaseAll. Each endpoint must independently clear held input on disconnect or timeout, because release packets cannot be delivered across a broken link.

Input frames are deliberately excluded from Debug/logging. Configuration parse errors omit source snippets to avoid exposing secrets. Queue overflow is a connection failure rather than a dropped press/release. No event replay occurs across reconnection.
