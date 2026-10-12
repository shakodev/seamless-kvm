# Stage 2 validation

## Recorded here (2026-10-12)

Development environment: Windows, Rust x86_64-pc-windows-gnu. There is no connected macOS machine. Native framework linking, Accessibility prompts, cursor movement, and physical Windows↔macOS interaction are **unverified**. Rust type-checking of the FFI wrapper does not verify Apple's ABI or event delivery.

Passed on Windows: workspace formatting, Clippy with warnings denied, workspace tests (29 tests), and native-wrapper Clippy using `native-check`. A workspace development build passed. `windows-host check` also initialized and tore down the monitor, hooks, Raw Input, message pump, and crash watchdog with input passing through.

The native input integration test uses real pinned TLS and the existing host handshake, replacing only Quartz event posting with a recorder. It checks normalized entry/return, held Command+A, repeats, scrolling, dragging, a second session, abrupt TLS loss, and balanced releases. Failure tests cover heartbeat expiry, invalid host messages, permission revocation, display generation changes, and cancellation cleanup. Unit tests also cover aliased physical keys, virtual-key changes before release, both Shift sides, high-resolution wheel remainders, all five buttons, Caps Lock, and allocation failure during cleanup.

## Run on the intended Windows and Mac desktops

Build both binaries on their respective OS and follow README pairing/setup. Use a harmless text document and a scrollable window. Keep the Windows emergency chord available: **Ctrl+Alt+Shift+F12**. Record OS versions, monitor arrangements/scaling, selected display ID, keyboard layouts, and observed results. Do not record typed content or pairing credentials.

| Check | Action and expected result | Status here |
| --- | --- | --- |
| macOS native build | `cargo build --release -p macos-client`, workspace fmt/Clippy/tests on Mac; no linker/ABI errors | Pending Mac |
| Permission denied | Run without Accessibility permission; clear setup message, no host-ready session, Windows remains usable | Pending Mac |
| Permission granted | Grant through macOS settings, restart, run `check`; opt-in `desktop_setup_and_teardown` test completes without hanging | Pending Mac |
| Display choice | Set external `display_id`; unplug it and verify fallback to main after reconnect; no ID uses main | Pending Mac |
| Cursor switching | Enter near Windows top/middle/bottom; cursor appears on Mac's left edge at matching relative height. Move right then left past the edge; Windows resumes near its right edge at matching height | Pending both |
| Resolution/scaling | Repeat with 4K/1080p, Retina scaled modes, negative monitor origins, and monitor above/below main. Check all four Mac bounds, fractional pointer speed, and repeat crossings | Pending both |
| Keyboard | Letters/punctuation in matching layouts, both Shifts together and separate releases, both Ctrl/Alt/Win sides, Win+A/Win+C/Win+V, arrows, Home/End, keypad Enter/divide and repeated keys. `win_to_command=false` ignores Win | Pending both |
| Mouse | All five buttons, right-click, double/triple-click, held-button entry and dragging, vertical/horizontal wheel in both directions and partial detents | Pending both |
| Holds at return | Return while holding a key and drag button; Mac receives releases, Windows remains usable. Physically release/repress for local use | Pending both |
| Network failure | While holding Shift/key/button remotely, unplug LAN or stop host. Mac releases holds by timeout and Windows resumes; restore LAN/restart host and verify reconnect without replay | Pending both |
| Graceful shutdown | While remotely holding inputs, send Mac SIGTERM/SIGHUP from another terminal; all holds release and Windows resumes. Ctrl+C also exits when terminal receives it | Pending both |
| Emergency | Trigger emergency chord remotely; Windows resumes, Mac releases, client reconnects, no stale input replays | Pending both |
| Live display changes | While dragging, unplug/change resolution/rotate selected Mac monitor; session ends, holds release, new bounds are advertised on reconnect | Pending both |
| Permission revoked | Disable Accessibility while remote; client stops and Windows resumes. macOS may reject final releases after revocation; physically reset any affected Mac keys/buttons | Pending both |

`CGEventPost` provides no delivery result, so visible application behavior must be checked. A headless macOS CI build or event recorder cannot substitute for these rows. Force-killing the Mac process cannot run its cleanup; the host's independent timeout still restores Windows input. Lock states/LEDs, media/vendor keys, and secure input limitations are listed in README.
