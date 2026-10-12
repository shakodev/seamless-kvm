# SeamlessKVM — Stage 1

Windows 11 sends keyboard and mouse input over authenticated TLS to a Windows-compatible mock client. The mock simulates a macOS screen and held input state; it never injects input. The native macOS client is a placeholder for a later stage. No video, clipboard, web UI, or background service.

## Build and pair

Install stable Rust and the Visual Studio C++ build tools/Windows SDK (or Rust's Windows GNU toolchain plus a compatible MinGW-w64 compiler on PATH). From the repository root in PowerShell:

```powershell
cargo build --release --workspace
.\target\release\windows-host.exe pair certs
.\target\release\windows-host.exe monitors
```

Set `[monitor]` in `certs/host.local.toml` to the physical pixel bounds of the Windows monitor whose right edge should switch control. The host rejects bounds that do not match an actual monitor. `monitors` prints the required values, including negative desktop coordinates. Paths inside configurations are relative to the working directory. Example schemas are in `config/`.

The pairing command creates a self-signed certificate, PKCS#8 DER private key, random 256-bit secret, and matching host/mock configurations. It refuses to overwrite files. Keep the private key and both configurations private. Copy only `mock.local.toml` to the client through a trusted channel. The SHA-256 certificate pin is the trust anchor: TLS checks possession of its private key, without relying on CA, hostname, or expiration checks. Changing the certificate requires updating the pin. Regenerate in a new directory to rotate credentials.

## Run on one Windows machine

First check native API startup with input always passing through:

```powershell
.\target\release\windows-host.exe check certs\host.local.toml
```

Then run these commands in separate terminals:

```powershell
.\target\release\windows-host.exe certs\host.local.toml
.\target\release\mock-client.exe certs\mock.local.toml
```

Wait for `Authenticated client ready`. Move past the configured monitor's right edge. The Windows cursor is confined to a pixel inside that edge; physical relative movement controls the mock's virtual cursor. Vertical entry/return positions scale between different screen heights. Move left beyond the mock's x=0 to return. The mock prints ownership changes and cursor coordinates once per second, never keys or credentials. Add `--auto-return` to the mock command to return automatically two seconds after each entry.

Keys, left/right modifiers, five mouse buttons, horizontal/vertical scrolling, and held-button dragging are forwarded in order. Held Windows keys/buttons receive local releases at entry and corresponding remote downs (modifiers first). At return or failure, remote holds are cleared. Release and press held keys/buttons again to use them locally after a normal return; old remote presses are not replayed into Windows applications. Dragging is simulated within the remote screen; this does not transfer windows, files, or desktop objects.

**Emergency: Ctrl+Alt+Shift+F12 restores local control and disconnects the session.** Ctrl+C stops either program when local. The mock reconnects automatically after host restart, timeout, or emergency disconnection.

For LAN testing, set host `listen` to its private LAN IP and mock `host` to that same address. Allow inbound TCP 24800 on the Windows **Private** firewall profile. The generated defaults bind loopback. Only one authenticated client is admitted; additional connections cannot replace it.

## Failure behavior and limits

- Suppression requires an authenticated, ready session and a fresh network-worker lease. No input event is silently dropped: a full 1,024-event queue revokes the session.
- Heartbeats run every 200 ms. Default peer timeout is 2 seconds, writes time out after 500 ms, and a stalled network worker loses its local suppression lease after 750 ms. The input thread checks failures on events and a 20 ms timer.
- Disconnect, malformed frames, invalid client messages, queue overflow, emergency, and shutdown restore local control. `ReleaseAll` is best effort over a usable connection; the mock also independently releases on disconnect/timeout. Stale return messages cannot end a newer switch.
- A separate small watchdog process clears cursor confinement after host exit/crash or a 1-second input-pump stall. It revokes a stalled pump before it can resume interception. Windows removes hooks when their owner exits.
- Use a physical relative mouse on the ordinary interactive desktop. Absolute mouse devices fail back to local control. Elevated windows, secure desktops/UAC, applications reading their own Raw Input, and other software that replaces global cursor confinement are outside Stage 1's isolation guarantees. Low-level hooks suppress legacy Windows input, not other applications' independently registered Raw Input.
- Display topology is validated at startup; restart and update bounds after changing monitors. A second host or native diagnostic in the same Windows session is rejected. No macOS injection or macOS validation is included.

## Checks

```powershell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Tests cover framing, partial reads, pinning, pairing rejection, switching, drag state, abrupt disconnects, heartbeat expiry, extra-client rejection, stale sessions, and release behavior. `windows-host check` verifies actual Win32 setup/teardown while input passes through. Physical edge crossing, emergency keys, and mixed-DPI behavior still require interactive testing on the intended Windows desktop; automated checks do not certify those hardware paths. See [docs/PROTOCOL.md](docs/PROTOCOL.md) for the wire format and invariants.

If using the workspace-local toolchain prepared during development, enable it for the current PowerShell session before running Cargo:

```powershell
$env:RUSTUP_HOME = "$PWD\.tools\rustup"
$env:CARGO_HOME = "$PWD\.tools\cargo"
$env:PATH = "$PWD\.tools\cargo\bin;$PWD\.tools\w64devkit\bin;$env:PATH"
```
