# SeamlessKVM — Stage 2

Windows 11 controls a native Rust macOS client over authenticated TLS. Move past Windows' left edge to enter the selected Mac display at its right edge; move right past that display's right edge to return. Quartz injects keyboard, mouse, dragging, and scrolling events in protocol order. The Stage 1 mock remains available for testing without injection. No video, clipboard, web UI, or background service.

## Build and pair

Install stable Rust and the Visual Studio C++ build tools/Windows SDK (or Rust's Windows GNU toolchain plus a compatible MinGW-w64 compiler on PATH). From the repository root in PowerShell:

```powershell
cargo build --release --workspace
.\target\release\windows-host.exe pair certs
.\target\release\windows-host.exe monitors
```

Set `[monitor]` in `certs/host.local.toml` to the physical pixel bounds of the Windows monitor whose left edge should switch control. The host rejects bounds that do not match an actual monitor. `monitors` prints the required values, including negative desktop coordinates. Paths inside configurations are relative to the working directory. Example schemas are in `config/`.

The pairing command creates a self-signed certificate, PKCS#8 DER private key, random 256-bit secret, and matching host/mock configurations. It refuses to overwrite files. Keep the private key and both configurations private. Copy only `mock.local.toml` to the client through a trusted channel. The SHA-256 certificate pin is the trust anchor: TLS checks possession of its private key, without relying on CA, hostname, or expiration checks. Changing the certificate requires updating the pin. Regenerate in a new directory to rotate credentials.

Switching direction is fixed to **Mac on the left of Windows**. After updating this version, rebuild and restart both endpoints; an older client still uses the opposite entry/return edges. Pairing credentials and monitor bounds do not need to change.

## Build and run on macOS

Use an interactive logged-in macOS desktop with stable Rust and Apple's command line tools (`xcode-select --install`). Build on the Mac for its architecture (Apple silicon or Intel); Windows builds do not produce a macOS executable:

```sh
cargo build --release -p macos-client
./target/release/macos-client displays
```

Copy the generated `mock.local.toml` through a trusted channel to the Mac as `macos.local.toml`. It contains the pin and pairing secret; **never copy the host private key**. Set `host` to the Windows LAN IP, set the host's `listen` to that same IP, and permit TCP 24800 on the Windows Private firewall profile. Only one client can connect, so stop the mock before using the native client. Protect the Mac configuration with `chmod 600 macos.local.toml`.

The generated Stage 1 file works directly; native display discovery ignores its `screen_width`/`screen_height`. Optional native settings are shown in [config/macos.example.toml](config/macos.example.toml):

- `display_id`: an external display ID printed by `displays`. If it is absent/unavailable, built-in, or a mirror secondary, use the main display. IDs can change after reconnecting hardware or rebooting; run `displays` again if needed.
- `win_to_command = true`: left/right Windows keys become left/right Command. Set false to ignore Windows keys. Ctrl remains Control; Alt becomes Option. Physical scan positions use the Mac's selected input layout, so select matching layouts for matching characters. AltGr arrives as Windows Ctrl+Alt, not macOS Option alone.
- `pointer_speed = 1.0`: logical macOS display points per physical mouse count. Entry and return preserve normalized vertical position across resolutions. Retina backing pixels need no extra multiplier; adjust speed for comfort. The cursor stays within the selected display.

Run the diagnostic once to request Accessibility permission:

```sh
./target/release/macos-client check macos.local.toml
```

If denied, the client exits before connecting. Follow the macOS prompt to **System Settings → Privacy & Security → Accessibility** (or the category named by your OS). Enable the launching terminal and/or client shown by macOS; add the client binary with the `+` button if needed. Restart the terminal/client and rerun `check`. Rebuilding or moving an unsigned binary may require granting permission again. `AXIsProcessTrustedWithOptions` checks authorization; the client never edits the permission database or bypasses security. The diagnostic enumerates displays and creates/destroys a Quartz source without injecting input or connecting to Windows. Screen Recording is not used.

Start the Windows host with its existing configuration, then on the Mac:

```sh
./target/release/macos-client macos.local.toml
```

Wait for ready on both machines, then cross Windows' left edge. Held modifiers, keys, and buttons transfer with control; dragging continues inside the selected Mac display. The client releases held input on return, new entry, `ReleaseAll`, disconnect, heartbeat expiry, and graceful shutdown (Ctrl+C, SIGTERM, SIGHUP). It reconnects automatically after host/network failure or emergency disconnect, without replaying input. Permission revocation releases input best effort and exits with setup guidance. Display changes close the current session, release holds, then re-enumerate and reconnect using current bounds. No display polling or input hooks run on the Mac; permission checks share the 200 ms heartbeat timer.

## Mouse responsiveness

Rebuild and restart both endpoints after updating the mouse optimizations. Stop the running Windows host before `cargo build --release -p windows-host` so Windows can replace its executable; on the Mac use `cargo build --release -p macos-client`. To build while the host is running, add `--target-dir target/mouse-optimized` and use that folder's `release/windows-host.exe` after stopping the old host. Existing pairing and `pointer_speed` settings still work; the wire format remains v1.

Input is event-driven, with no smoothing timer or deliberate buffering delay. The host sends up to 32 already-queued events in one TLS write, explicitly flushes encrypted data to the transport, and skips stationary Raw Input reports. The Mac posts each movement individually in order; the earlier hover-coalescing change was removed after reports of cursor jumps. Fractional pointer speeds preserve relative movement totals instead of rounding every small delta independently. Periodic Accessibility checks run off the input executor, with one check in flight and a timeout, so a slow OS query does not pause mouse posting.

If using the separately built host, stop the old host first and launch this exact executable from the repository root with your existing configuration:

```powershell
.\target\mouse-optimized\release\windows-host.exe certs\host.local.toml
```

Running `target/release/windows-host.exe` does **not** select the separately built update. Rebuild/restart the Mac client from the updated source too.

For the reported Ethernet-host / Wi-Fi-Mac setup, test the updated release builds at the existing 2000 Hz mouse setting first. If jitter remains, compare 1000 Hz with the same DPI and `pointer_speed`, then try Ethernet on the Mac if available to separate network effects from input processing. Keep other settings unchanged during each comparison. Automated tests do not establish a measured end-to-end latency or remove network delivery gaps. See the [mouse validation checklist](docs/STAGE2_VALIDATION.md#cursor-jump-correction-2026-10-13).

## Mock test on one Windows machine

First check native API startup with input always passing through:

```powershell
.\target\release\windows-host.exe check certs\host.local.toml
```

Then run these commands in separate terminals:

```powershell
.\target\release\windows-host.exe certs\host.local.toml
.\target\release\mock-client.exe certs\mock.local.toml
```

Wait for `Authenticated client ready`. Move past the configured monitor's left edge. The Windows cursor is confined to a pixel inside that edge; physical relative movement controls the mock's virtual cursor. Vertical entry/return positions scale between different screen heights. The mock starts at its right edge. Move right beyond that edge to return. The mock prints ownership changes and cursor coordinates once per second, never keys or credentials. Add `--auto-return` to the mock command to return automatically two seconds after each entry.

Keys, left/right modifiers, five mouse buttons, horizontal/vertical scrolling, and held-button dragging are forwarded in order. Held Windows keys/buttons receive local releases at entry and corresponding remote downs (modifiers first). At return or failure, remote holds are cleared. Release and press held keys/buttons again to use them locally after a normal return; old remote presses are not replayed into Windows applications. Dragging is simulated within the remote screen; this does not transfer windows, files, or desktop objects.

**Emergency: Ctrl+Alt+Shift+F12 restores local control and disconnects the session.** Ctrl+C stops either program when local. The mock reconnects automatically after host restart, timeout, or emergency disconnection.

For LAN testing, set host `listen` to its private LAN IP and mock `host` to that same address. Allow inbound TCP 24800 on the Windows **Private** firewall profile. The generated defaults bind loopback. Only one authenticated client is admitted; additional connections cannot replace it.

## Failure behavior and limits

- Suppression requires an authenticated, ready session and a fresh network-worker lease. No input event is silently dropped: a full 1,024-event queue revokes the session.
- Heartbeats run every 200 ms. Default peer timeout is 2 seconds, writes time out after 500 ms, and a stalled network worker loses its local suppression lease after 750 ms. The input thread checks failures on events and a 20 ms timer.
- Disconnect, malformed frames, invalid client messages, queue overflow, emergency, and shutdown restore local control. `ReleaseAll` is best effort over a usable connection; the mock also independently releases on disconnect/timeout. Stale return messages cannot end a newer switch.
- A separate small watchdog process clears cursor confinement after host exit/crash or a 1-second input-pump stall. It revokes a stalled pump before it can resume interception. Windows removes hooks when their owner exits.
- Use a physical relative mouse on the ordinary interactive desktop. Absolute mouse devices fail back to local control. Elevated windows, secure desktops/UAC, applications reading their own Raw Input, and other software that replaces global cursor confinement are outside Stage 1's isolation guarantees. Low-level hooks suppress legacy Windows input, not other applications' independently registered Raw Input.
- Windows display topology is validated at startup; restart and update host bounds after changing Windows monitors. A second host or native diagnostic in the same Windows session is rejected. The Mac handles display changes by disconnecting and reselecting its display.
- Quartz event posting has no delivery acknowledgement. Secure input, login/lock screens, permission revocation, and applications that reject synthetic events can prevent injection. Use one input source at a time: simultaneous physical Mac input can displace the cursor or interfere with modifiers. SIGKILL, process abort, power loss, or revoked permission can prevent final native releases; the Windows host still independently restores local control on timeout. This client does not install a Mac watchdog.
- Standard ANSI/ISO scan positions, common JIS positions, F1–F20, navigation/keypad, and left/right modifiers are mapped. Pause/PrintScreen become F15/F13, ScrollLock becomes F14, NumLock becomes keypad Clear. Media/vendor/Fn/menu keys and F21–F24 are ignored. Caps Lock transitions during remote control are supported, but existing Windows/Mac lock states and LEDs are not synchronized by protocol v1. Double/triple clicks use a 500 ms, 4-point threshold. Mouse side-button actions depend on the Mac application.

## Checks

```powershell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Tests cover framing, partial reads, pinning, pairing rejection, switching, drag state, abrupt disconnects, heartbeat expiry, extra-client rejection, stale sessions, and release behavior. `windows-host check` verifies actual Win32 setup/teardown while input passes through. Physical edge crossing, emergency keys, and mixed-DPI behavior still require interactive testing on the intended Windows desktop; automated checks do not certify those hardware paths. See [docs/PROTOCOL.md](docs/PROTOCOL.md) for the wire format and invariants.

The native client's tests additionally exercise scan/modifier mapping, negative display origins, scaling/clamping, ordered Quartz event descriptions, repeated keys, TLS entry/return and release on failure. Windows can type-check the isolated native Rust code with `cargo clippy -p macos-client --all-targets --features native-check -- -D warnings`; use this feature only for check/Clippy on non-macOS, since Apple frameworks cannot link there. On macOS, run the workspace checks above and the opt-in desktop diagnostic test:

```sh
cargo test -p macos-client native::tests::desktop_setup_and_teardown -- --ignored
```

**Current validation:** Windows formatting, workspace and native-wrapper Clippy, 45 automated tests, and release Windows host/mock builds passed. The Win32 startup/teardown diagnostic previously passed. A macOS build and actual Windows↔macOS latency/smoothness measurements have not been performed in this environment. Follow [docs/STAGE2_VALIDATION.md](docs/STAGE2_VALIDATION.md) on the intended machines before relying on native input behavior.

If using the workspace-local toolchain prepared during development, enable it for the current PowerShell session before running Cargo:

```powershell
$env:RUSTUP_HOME = "$PWD\.tools\rustup"
$env:CARGO_HOME = "$PWD\.tools\cargo"
$env:PATH = "$PWD\.tools\cargo\bin;$PWD\.tools\w64devkit\bin;$env:PATH"
```
