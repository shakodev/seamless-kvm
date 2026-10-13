//! All Win32 FFI lives here. Callbacks and their thread-local Context run on one
//! message-pump thread. Windows owns hook pointers only until UnhookWindowsHookEx.
//! Raw-input buffers are aligned Rust RAWINPUT values and checked before reading.
//! Hooks use bounded try_send and never wait or perform network/file IO.
use crate::network::{Bridge, Command, InputPacket};
use anyhow::{bail, ensure, Context as _, Result};
use shared::{state::SwitchState, Bounds, Message};
use std::{
    cell::RefCell,
    mem::{size_of, zeroed},
    ptr::{null, null_mut},
    sync::{atomic::Ordering, mpsc, Arc, OnceLock},
    thread,
};
use tokio::sync::mpsc::Sender;
use windows_sys::Win32::{
    Foundation::*,
    Graphics::Gdi::*,
    System::{LibraryLoader::GetModuleHandleW, Threading::*},
    UI::{
        HiDpi::*,
        Input::{KeyboardAndMouse::*, *},
        WindowsAndMessaging::*,
    },
};

#[derive(Clone, Copy, Default)]
struct Key {
    down: bool,
    scan: u16,
    extended: bool,
}
struct Context {
    bounds: Bounds,
    bridge: Arc<Bridge>,
    tx: Sender<InputPacket>,
    commands: mpsc::Receiver<Command>,
    session: u64,
    switch: SwitchState,
    keys: [Key; 256],
    buttons: [bool; 5],
    // Swallow releases for physical inputs whose downs belonged to the remote
    // session (including the emergency chord), until physically released.
    quarantined_keys: [bool; 256],
    quarantined_buttons: [bool; 5],
    last_local: POINT,
    pending_edge: Option<POINT>,
    heartbeat: HANDLE,
    revoke: HANDLE,
    clip: HANDLE,
    last_pump_beat: u64,
}
thread_local! {static CTX:RefCell<Option<Context>>=const{RefCell::new(None)};}
static ACTIVE_BRIDGE: OnceLock<Arc<Bridge>> = OnceLock::new();
fn fail_open() -> bool {
    // A reentrant physical callback must revoke ownership: otherwise its local
    // down could be followed by a remotely suppressed up and stick Windows.
    if let Some(bridge) = ACTIVE_BRIDGE.get() {
        bridge.session.store(0, Ordering::Release);
        if bridge.clip_owned.load(Ordering::Acquire) {
            unsafe {
                ClipCursor(null());
            }
        }
    }
    false
}

impl Context {
    fn send(&mut self, message: Message) -> bool {
        if self
            .tx
            .try_send(InputPacket {
                session: self.session,
                message,
            })
            .is_ok()
        {
            true
        } else {
            self.bridge
                .session
                .compare_exchange(self.session, 0, Ordering::AcqRel, Ordering::Acquire)
                .ok();
            self.restore(None);
            false
        }
    }
    fn sync_session(&mut self) {
        let now = self.bridge.now();
        if now.saturating_sub(self.last_pump_beat) >= 100 {
            // Polling timeout zero never waits. Check revocation before a stalled
            // pump can resume suppressing input, even if WM_TIMER was starved.
            unsafe {
                if WaitForSingleObject(self.revoke, 0) == WAIT_OBJECT_0 {
                    self.bridge.session.store(0, Ordering::Release);
                    self.bridge.stop.store(true, Ordering::Release);
                }
                SetEvent(self.heartbeat);
            }
            self.last_pump_beat = now;
        }
        let mut session = self.bridge.session.load(Ordering::Acquire);
        let healthy = self.bridge.healthy();
        if session != 0 && !healthy {
            self.bridge
                .session
                .compare_exchange(session, 0, Ordering::AcqRel, Ordering::Acquire)
                .ok();
            session = 0;
        }
        if self.session != session || !healthy {
            if self.switch.remote() {
                self.restore(None);
            }
            self.session = if healthy { session } else { 0 };
            self.switch.set_ready(self.session != 0);
            self.pending_edge = None;
            if self.session == 0 {
                self.quarantined_keys.fill(false);
                self.quarantined_buttons.fill(false);
            }
        }
    }
    fn restore(&mut self, y: Option<u16>) {
        let was_remote = self.switch.remote();
        self.switch.release();
        // SAFETY: NULL releases the global confinement; all coordinates are
        // validated monitor coordinates. Never confine unless ClipCursor succeeds.
        unsafe {
            self.release_clip();
            if was_remote {
                let mut p: POINT = zeroed();
                GetCursorPos(&mut p);
                SetCursorPos(
                    (self.bounds.left + 2).min(self.bounds.right()),
                    y.map(|v| self.bounds.denormalize_y(v))
                        .unwrap_or(p.y)
                        .clamp(
                            self.bounds.top,
                            self.bounds.top + self.bounds.height as i32 - 1,
                        ),
                );
            }
        }
        if was_remote {
            for (q, k) in self.quarantined_keys.iter_mut().zip(self.keys.iter()) {
                *q = k.down;
            }
            self.quarantined_buttons = self.buttons;
        }
    }
    fn release_clip(&self) {
        if self.bridge.clip_owned.swap(false, Ordering::AcqRel) {
            unsafe {
                ClipCursor(null());
                ResetEvent(self.clip);
            }
        }
    }
    fn timer(&mut self) {
        self.sync_session();
        while let Ok(Command::Return { session, epoch, y }) = self.commands.try_recv() {
            if session == self.session && self.switch.return_local(epoch) {
                // return_local validates the epoch; restore needs the previous
                // remote state for cursor placement and release quarantine.
                self.quarantined_keys = std::array::from_fn(|i| self.keys[i].down);
                self.quarantined_buttons = self.buttons;
                unsafe {
                    self.release_clip();
                    SetCursorPos(
                        (self.bounds.left + 2).min(self.bounds.right()),
                        self.bounds.denormalize_y(y),
                    );
                }
                self.send(Message::ReleaseAll);
            }
        }
        if self.bridge.stop.load(Ordering::Acquire) {
            unsafe {
                PostQuitMessage(0);
            }
        }
    }
    fn movement(&mut self, dx: i32, dy: i32) {
        self.sync_session();
        if self.switch.remote() {
            self.send(Message::Move { dx, dy });
            return;
        }
        if let Some(p) = self.pending_edge.take() {
            self.enter_at(self.bounds.left, p.y, dx);
            return;
        }
        let mut p: POINT = unsafe { zeroed() };
        if unsafe { GetCursorPos(&mut p) } == 0 {
            return;
        }
        if p.y < self.bounds.top
            || p.y >= self.bounds.top + self.bounds.height as i32
            || p.x < self.bounds.left
        {
            return;
        }
        self.enter_at(p.x, p.y, dx);
    }
    fn enter_at(&mut self, x: i32, y: i32, dx: i32) {
        let p = POINT { x, y };
        if let Some(enter) = self.switch.enter(self.bounds, p.x, p.y, dx) {
            let clip = RECT {
                left: self.bounds.left + 1,
                top: p.y,
                right: self.bounds.left + 2,
                bottom: p.y + 1,
            };
            // Mark ownership before confinement so the helper also covers a
            // crash between ClipCursor and the next instruction.
            self.bridge.clip_owned.store(true, Ordering::Release);
            unsafe {
                SetEvent(self.clip);
            }
            if unsafe { ClipCursor(&clip) } == 0 {
                self.restore(None);
                return;
            }
            // Balance local downs before transferring held modifiers/buttons.
            if !self.release_local() {
                self.restore(None);
                return;
            }
            if !self.send(enter) {
                return;
            }
            // Modifiers precede other held keys and drag buttons on entry.
            for modifiers in [true, false] {
                for i in 0..256 {
                    let modifier = matches!(i,0x10..=0x12|0x5b|0x5c|0xa0..=0xa5);
                    let k = self.keys[i];
                    if modifier == modifiers
                        && k.down
                        && !self.send(Message::Key {
                            vk: i as u16,
                            scan: k.scan,
                            extended: k.extended,
                            down: true,
                        })
                    {
                        return;
                    }
                }
            }
            for i in 0..5 {
                if self.buttons[i]
                    && !self.send(Message::Button {
                        button: i as u8,
                        down: true,
                    })
                {
                    return;
                }
            }
            self.quarantined_keys.fill(false);
            self.quarantined_buttons.fill(false);
        }
    }
    fn release_local(&self) -> bool {
        // Fixed stack storage; injected events bypass both hooks. This balances
        // downs Windows already saw, so crossing during a drag cannot stick it.
        let mut inputs: [INPUT; 261] = unsafe { zeroed() };
        let mut n = 0;
        for (vk, key) in self.keys.iter().enumerate() {
            if key.down && !self.quarantined_keys[vk] {
                inputs[n] = INPUT {
                    r#type: INPUT_KEYBOARD,
                    Anonymous: INPUT_0 {
                        ki: KEYBDINPUT {
                            wVk: vk as u16,
                            wScan: key.scan,
                            dwFlags: KEYEVENTF_KEYUP
                                | if key.extended {
                                    KEYEVENTF_EXTENDEDKEY
                                } else {
                                    0
                                },
                            time: 0,
                            dwExtraInfo: 0,
                        },
                    },
                };
                n += 1;
            }
        }
        for (i, down) in self.buttons.iter().enumerate() {
            if *down && !self.quarantined_buttons[i] {
                let (flags, data) = match i {
                    0 => (MOUSEEVENTF_LEFTUP, 0),
                    1 => (MOUSEEVENTF_RIGHTUP, 0),
                    2 => (MOUSEEVENTF_MIDDLEUP, 0),
                    3 => (MOUSEEVENTF_XUP, 1),
                    _ => (MOUSEEVENTF_XUP, 2),
                };
                inputs[n] = INPUT {
                    r#type: INPUT_MOUSE,
                    Anonymous: INPUT_0 {
                        mi: MOUSEINPUT {
                            dx: 0,
                            dy: 0,
                            mouseData: data,
                            dwFlags: flags,
                            time: 0,
                            dwExtraInfo: 0,
                        },
                    },
                };
                n += 1;
            }
        }
        n == 0
            || unsafe { SendInput(n as u32, inputs.as_ptr(), size_of::<INPUT>() as i32) }
                == n as u32
    }
    fn key(&mut self, vk: usize, scan: u16, extended: bool, down: bool) -> bool {
        self.sync_session();
        if vk >= 256 {
            return false;
        }
        self.keys[vk] = Key {
            down,
            scan,
            extended,
        };
        let ctrl = self.keys[VK_LCONTROL as usize].down || self.keys[VK_RCONTROL as usize].down;
        let alt = self.keys[VK_LMENU as usize].down || self.keys[VK_RMENU as usize].down;
        let shift = self.keys[VK_LSHIFT as usize].down || self.keys[VK_RSHIFT as usize].down;
        if self.switch.remote() && down && vk == VK_F12 as usize && ctrl && alt && shift {
            self.bridge
                .session
                .compare_exchange(self.session, 0, Ordering::AcqRel, Ordering::Acquire)
                .ok();
            self.restore(None);
            self.switch.set_ready(false);
            return true;
        }
        if self.quarantined_keys[vk] {
            if !down {
                self.quarantined_keys[vk] = false;
            }
            return true;
        }
        if self.switch.remote() {
            self.send(Message::Key {
                vk: vk as u16,
                scan,
                extended,
                down,
            });
            true
        } else {
            false
        }
    }
    fn mouse(&mut self, msg: u32, data: u32, point: POINT) -> bool {
        self.sync_session();
        if msg == WM_MOUSEMOVE && !self.switch.remote() {
            let previous = self.last_local;
            self.last_local = point;
            // A fast motion may jump over the configured edge onto an adjacent
            // monitor before WM_INPUT runs. Detect that crossing here too.
            if previous.x >= self.bounds.left
                && previous.x <= self.bounds.right()
                && previous.y >= self.bounds.top
                && previous.y < self.bounds.top + self.bounds.height as i32
                && point.x <= self.bounds.left
                && point.x < previous.x
                && point.y >= self.bounds.top
                && point.y < self.bounds.top + self.bounds.height as i32
            {
                self.pending_edge = Some(point);
            }
        }
        let button = match msg {
            WM_LBUTTONDOWN => Some((0, true)),
            WM_LBUTTONUP => Some((0, false)),
            WM_RBUTTONDOWN => Some((1, true)),
            WM_RBUTTONUP => Some((1, false)),
            WM_MBUTTONDOWN => Some((2, true)),
            WM_MBUTTONUP => Some((2, false)),
            WM_XBUTTONDOWN | WM_XBUTTONUP => {
                Some((if data >> 16 == 1 { 3 } else { 4 }, msg == WM_XBUTTONDOWN))
            }
            _ => None,
        };
        if let Some((button, down)) = button {
            self.buttons[button] = down;
            if self.quarantined_buttons[button] {
                if !down {
                    self.quarantined_buttons[button] = false;
                }
                return true;
            }
            if self.switch.remote() {
                self.send(Message::Button {
                    button: button as u8,
                    down,
                });
                return true;
            }
        }
        if self.switch.remote() && (msg == WM_MOUSEWHEEL || msg == WM_MOUSEHWHEEL) {
            self.send(Message::Scroll {
                horizontal: msg == WM_MOUSEHWHEEL,
                delta: (data >> 16) as i16,
            });
            return true;
        }
        // Mouse Raw Input remains available independently of suppressed legacy
        // movement. Relative deltas are forwarded exclusively from WM_INPUT.
        self.switch.remote() && msg == WM_MOUSEMOVE
    }
}

unsafe extern "system" fn keyboard_hook(code: i32, w: WPARAM, l: LPARAM) -> LRESULT {
    // SAFETY: Win32 provides valid KBDLLHOOKSTRUCT for HC_ACTION only.
    if code == HC_ACTION as i32 {
        let k = &*(l as *const KBDLLHOOKSTRUCT);
        if k.flags & LLKHF_INJECTED == 0 {
            let down = w as u32 == WM_KEYDOWN || w as u32 == WM_SYSKEYDOWN;
            let suppressed = CTX.with(|s| {
                s.try_borrow_mut()
                    .ok()
                    .and_then(|mut s| {
                        s.as_mut().map(|c| {
                            c.key(
                                k.vkCode as usize,
                                k.scanCode as u16,
                                k.flags & LLKHF_EXTENDED != 0,
                                down,
                            )
                        })
                    })
                    .unwrap_or_else(fail_open)
            });
            if suppressed {
                return 1;
            }
        }
    }
    CallNextHookEx(null_mut(), code, w, l)
}
unsafe extern "system" fn mouse_hook(code: i32, w: WPARAM, l: LPARAM) -> LRESULT {
    // SAFETY: Win32 provides valid MSLLHOOKSTRUCT for HC_ACTION only.
    if code == HC_ACTION as i32 {
        let m = &*(l as *const MSLLHOOKSTRUCT);
        if m.flags & LLMHF_INJECTED == 0 {
            let suppressed = CTX.with(|s| {
                s.try_borrow_mut()
                    .ok()
                    .and_then(|mut s| s.as_mut().map(|c| c.mouse(w as u32, m.mouseData, m.pt)))
                    .unwrap_or_else(fail_open)
            });
            if suppressed {
                return 1;
            }
        }
    }
    CallNextHookEx(null_mut(), code, w, l)
}
unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    if msg == WM_INPUT {
        let mut raw: RAWINPUT = zeroed();
        let mut size = size_of::<RAWINPUT>() as u32;
        let count = GetRawInputData(
            l as HRAWINPUT,
            RID_INPUT,
            &mut raw as *mut _ as *mut _,
            &mut size,
            size_of::<RAWINPUTHEADER>() as u32,
        );
        if count != u32::MAX
            && count >= size_of::<RAWINPUTHEADER>() as u32
            && raw.header.dwType == RIM_TYPEMOUSE
        {
            let mouse = raw.data.mouse;
            CTX.with(|s| {
                if let Ok(mut s) = s.try_borrow_mut() {
                    if let Some(c) = s.as_mut() {
                        if mouse.usFlags & MOUSE_MOVE_ABSOLUTE == 0 {
                            c.movement(mouse.lLastX, mouse.lLastY);
                        } else if c.switch.remote() {
                            c.bridge.session.store(0, Ordering::Release);
                            c.restore(None);
                        }
                    }
                }
            });
        }
        // DefWindowProc performs required foreground WM_INPUT cleanup.
    } else if msg == WM_TIMER {
        CTX.with(|s| {
            if let Ok(mut s) = s.try_borrow_mut() {
                if let Some(c) = s.as_mut() {
                    c.timer();
                }
            }
        });
        return 0;
    }
    DefWindowProcW(hwnd, msg, w, l)
}

struct Resources {
    keyboard: HHOOK,
    mouse: HHOOK,
    window: HWND,
    bridge: Arc<Bridge>,
    heartbeat: Handle,
    revoke: Handle,
    clip: Handle,
    _instance: Handle,
}
struct Handle(HANDLE);
impl Drop for Handle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
}
fn event_name(pid: u32, suffix: &str) -> Vec<u16> {
    format!("Local\\SeamlessKVM-{pid}-{suffix}\0")
        .encode_utf16()
        .collect()
}
unsafe fn create_event(pid: u32, suffix: &str) -> Result<Handle> {
    let handle = CreateEventW(
        null(),
        (suffix == "clip") as i32,
        0,
        event_name(pid, suffix).as_ptr(),
    );
    ensure!(!handle.is_null(), "watchdog event creation failed");
    Ok(Handle(handle))
}
impl Drop for Resources {
    fn drop(&mut self) {
        self.bridge.session.store(0, Ordering::Release);
        self.bridge.stop.store(true, Ordering::Release);
        unsafe {
            if self.bridge.clip_owned.swap(false, Ordering::AcqRel) {
                ClipCursor(null());
                ResetEvent(self.clip.0);
            }
            if !self.keyboard.is_null() {
                UnhookWindowsHookEx(self.keyboard);
            }
            if !self.mouse.is_null() {
                UnhookWindowsHookEx(self.mouse);
            }
            if !self.window.is_null() {
                KillTimer(self.window, 1);
                DestroyWindow(self.window);
            }
        }
        CTX.with(|s| {
            s.borrow_mut().take();
        });
    }
}

pub fn start(
    bounds: Bounds,
    bridge: Arc<Bridge>,
    tx: Sender<InputPacket>,
    commands: mpsc::Receiver<Command>,
) -> Result<thread::JoinHandle<()>> {
    let (started, ready) = mpsc::sync_channel(1);
    ensure!(
        ACTIVE_BRIDGE.set(bridge.clone()).is_ok(),
        "input subsystem already initialized"
    );
    let input = thread::Builder::new()
        .name("win32-input".into())
        .spawn(move || {
            // SAFETY: all window, hook, raw registration and destruction operations
            // are performed on this thread, whose pump outlives their registrations.
            let setup = unsafe { setup(bounds, bridge, tx, commands) };
            match setup {
                Ok(_resources) => {
                    if started.send(Ok(())).is_ok() {
                        unsafe {
                            let mut msg: MSG = zeroed();
                            while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
                                TranslateMessage(&msg);
                                DispatchMessageW(&msg);
                            }
                        }
                    }
                }
                Err(e) => {
                    let _ = started.send(Err(e));
                }
            }
        })?;
    ready
        .recv()
        .context("input thread failed during startup")??;
    Ok(input)
}
unsafe fn setup(
    bounds: Bounds,
    bridge: Arc<Bridge>,
    tx: Sender<InputPacket>,
    commands: mpsc::Receiver<Command>,
) -> Result<Resources> {
    let instance_name: Vec<u16> = "Local\\SeamlessKVMHost\0".encode_utf16().collect();
    let instance_guard = Handle(CreateMutexW(null(), 0, instance_name.as_ptr()));
    ensure!(
        !instance_guard.0.is_null() && GetLastError() != ERROR_ALREADY_EXISTS,
        "another host or native diagnostic is running"
    );
    ensure!(
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) != 0,
        "could not enable physical-pixel DPI awareness"
    );
    let rect = RECT {
        left: bounds.left,
        top: bounds.top,
        right: bounds.right() + 1,
        bottom: bounds.top + bounds.height as i32,
    };
    let monitor = MonitorFromRect(&rect, MONITOR_DEFAULTTONULL);
    ensure!(!monitor.is_null(), "configured monitor not found");
    let mut info: MONITORINFO = zeroed();
    info.cbSize = size_of::<MONITORINFO>() as u32;
    ensure!(
        GetMonitorInfoW(monitor, &mut info) != 0,
        "monitor query failed"
    );
    let actual = info.rcMonitor;
    ensure!(
        actual.left == rect.left
            && actual.top == rect.top
            && actual.right == rect.right
            && actual.bottom == rect.bottom,
        "configured bounds must match an actual monitor in physical pixels"
    );
    let pid = std::process::id();
    let heartbeat = create_event(pid, "heartbeat")?;
    let revoke = create_event(pid, "revoke")?;
    let ready = create_event(pid, "ready")?;
    let clip = create_event(pid, "clip")?;
    SetEvent(heartbeat.0);
    // Detached helper survives a host crash or stalled message pump.
    use std::os::windows::process::CommandExt;
    let _child = std::process::Command::new(std::env::current_exe()?)
        .arg("--watchdog")
        .arg(std::process::id().to_string())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .context("could not start crash watchdog")?;
    ensure!(
        WaitForSingleObject(ready.0, 3000) == WAIT_OBJECT_0,
        "crash watchdog did not start"
    );
    let mut resources = Resources {
        keyboard: null_mut(),
        mouse: null_mut(),
        window: null_mut(),
        bridge: bridge.clone(),
        heartbeat,
        revoke,
        clip,
        _instance: instance_guard,
    };
    let instance = GetModuleHandleW(null());
    let name: Vec<u16> = "SeamlessKVMInput\0".encode_utf16().collect();
    let class = WNDCLASSW {
        lpfnWndProc: Some(wndproc),
        hInstance: instance,
        lpszClassName: name.as_ptr(),
        ..zeroed()
    };
    ensure!(
        RegisterClassW(&class) != 0,
        "window class registration failed"
    );
    resources.window = CreateWindowExW(
        0,
        name.as_ptr(),
        name.as_ptr(),
        0,
        0,
        0,
        0,
        0,
        HWND_MESSAGE,
        null_mut(),
        instance,
        null(),
    );
    ensure!(!resources.window.is_null(), "input window creation failed");
    let mut last_local: POINT = zeroed();
    GetCursorPos(&mut last_local);
    // Capture physical holds that predate hook installation, excluding generic
    // modifier aliases and mouse virtual keys (tracked separately).
    let mut keys = [Key::default(); 256];
    for vk in 8..255 {
        if matches!(vk, 0x10..=0x12) {
            continue;
        }
        let scan = MapVirtualKeyW(vk, MAPVK_VK_TO_VSC_EX);
        keys[vk as usize] = Key {
            down: GetAsyncKeyState(vk as i32) < 0,
            scan: (scan & 0xff) as u16,
            extended: (scan >> 8) == 0xe0,
        };
    }
    let buttons = [VK_LBUTTON, VK_RBUTTON, VK_MBUTTON, VK_XBUTTON1, VK_XBUTTON2]
        .map(|vk| GetAsyncKeyState(vk as i32) < 0);
    CTX.with(|s| {
        *s.borrow_mut() = Some(Context {
            bounds,
            bridge,
            tx,
            commands,
            session: 0,
            switch: SwitchState::default(),
            keys,
            buttons,
            quarantined_keys: [false; 256],
            quarantined_buttons: [false; 5],
            last_local,
            pending_edge: None,
            heartbeat: resources.heartbeat.0,
            revoke: resources.revoke.0,
            clip: resources.clip.0,
            last_pump_beat: 0,
        })
    });
    let device = RAWINPUTDEVICE {
        usUsagePage: 1,
        usUsage: 2,
        dwFlags: RIDEV_INPUTSINK,
        hwndTarget: resources.window,
    };
    ensure!(
        RegisterRawInputDevices(&device, 1, size_of::<RAWINPUTDEVICE>() as u32) != 0,
        "raw input registration failed"
    );
    resources.keyboard = SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_hook), instance, 0);
    ensure!(!resources.keyboard.is_null(), "keyboard hook failed");
    resources.mouse = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_hook), instance, 0);
    ensure!(!resources.mouse.is_null(), "mouse hook failed");
    ensure!(
        SetTimer(resources.window, 1, 20, None) != 0,
        "watchdog timer failed"
    );
    Ok(resources)
}

pub fn watchdog(pid: u32) -> Result<()> {
    // Hooks are removed by Windows when their owner dies. ClipCursor is shared
    // state, so this independent process explicitly releases it after host exit.
    unsafe {
        let process = Handle(OpenProcess(PROCESS_SYNCHRONIZE, 0, pid));
        if process.0.is_null() {
            bail!("could not open parent process");
        }
        let open = |suffix: &str| -> Result<Handle> {
            let event = OpenEventW(
                EVENT_MODIFY_STATE | SYNCHRONIZATION_SYNCHRONIZE,
                0,
                event_name(pid, suffix).as_ptr(),
            );
            ensure!(!event.is_null(), "watchdog event open failed");
            Ok(Handle(event))
        };
        let heartbeat = open("heartbeat")?;
        let revoke = open("revoke")?;
        let ready = open("ready")?;
        let clip = open("clip")?;
        SetEvent(ready.0);
        let handles = [process.0, heartbeat.0];
        loop {
            match WaitForMultipleObjects(2, handles.as_ptr(), 0, 1000) {
                WAIT_OBJECT_0 => {
                    if WaitForSingleObject(clip.0, 0) == WAIT_OBJECT_0 {
                        ClipCursor(null());
                    }
                    break;
                }
                n if n == WAIT_OBJECT_0 + 1 => {}
                _ => {
                    SetEvent(revoke.0);
                    if WaitForSingleObject(clip.0, 0) == WAIT_OBJECT_0 {
                        ClipCursor(null());
                    }
                    WaitForSingleObject(process.0, INFINITE);
                    if WaitForSingleObject(clip.0, 0) == WAIT_OBJECT_0 {
                        ClipCursor(null());
                    }
                    break;
                }
            }
        }
    }
    Ok(())
}

pub fn monitors() -> Result<Vec<Bounds>> {
    unsafe extern "system" fn collect(
        monitor: HMONITOR,
        _dc: HDC,
        _rect: *mut RECT,
        data: LPARAM,
    ) -> BOOL {
        // SAFETY: EnumDisplayMonitors calls synchronously with our live Vec.
        let values = &mut *(data as *mut Vec<Bounds>);
        let mut info: MONITORINFO = zeroed();
        info.cbSize = size_of::<MONITORINFO>() as u32;
        if GetMonitorInfoW(monitor, &mut info) != 0 {
            let r = info.rcMonitor;
            values.push(Bounds {
                left: r.left,
                top: r.top,
                width: (r.right - r.left) as u32,
                height: (r.bottom - r.top) as u32,
            });
        }
        1
    }
    unsafe {
        ensure!(
            SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) != 0,
            "could not enable physical-pixel DPI awareness"
        );
        let mut values = Vec::new();
        ensure!(
            EnumDisplayMonitors(
                null_mut(),
                null(),
                Some(collect),
                &mut values as *mut _ as LPARAM
            ) != 0,
            "monitor enumeration failed"
        );
        Ok(values)
    }
}

pub fn check(bounds: Bounds) -> Result<()> {
    let bridge = Arc::new(Bridge::new());
    let (tx, _rx) = tokio::sync::mpsc::channel(1024);
    let (_commands, rx) = mpsc::sync_channel(16);
    let input = start(bounds, bridge.clone(), tx, rx)?;
    // There is no authenticated session in this diagnostic. Hooks pass through.
    thread::sleep(std::time::Duration::from_millis(200));
    bridge.stop.store(true, Ordering::Release);
    input
        .join()
        .map_err(|_| anyhow::anyhow!("input thread failed"))?;
    println!("Win32 monitor, hooks, Raw Input, message pump, and crash watchdog initialized and shut down successfully (input passthrough)");
    Ok(())
}
