//! All macOS FFI is isolated here. Quartz objects follow Create/Copy ownership;
//! pointers never outlive their RAII owner. No hooks, capture, or security bypass.
use crate::{
    input::{Display, Event, Injector},
    keyboard,
    session::DisplayChanges,
};
use anyhow::{ensure, Context, Result};
use shared::Bounds;
use std::{ffi::c_void, ptr, sync::Arc, thread};

type Ref = *const c_void;
type MutRef = *mut c_void;
#[repr(C)]
#[derive(Clone, Copy)]
struct Point {
    x: f64,
    y: f64,
}
#[repr(C)]
#[derive(Clone, Copy)]
struct Size {
    width: f64,
    height: f64,
}
#[repr(C)]
#[derive(Clone, Copy)]
struct Rect {
    origin: Point,
    size: Size,
}
type DisplayCallback = unsafe extern "C" fn(u32, u32, MutRef);
extern "C" {
    fn pthread_main_np() -> i32;
}
#[repr(C)]
struct TimerContext {
    version: isize,
    info: MutRef,
    retain: Ref,
    release: Ref,
    copy_description: Ref,
}
struct Startup {
    ready: Option<std::sync::mpsc::SyncSender<Result<usize>>>,
    run_loop: MutRef,
    delivered: bool,
}

#[cfg_attr(target_os = "macos", link(name = "CoreGraphics", kind = "framework"))]
extern "C" {
    fn CGGetActiveDisplayList(max: u32, displays: *mut u32, count: *mut u32) -> i32;
    fn CGDisplayBounds(display: u32) -> Rect;
    fn CGMainDisplayID() -> u32;
    fn CGDisplayIsBuiltin(display: u32) -> u32;
    fn CGDisplayIsInMirrorSet(display: u32) -> u32;
    fn CGDisplayMirrorsDisplay(display: u32) -> u32;
    fn CGDisplayRegisterReconfigurationCallback(callback: DisplayCallback, context: MutRef) -> i32;
    fn CGDisplayRemoveReconfigurationCallback(callback: DisplayCallback, context: MutRef) -> i32;
    fn CGEventSourceCreate(state: i32) -> MutRef;
    fn CGEventSourceSetLocalEventsSuppressionInterval(source: MutRef, seconds: f64);
    fn CGEventCreateMouseEvent(source: MutRef, kind: u32, position: Point, button: u32) -> MutRef;
    fn CGEventCreateKeyboardEvent(source: MutRef, code: u16, down: bool) -> MutRef;
    fn CGEventCreateScrollWheelEvent(
        source: MutRef,
        units: u32,
        wheels: u32,
        wheel1: i32,
        ...
    ) -> MutRef;
    fn CGEventSetFlags(event: MutRef, flags: u64);
    fn CGEventSetType(event: MutRef, kind: u32);
    fn CGEventSetIntegerValueField(event: MutRef, field: u32, value: i64);
    fn CGEventPost(tap: u32, event: MutRef);
}
#[cfg_attr(
    target_os = "macos",
    link(name = "ApplicationServices", kind = "framework")
)]
extern "C" {
    static kAXTrustedCheckOptionPrompt: Ref;
    fn AXIsProcessTrustedWithOptions(options: Ref) -> u8;
}
#[cfg_attr(target_os = "macos", link(name = "CoreFoundation", kind = "framework"))]
extern "C" {
    static kCFBooleanTrue: Ref;
    static kCFRunLoopDefaultMode: Ref;
    fn CFDictionaryCreate(
        allocator: Ref,
        keys: *const Ref,
        values: *const Ref,
        count: isize,
        key_callbacks: Ref,
        value_callbacks: Ref,
    ) -> MutRef;
    fn CFRetain(object: Ref) -> Ref;
    fn CFRelease(object: Ref);
    fn CFRunLoopGetCurrent() -> MutRef;
    fn CFRunLoopRun();
    fn CFRunLoopStop(run_loop: MutRef);
    fn CFRunLoopTimerCreate(
        allocator: Ref,
        fire_date: f64,
        interval: f64,
        flags: usize,
        order: isize,
        callback: unsafe extern "C" fn(MutRef, MutRef),
        context: MutRef,
    ) -> MutRef;
    fn CFRunLoopAddTimer(run_loop: MutRef, timer: MutRef, mode: Ref);
    fn CFRunLoopTimerInvalidate(timer: MutRef);
}

struct Owned(MutRef);
impl Owned {
    fn new(value: MutRef) -> Result<Self> {
        ensure!(!value.is_null(), "Quartz/Core Foundation allocation failed");
        Ok(Self(value))
    }
}
impl Drop for Owned {
    fn drop(&mut self) {
        // SAFETY: construction checks NULL; this is one owned Create/Retain ref.
        unsafe { CFRelease(self.0) }
    }
}

pub fn trusted(prompt: bool) -> Result<()> {
    // SAFETY: constant CF keys/values are immortal; no callbacks are needed for
    // a temporary dictionary of these unowned constants. AX does not retain it.
    let trusted = unsafe {
        if prompt {
            let options = Owned::new(CFDictionaryCreate(
                ptr::null(),
                &kAXTrustedCheckOptionPrompt,
                &kCFBooleanTrue,
                1,
                ptr::null(),
                ptr::null(),
            ))?;
            AXIsProcessTrustedWithOptions(options.0) != 0
        } else {
            AXIsProcessTrustedWithOptions(ptr::null()) != 0
        }
    };
    ensure!(trusted, "Accessibility permission required. Enable the terminal/client in System Settings > Privacy & Security > Accessibility (use the category named by the macOS prompt), then restart macos-client. See README.md.");
    Ok(())
}

pub fn displays() -> Result<Vec<Display>> {
    // Display enumeration is infrequent (startup/reconnect), never per input.
    let mut ids = [0u32; 64];
    let mut count = 0u32;
    // SAFETY: writable array/count have the advertised capacity; display IDs are
    // supplied by Quartz. Rect/Point use the 64-bit macOS C ABI.
    unsafe {
        ensure!(
            CGGetActiveDisplayList(ids.len() as u32, ids.as_mut_ptr(), &mut count) == 0,
            "could not enumerate active displays"
        );
        ensure!(count < ids.len() as u32, "too many displays");
        let main = CGMainDisplayID();
        let mut out = Vec::with_capacity(count as usize);
        for &id in &ids[..count as usize] {
            // Ignore mirror secondaries; they share a primary coordinate space.
            if CGDisplayIsInMirrorSet(id) != 0 && CGDisplayMirrorsDisplay(id) != 0 {
                continue;
            }
            let rect = CGDisplayBounds(id);
            ensure!(
                [
                    rect.origin.x,
                    rect.origin.y,
                    rect.size.width,
                    rect.size.height
                ]
                .iter()
                .all(|n| n.is_finite() && n.fract() == 0.0),
                "unsupported fractional display bounds"
            );
            ensure!(
                (i32::MIN as f64..=i32::MAX as f64).contains(&rect.origin.x)
                    && (i32::MIN as f64..=i32::MAX as f64).contains(&rect.origin.y)
                    && (2.0..=32768.0).contains(&rect.size.width)
                    && (2.0..=32768.0).contains(&rect.size.height),
                "unsupported display bounds"
            );
            let bounds = Bounds {
                left: rect.origin.x as i32,
                top: rect.origin.y as i32,
                width: rect.size.width as u32,
                height: rect.size.height as u32,
            };
            ensure!(bounds.valid(), "invalid display bounds");
            out.push(Display {
                id,
                bounds,
                builtin: CGDisplayIsBuiltin(id) != 0,
                main: id == main,
            });
        }
        Ok(out)
    }
}

pub struct Quartz {
    source: Owned,
}
impl Quartz {
    pub fn new() -> Result<Self> {
        trusted(false)?;
        // SAFETY: -1 is kCGEventSourceStatePrivate, isolating injected modifier
        // state. Suppression interval zero avoids blocking local Mac input.
        let source = unsafe {
            let source = Owned::new(CGEventSourceCreate(-1))?;
            CGEventSourceSetLocalEventsSuppressionInterval(source.0, 0.0);
            source
        };
        Ok(Self { source })
    }
}
impl Injector for Quartz {
    fn post(&mut self, event: Event) -> Result<()> {
        // SAFETY: source is a live owned CGEventSource. Event fields are Quartz
        // constants, buttons 0..4, key codes <128, finite global display points.
        // All events are created, posted once in protocol order, then released.
        unsafe {
            let (raw, flags) = match event {
                Event::Motion {
                    x,
                    y,
                    drag,
                    dx,
                    dy,
                    flags,
                } => {
                    let kind = match drag {
                        None => 5,
                        Some(0) => 6,
                        Some(1) => 7,
                        Some(_) => 27,
                    };
                    let e = Owned::new(CGEventCreateMouseEvent(
                        self.source.0,
                        kind,
                        Point { x, y },
                        u32::from(drag.unwrap_or(0)),
                    ))?;
                    CGEventSetIntegerValueField(e.0, 4, dx); // kCGMouseEventDeltaX
                    CGEventSetIntegerValueField(e.0, 5, dy);
                    (e, flags)
                }
                Event::Button {
                    x,
                    y,
                    button,
                    down,
                    clicks,
                    flags,
                } => {
                    let kind = match (button, down) {
                        (0, true) => 1,
                        (0, false) => 2,
                        (1, true) => 3,
                        (1, false) => 4,
                        (_, true) => 25,
                        (_, false) => 26,
                    };
                    let e = Owned::new(CGEventCreateMouseEvent(
                        self.source.0,
                        kind,
                        Point { x, y },
                        u32::from(button),
                    ))?;
                    CGEventSetIntegerValueField(e.0, 1, clicks); // kCGMouseEventClickState
                    (e, flags)
                }
                Event::Key {
                    code,
                    down,
                    repeat,
                    flags,
                } => {
                    let e = Owned::new(CGEventCreateKeyboardEvent(self.source.0, code, down))?;
                    if keyboard::modifier(code) != 0 || code == 57 {
                        CGEventSetType(e.0, 12);
                    }
                    CGEventSetIntegerValueField(e.0, 8, i64::from(repeat));
                    (e, flags)
                }
                Event::Scroll {
                    horizontal,
                    points,
                    flags,
                } => {
                    // 0=kCGScrollEventUnitPixel. Two signed i32 varargs axes;
                    // Windows positive horizontal is right, Quartz is left.
                    let (vertical, horizontal) = if horizontal { (0, points) } else { (points, 0) };
                    (
                        Owned::new(CGEventCreateScrollWheelEvent(
                            self.source.0,
                            0,
                            2,
                            vertical,
                            horizontal,
                        ))?,
                        flags,
                    )
                }
            };
            CGEventSetFlags(raw.0, flags);
            CGEventPost(0, raw.0); // kCGHIDEventTap; CGEventPost has no success result
        }
        Ok(())
    }
}

unsafe extern "C" fn display_changed(_: u32, _: u32, context: MutRef) {
    // SAFETY: main holds this Arc through run-loop teardown and callback
    // unregistration; callback never stores the borrowed pointer.
    unsafe {
        (&*context.cast::<DisplayChanges>()).mark();
    }
}
unsafe extern "C" fn keep_alive(_: MutRef, context: MutRef) {
    // SAFETY: Startup lives on the main stack until timer invalidation after
    // CFRunLoopRun returns. Readiness is sent inside an active run loop, so an
    // immediate worker exit cannot stop the loop before CFRunLoopRun starts.
    unsafe {
        let startup = &mut *context.cast::<Startup>();
        if let Some(ready) = startup.ready.take() {
            startup.delivered = ready.send(Ok(startup.run_loop as usize)).is_ok();
            if !startup.delivered {
                CFRunLoopStop(startup.run_loop);
            }
        }
    }
}

struct Registration(MutRef);
impl Drop for Registration {
    fn drop(&mut self) {
        // SAFETY: callback context remains live until after this registration.
        unsafe {
            CGDisplayRemoveReconfigurationCallback(display_changed, self.0);
        }
    }
}
struct Timer(Owned);
impl Drop for Timer {
    fn drop(&mut self) {
        // SAFETY: invalidation removes the timer before its stack context dies.
        unsafe {
            CFRunLoopTimerInvalidate(self.0 .0);
        }
    }
}

/// Pump Quartz notifications on the process main thread; Tokio and input posting
/// run on one worker. The startup timer fires once, then sleeps without polling.
pub fn run_main_loop<F>(changes: Arc<DisplayChanges>, work: F) -> Result<()>
where
    F: FnOnce() -> Result<()> + Send + 'static,
{
    // SAFETY: this is called from main, not a Tokio worker. Public display access
    // establishes the process WindowServer connection before registration.
    unsafe {
        ensure!(
            pthread_main_np() != 0,
            "Quartz run loop requires the main thread"
        );
        ensure!(CGMainDisplayID() != 0, "interactive macOS desktop required");
        let run_loop = CFRunLoopGetCurrent();
        let _retained_loop = Owned::new(CFRetain(run_loop).cast_mut())?;
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        let mut startup = Startup {
            ready: Some(tx),
            run_loop,
            delivered: false,
        };
        let mut timer_context = TimerContext {
            version: 0,
            info: (&mut startup as *mut Startup).cast(),
            retain: ptr::null(),
            release: ptr::null(),
            copy_description: ptr::null(),
        };
        let timer = Timer(Owned::new(CFRunLoopTimerCreate(
            ptr::null(),
            0.0,
            1e12,
            0,
            0,
            keep_alive,
            (&mut timer_context as *mut TimerContext).cast(),
        ))?);
        let context = Arc::as_ptr(&changes).cast_mut().cast::<c_void>();
        ensure!(
            CGDisplayRegisterReconfigurationCallback(display_changed, context) == 0,
            "could not register display notifications"
        );
        let _registration = Registration(context);
        let worker = thread::Builder::new()
            .name("network-input".into())
            .spawn(move || {
                // Timer sends readiness while CFRunLoopRun is already active. Stop
                // therefore cannot race ahead of starting the main run loop.
                let active_loop = rx
                    .recv()
                    .context("main run loop stopped before startup")??;
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(work));
                CFRunLoopStop(active_loop as MutRef);
                match result {
                    Ok(result) => result,
                    Err(panic) => std::panic::resume_unwind(panic),
                }
            })?;
        CFRunLoopAddTimer(run_loop, timer.0 .0, kCFRunLoopDefaultMode);
        CFRunLoopRun();
        worker
            .join()
            .map_err(|_| anyhow::anyhow!("network/input worker panicked"))?
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    #[test]
    #[ignore = "requires an interactive macOS desktop and Accessibility grant"]
    fn desktop_setup_and_teardown() {
        super::trusted(false).unwrap();
        assert!(!super::displays().unwrap().is_empty());
        let _source = super::Quartz::new().unwrap();
    }
}
