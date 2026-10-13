# SeamlessKVM

**One keyboard and mouse. Windows and Mac.**

SeamlessKVM lets you control a Mac with the keyboard and mouse connected to your Windows PC. Move across the screen edge to switch computers; move back to return. Input travels over an authenticated, encrypted connection on your local network.

```text
        macOS client                       Windows host
    +--------------------+             +--------------------+
    |                    | <---------- |                    |
    |        Mac         |  move left  |     Windows 11     |
    |                    | ----------> |                    |
    +--------------------+  move right +--------------------+
                                         keyboard + mouse
```

The current layout is fixed: **Mac on the left, Windows on the right.** Each computer uses its own display. SeamlessKVM shares input; it does not stream video, sync the clipboard, or transfer files.

[Getting started](#getting-started) · [Configuration](#configuration) · [Troubleshooting](#troubleshooting) · [Development](#development)

> **Early development.** Build from source and expect rough edges. Native input behavior needs testing on your own Windows and Mac setup; see the [hardware validation checklist](docs/STAGE2_VALIDATION.md).

## Features

- **Edge switching.** Cross the selected Windows display's left edge to enter the Mac at its right edge. Your cursor keeps its relative vertical position between different screen sizes.
- **Keyboard and mouse forwarding.** Key repeats, left/right modifiers, five mouse buttons, dragging, and horizontal/vertical scrolling. Windows keys map to Command by default.
- **Native input.** Rust with Win32 hooks and Raw Input on Windows, Quartz on macOS, and Tokio networking. Mouse movement is event-driven, with no smoothing timer or deliberate buffering delay.
- **Windows control panel.** Build, pair, select a display, and start or stop the host from a native WPF window.
- **Connection recovery.** The client reconnects after network loss. The host restores local control on disconnect or timeout, and a watchdog clears cursor confinement after a host crash or input-pump stall.
- **Pinned TLS and pairing.** A certificate fingerprint identifies the host; a random pairing secret authenticates the client. Only one client is admitted at a time.

**Emergency shortcut: `Ctrl+Alt+Shift+F12` restores Windows control and disconnects the session.**

## Getting started

### Requirements

| Computer | Requirements |
| --- | --- |
| Windows host | Windows 11, a physical relative mouse, stable Rust, and Visual Studio C++ Build Tools with the Windows SDK. Rust's GNU toolchain with a compatible MinGW-w64 compiler is also supported. |
| Mac client | An interactive, logged-in macOS desktop, stable Rust, and Apple's Command Line Tools (`xcode-select --install`). Build on the Mac for its architecture. |
| Network | Both computers on the same LAN, with inbound TCP **24800** allowed on the Windows **Private** firewall profile. |

Clone the repository on **both computers**:

```sh
git clone https://github.com/shakodev/seamless-kvm.git
cd seamless-kvm
```

Run the commands below from the repository root. Paths inside host configuration files are resolved from the working directory, not from the TOML file's directory.

### 1. Set up Windows

Double-click **`SeamlessKVM.vbs`** to open the control panel. The launcher compiles itself on first use with Windows' included .NET Framework compiler; Rust and the C++ build tools are still needed to build the host.

1. Click **Build**.
2. Click **Create pairing…** and choose a new empty folder, such as `certs`. For an existing pairing, use **Browse…** to select `host.local.toml`.
3. Set **Listen address** to your Windows LAN IP and port, for example `192.168.1.10:24800`. New pairings default to `127.0.0.1:24800`, which only accepts connections from the same computer.
4. Select the **Windows display** whose left edge should switch to the Mac, then click **Save**.
5. Click **Check** to verify native setup, then **Start server**.

Allow inbound TCP 24800 through Windows Firewall on your Private network. The control panel does not change firewall rules.

**Build** and **Rebuild** stop the host launched by that window and leave it stopped until you click **Start server** again. Closing the window also stops its host. Keep the launcher and generated executables inside the repository.

<details>
<summary>Set up Windows from the command line</summary>

In PowerShell:

```powershell
cargo build --release -p windows-host
.\target\release\windows-host.exe pair certs
.\target\release\windows-host.exe monitors
```

Edit `certs/host.local.toml`:

- Set `listen` to your Windows LAN IP and port.
- Set `[monitor]` to the `left`, `top`, `width`, and `height` printed for your chosen display. These are physical desktop coordinates, which can be negative. The host rejects bounds that do not match an actual monitor.

Then check and start the host:

```powershell
.\target\release\windows-host.exe check certs\host.local.toml
.\target\release\windows-host.exe certs\host.local.toml
```

The `check` command sets up and tears down native input handling with input passing through. Stop a running command-line host before starting one in the GUI.

</details>

### 2. Pair and build the Mac client

The Windows pairing step creates four files:

| File | Purpose |
| --- | --- |
| `host-cert.der` | Host TLS certificate. |
| `host-key.der` | Host private key. Keep it on Windows. |
| `host.local.toml` | Windows configuration, including the pairing secret. |
| `mock.local.toml` | Client configuration, including the certificate pin and pairing secret. Despite its name, the native Mac client accepts this file. |

Copy **only `mock.local.toml`** to the Mac through a trusted channel and save it as `macos.local.toml` in the repository root. Change its `host` value to the Windows address you configured above:

```toml
host = "192.168.1.10:24800"
```

Keep the generated `certificate_sha256` and `pairing_secret` values intact. Then, on the Mac:

```sh
chmod 600 macos.local.toml
cargo build --release -p macos-client
./target/release/macos-client displays
./target/release/macos-client check macos.local.toml
```

The diagnostic requests **Accessibility** permission. Open **System Settings → Privacy & Security → Accessibility** and enable the terminal and/or client shown by macOS. Add the client binary with the `+` button if needed. Restart the terminal or client, then run `check` again. Rebuilding or moving the unsigned binary may require granting permission again. Screen Recording permission is not used.

### 3. Connect

With the Windows server running, start the Mac client:

```sh
./target/release/macos-client macos.local.toml
```

Wait for `Authenticated client ready` on Windows and `Connected and ready` on the Mac. Move past the selected Windows display's **left edge** to control the Mac. Move past the Mac display's **right edge** to return.

Keys and buttons held during entry transfer to the Mac, so you can continue a drag there. On return, remote holds are released. Release and press any still-held keys or buttons again to use them on Windows. Dragging does not move files or windows between computers.

Use **Stop server** in the control panel to end the session. For command-line processes, use `Ctrl+C` in their terminals when local control is available. The Mac client otherwise reconnects automatically, including after an emergency disconnect.

## Configuration

Start from generated pairing files. The examples in [`config/`](config/) document the schema; their placeholder credentials cannot authenticate a connection.

Optional settings in `macos.local.toml`:

| Setting | Default | Effect |
| --- | --- | --- |
| `display_id` | Main display | Select an external display using an ID from `macos-client displays`. A missing, unavailable, built-in, or mirrored-secondary selection falls back to the main display. |
| `win_to_command` | `true` | Map left/right Windows keys to left/right Command. Set `false` to ignore Windows keys. Ctrl remains Control; Alt becomes Option. |
| `pointer_speed` | `1.0` | Logical macOS points per physical mouse count. Accepts `0.05`–`20.0`; Retina displays need no extra multiplier. |
| `heartbeat_timeout_ms` | `2000` | Disconnect after this many milliseconds without receiving a message. Also configurable on the host. |
| `reconnect_ms` | `1000` | Delay before the client retries a connection. |

The native client discovers display dimensions itself and ignores `screen_width` and `screen_height` in the generated mock configuration. The cursor stays within the selected Mac display. Mac display changes trigger a reconnect and display selection; Windows display changes require updating the host's monitor bounds and restarting it.

Keyboard mapping follows physical key positions and the Mac's selected input layout. Use matching layouts for matching characters. AltGr arrives as Windows Ctrl+Alt, not as macOS Option alone.

## Security and limitations

Pairing uses a self-signed certificate, a SHA-256 certificate pin, and a random 256-bit secret. The pin is the trust anchor; certificate authority, hostname, and expiry checks are not used. TLS still verifies possession of the host's private key. To rotate credentials, generate a pairing in a new directory and replace the configurations on both computers. Pairing refuses to overwrite existing files.

Keep the private key and both generated configurations private. Keystrokes and pairing secrets are not logged. Windows input is intercepted only while an authenticated client is ready and the network worker remains responsive. Disconnects, timeouts, a full input queue, or the emergency shortcut restore local control.

Current limits:

- One Windows host, one Mac client, and a fixed Mac-on-the-left layout. There is no Linux endpoint or background service.
- Windows elevated applications, UAC/secure desktops, absolute mouse devices, and applications reading their own Raw Input are outside the supported input-isolation behavior.
- macOS login/lock screens, Secure Input, and applications that reject synthetic events can prevent input delivery. Simultaneous physical input on the Mac can interfere with cursor position and modifiers.
- Common keyboard positions, F1–F20, navigation/keypad keys, and left/right modifiers are mapped. Media, vendor, Fn, menu, and F21–F24 keys are ignored. Lock states and keyboard LEDs are not synchronized.
- The Mac releases held input on normal return, disconnect, timeout, and graceful shutdown. Force-killing the client or revoking Accessibility permission can prevent final releases; there is no Mac watchdog. Windows independently restores its own input on timeout.

## Troubleshooting

| Problem | What to check |
| --- | --- |
| Mac keeps retrying | Both addresses must use the Windows LAN IP, not `127.0.0.1`. Check the port, firewall, matching pairing files, and that no mock or second client is connected. |
| Connected, but no switch | Wait for the authenticated-ready status. Cross the selected Windows display's **left** edge. Recheck monitor bounds after changing Windows displays. |
| Mac receives no input | Rerun `macos-client check macos.local.toml`, verify Accessibility permission, and restart the launching terminal/client. |
| Wrong Mac display | Run `macos-client displays` again and update `display_id`. IDs can change after reconnecting a display or rebooting. |
| Pointer feels too fast or slow | Adjust `pointer_speed`. For jitter, compare a lower mouse polling rate or a wired network connection while keeping other settings unchanged. |
| GUI reports “Already running” | Stop the command-line host normally. The GUI does not take over or terminate a host it did not start. |
| Build cannot replace an executable | Stop that host before rebuilding. GUI builds use `target/gui-host/release`; command-line builds use `target/release`. Start the executable you actually rebuilt. |

When updating, rebuild and restart **both endpoints**. For input or connection problems, the [validation checklist](docs/STAGE2_VALIDATION.md) covers switching, permissions, held keys, and network failures.

## Development

| Directory | Contents |
| --- | --- |
| [`shared/`](shared/) | Versioned wire protocol, pinned TLS, configuration, and switching state. |
| [`windows-host/`](windows-host/) | Win32 hooks, Raw Input, host networking, and crash watchdog. |
| [`macos-client/`](macos-client/) | Quartz injection, keyboard mapping, display selection, and reconnect loop. |
| [`mock-client/`](mock-client/) | Simulated client for testing without native input injection. |
| [`windows-gui/`](windows-gui/) | WPF control panel and GUI lifecycle tests. |

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Tests cover framing, authentication, switching, event ordering, held-input cleanup, and connection failures. They do not replace physical testing on each OS. See the [protocol reference](docs/PROTOCOL.md) for wire details and invariants.

<details>
<summary>Test with the mock client on Windows</summary>

Create pairing files and configure the Windows display as above. Keep both `listen` and `host` at `127.0.0.1:24800` for a single-machine test. Build the mock:

```powershell
cargo build --release -p mock-client
```

Start the Windows host, then run this in a separate terminal:

```powershell
.\target\release\mock-client.exe certs\mock.local.toml --auto-return
```

Wait for the authenticated-ready status, then cross the configured left edge. The mock simulates a remote cursor without injecting input and returns control after two seconds. Omit `--auto-return` to return by moving past its virtual right edge. Stop the mock before connecting the Mac; it occupies the single client slot.

</details>

<details>
<summary>Native and GUI checks</summary>

Compile and smoke-test the control panel from Windows PowerShell:

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File windows-gui/Launch.ps1 -BuildOnly
powershell.exe -NoProfile -ExecutionPolicy Bypass -File windows-gui/Test.ps1
```

For the native host lifecycle check, stop all existing hosts first:

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File windows-gui/Test-Host.ps1
```

Type-check the macOS native wrapper from Windows without linking Apple frameworks:

```sh
cargo clippy -p macos-client --all-targets --features native-check -- -D warnings
```

Use `native-check` only with check/Clippy on non-macOS systems. On a Mac with Accessibility permission, run the opt-in desktop setup test:

```sh
cargo test -p macos-client native::tests::desktop_setup_and_teardown -- --ignored
```

</details>

Bug reports and focused pull requests are welcome. For input bugs, include OS versions, display layout/scaling, keyboard layout, and steps to reproduce. Include relevant tests for protocol, switching, or failure-handling changes, and state which OS you tested on. Keep pairing files and typed content out of reports.
