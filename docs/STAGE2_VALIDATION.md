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
| Cursor switching | Enter through Windows' left edge near top/middle/bottom; cursor appears on Mac's right edge at matching relative height. Move left then right past the edge; Windows resumes near its left edge at matching height | Pending both |
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

## Left-side layout update (2026-10-13)

The user reported that basic Windows/Mac switching worked after correcting the Windows monitor bounds. That report does not establish every input/permission/failure test above. The new layout now exits Windows on the left, enters Mac on the right, and returns through the Mac's right edge. The right side of Windows and the left side of the Mac do not switch control.

Added regression coverage checks authenticated left-edge entry, rejecting inward/stationary motion, a negative Windows monitor origin, Mac right-edge entry at three vertical positions, left-edge clamping, right-edge return with the current epoch, scaled dragging, and held-input release. Updated both TLS tests and the executable mock scenario for the mirrored layout. Windows formatting, workspace/native-wrapper Clippy, all 31 tests, and release Windows host/mock builds passed. Physical validation of this new direction still requires rebuilding and restarting the client on the Mac.

## Mouse responsiveness update (2026-10-13)

The user reports a Windows Ethernet host, a Mac on Wi-Fi, and a mouse currently polling at 2000 Hz (supports 125–8000 Hz). No packet timing or physical cursor measurements were available here. The update reduces per-event transport overhead and queued hover posting without adding a timer, changing the protocol, or interpolating future motion. Drag paths retain all movement samples.

Windows validation passed: formatting, workspace Clippy with warnings denied, native-wrapper Clippy with `native-check`, all 44 automated tests, and release Windows host/mock builds. The running host locked the original release executable, so the updated binaries were built under `target/mouse-optimized/release/`; the running session was left in place. The Mac native framework code was type-checked only; no Mac build or live cursor benchmark was performed.

Deterministic evidence, **not an end-to-end latency benchmark**:

- A full 32-message batch produces identical ordered bytes with one underlying write instead of 32. Pinned TLS integration tests exercise batched entry, dragging, return, and disconnect cleanup.
- Recorder tests apply 2,000 and 8,000 queued hover samples, flushing every 32: 63 and 250 motion posts respectively, with identical final position and total relative displacement to individual processing. These counts model queued bursts, not timed hardware polling.
- Regression tests cover lone-sample delivery without another report, stationary suppression, fractional speeds (0.25, 0.5, 1.25), negative/positive origins, all input ordering barriers, every drag sample, per-sample clamping, return followed by a reversal, new entry, protocol failure during a burst, and cleanup after a pending motion post fails.

On the intended desktops, rebuild/restart both release binaries and compare at the same DPI, display refresh rate, and `pointer_speed`:

1. At 2000 Hz, test slow diagonal motion, small circles, rapid direction changes, and stopping precisely on small targets. Look for cursor stepping or trailing after stopping.
2. Draw/drag curves with a held button, select text while holding Shift, click immediately after movement, and scroll while moving. Paths, modifiers, and click locations must remain correct.
3. Test fractional pointer speed, display clamps, and a quick return-edge crossing followed by reversal; switching must still happen at the first crossing.
4. Repeat at 1000 Hz, then compare a wired Mac connection if available. Record qualitative differences separately from any instrumented latency measurements. Repeat failure/emergency checks above while moving or dragging.
