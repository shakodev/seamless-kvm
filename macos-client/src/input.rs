use crate::keyboard;
use anyhow::{ensure, Result};
use shared::{Bounds, Message};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Display {
    pub id: u32,
    pub bounds: Bounds, // global Quartz points, not backing pixels
    pub builtin: bool,
    pub main: bool,
}
pub fn select_display(displays: &[Display], preferred: Option<u32>) -> Option<Display> {
    displays
        .iter()
        .find(|d| Some(d.id) == preferred && !d.builtin)
        .or_else(|| displays.iter().find(|d| d.main))
        .copied()
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Event {
    Motion {
        x: f64,
        y: f64,
        drag: Option<u8>,
        dx: i64,
        dy: i64,
        flags: u64,
    },
    Button {
        x: f64,
        y: f64,
        button: u8,
        down: bool,
        clicks: i64,
        flags: u64,
    },
    Key {
        code: u16,
        down: bool,
        repeat: bool,
        flags: u64,
    },
    Scroll {
        horizontal: bool,
        points: i32,
        flags: u64,
    },
}

/// Implementations must post synchronously in call order; errors end the session.
pub trait Injector {
    fn post(&mut self, event: Event) -> Result<()>;
}

pub struct Input<I: Injector> {
    pub injector: I,
    pub display: Display,
    pub epoch: Option<u64>,
    pub x: f64,
    pub y: f64,
    keys: [bool; 128],
    physical_keys: [Option<u16>; 512],
    buttons: [bool; 5],
    clicks: [i64; 5],
    last_click: [Option<(Instant, f64, f64)>; 5],
    caps: bool,
    scroll_remainder: [i32; 2],
    speed: f64,
    win_to_command: bool,
    pending_motion: Option<Event>,
}
impl<I: Injector> Input<I> {
    pub fn new(injector: I, display: Display, speed: f64, win_to_command: bool) -> Self {
        Self {
            injector,
            display,
            epoch: None,
            x: f64::from(display.bounds.right()),
            y: f64::from(display.bounds.top),
            keys: [false; 128],
            physical_keys: [None; 512],
            buttons: [false; 5],
            clicks: [0; 5],
            last_click: [None; 5],
            caps: false,
            scroll_remainder: [0; 2],
            speed,
            win_to_command,
            pending_motion: None,
        }
    }
    fn flags(&self) -> u64 {
        [54u16, 55, 56, 60, 59, 62, 58, 61]
            .into_iter()
            .filter(|code| self.keys[usize::from(*code)])
            .fold(if self.caps { keyboard::CAPS } else { 0 }, |f, code| {
                f | keyboard::modifier(code)
            })
    }
    pub fn held_count(&self) -> usize {
        self.keys.iter().filter(|v| **v).count() + self.buttons.iter().filter(|v| **v).count()
    }
    /// Best effort through all releases, even if one native allocation fails.
    pub fn release(&mut self) -> Result<()> {
        // Flush before releasing modifiers/buttons, but still attempt every
        // release if posting the final movement fails.
        let mut first_error = self.flush_motion().err();
        self.epoch = None;
        self.physical_keys.fill(None);
        for modifiers in [false, true] {
            for code in 0..128 {
                if self.keys[code] && (keyboard::modifier(code as u16) != 0) == modifiers {
                    self.keys[code] = false;
                    if code == 57 {
                        self.caps = false;
                    }
                    let event = Event::Key {
                        code: code as u16,
                        down: false,
                        repeat: false,
                        flags: keyboard::event_flags(code as u16, self.flags()),
                    };
                    if let Err(e) = self.injector.post(event) {
                        first_error.get_or_insert(e);
                    }
                }
            }
        }
        if self.caps {
            self.caps = false;
            if let Err(e) = self.injector.post(Event::Key {
                code: 57,
                down: false,
                repeat: false,
                flags: self.flags(),
            }) {
                first_error.get_or_insert(e);
            }
        }
        for button in 0..5 {
            if self.buttons[button] {
                self.buttons[button] = false;
                if let Err(e) = self.injector.post(Event::Button {
                    x: self.x,
                    y: self.y,
                    button: button as u8,
                    down: false,
                    clicks: self.clicks[button],
                    flags: self.flags(),
                }) {
                    first_error.get_or_insert(e);
                }
            }
        }
        self.last_click.fill(None);
        self.scroll_remainder.fill(0);
        first_error.map_or(Ok(()), Err)
    }
    pub fn apply(&mut self, message: Message) -> Result<Option<Message>> {
        let reply = self.apply_queued(message)?;
        self.flush_motion()?;
        Ok(reply)
    }
    /// Only the session's bounded, already-available burst may defer hover
    /// posting. A lone sample is flushed immediately, with no smoothing timer.
    pub(crate) fn flush_motion(&mut self) -> Result<()> {
        if let Some(event) = self.pending_motion.take() {
            self.injector.post(event)?;
        }
        Ok(())
    }
    pub(crate) fn apply_queued(&mut self, message: Message) -> Result<Option<Message>> {
        // Never move across a key/button/scroll/entry/release ordering barrier.
        if !matches!(
            message,
            Message::Move { .. } | Message::Enter { .. } | Message::ReleaseAll
        ) {
            self.flush_motion()?;
        }
        if let Message::Enter { epoch, y } = message {
            self.release()?;
            self.epoch = Some(epoch);
            self.x = f64::from(self.display.bounds.right());
            self.y = f64::from(self.display.bounds.denormalize_y(y));
            self.injector.post(Event::Motion {
                x: self.x,
                y: self.y,
                drag: None,
                dx: 0,
                dy: 0,
                flags: 0,
            })?;
            return Ok(None);
        }
        if message == Message::ReleaseAll {
            self.release()?;
            return Ok(None);
        }
        if self.epoch.is_none() {
            return Ok(None);
        }
        match message {
            Message::Move { dx, dy } => {
                let b = self.display.bounds;
                let next_x = self.x + f64::from(dx) * self.speed;
                let old_y = self.y;
                self.y = (self.y + f64::from(dy) * self.speed)
                    .clamp(f64::from(b.top), f64::from(b.top) + f64::from(b.height - 1));
                if next_x > f64::from(b.right()) {
                    let reply = Message::Return {
                        epoch: self.epoch.unwrap(),
                        y: b.normalize_y(self.y.round() as i32),
                    };
                    self.release()?;
                    return Ok(Some(reply));
                }
                let old_x = self.x;
                self.x = next_x.max(f64::from(b.left));
                if self.x == old_x && self.y == old_y {
                    return Ok(None);
                }
                // Differences of rounded positions retain fractional counts
                // across reports; rounding each delta loses/overstates motion
                // at speeds such as 0.25 and 0.5 points per count.
                let mut dx = self.x.round() as i64 - old_x.round() as i64;
                let mut dy = self.y.round() as i64 - old_y.round() as i64;
                if let Some(Event::Motion {
                    dx: queued_x,
                    dy: queued_y,
                    ..
                }) = self.pending_motion
                {
                    dx += queued_x;
                    dy += queued_y;
                }
                let drag = self.buttons.iter().position(|b| *b).map(|b| b as u8);
                self.pending_motion = Some(Event::Motion {
                    x: self.x,
                    y: self.y,
                    drag,
                    dx,
                    dy,
                    flags: self.flags(),
                });
                // Drawing and drag selection need the entire path, even when
                // the network delivered a burst. Only hover motion coalesces.
                if drag.is_some() {
                    self.flush_motion()?;
                }
            }
            Message::Key {
                vk,
                scan,
                extended,
                down,
            } => {
                if let Some(code) = keyboard::translate(vk, scan, extended, self.win_to_command) {
                    // Track physical positions even if NumLock/layout changes
                    // the virtual key before release. Pause shares NumLock's
                    // scan, and synthetic special keys can have scan=0.
                    ensure!(vk < 256, "invalid virtual key");
                    let slot = if scan == 0 || scan >= 128 || vk == 0x13 {
                        256 + usize::from(vk)
                    } else {
                        usize::from(scan) + if extended { 128 } else { 0 }
                    };
                    let held = self.keys[usize::from(code)];
                    if down {
                        ensure!(
                            self.physical_keys[slot].is_none_or(|old| old == code),
                            "key identity changed while held"
                        );
                        if self.physical_keys[slot].is_none() {
                            self.physical_keys[slot] = Some(code);
                            if held {
                                return Ok(None);
                            }
                        }
                    } else {
                        if self.physical_keys[slot] != Some(code) {
                            return Ok(None);
                        }
                        self.physical_keys[slot] = None;
                        if self.physical_keys.contains(&Some(code)) {
                            return Ok(None);
                        }
                    }
                    // Modifier autorepeats would create spurious flagsChanged events.
                    if held && down && (keyboard::modifier(code) != 0 || code == 57) {
                        return Ok(None);
                    }
                    self.keys[usize::from(code)] = down;
                    if code == 57 && down && !held {
                        self.caps = !self.caps;
                    }
                    self.injector.post(Event::Key {
                        code,
                        down,
                        repeat: down && held,
                        flags: keyboard::event_flags(code, self.flags()),
                    })?;
                }
            }
            Message::Button { button, down } => {
                ensure!(button < 5, "invalid mouse button");
                let index = usize::from(button);
                if self.buttons[index] == down {
                    return Ok(None);
                }
                self.buttons[index] = down;
                if down {
                    let now = Instant::now();
                    let consecutive = self.last_click[index].is_some_and(|(t, x, y)| {
                        now.duration_since(t) <= Duration::from_millis(500)
                            && (x - self.x).abs() <= 4.0
                            && (y - self.y).abs() <= 4.0
                    });
                    self.clicks[index] = if consecutive {
                        (self.clicks[index] + 1).min(3)
                    } else {
                        1
                    };
                    self.last_click[index] = Some((now, self.x, self.y));
                }
                self.injector.post(Event::Button {
                    x: self.x,
                    y: self.y,
                    button,
                    down,
                    clicks: self.clicks[index],
                    flags: self.flags(),
                })?;
            }
            Message::Scroll { horizontal, delta } => {
                // WHEEL_DELTA=120 -> 30 Quartz pixels. Preserve sub-pixel
                // high-resolution deltas independently for each wheel axis.
                let remainder = &mut self.scroll_remainder[usize::from(horizontal)];
                let accumulated = *remainder + i32::from(delta);
                let points = accumulated / 4;
                *remainder = accumulated % 4;
                if points != 0 {
                    self.injector.post(Event::Scroll {
                        horizontal,
                        points: if horizontal { -points } else { points },
                        flags: self.flags(),
                    })?;
                }
            }
            _ => {}
        }
        Ok(None)
    }
}
impl<I: Injector> Drop for Input<I> {
    fn drop(&mut self) {
        let _ = self.release();
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    #[derive(Default)]
    pub struct Recording(pub Vec<Event>);
    impl Injector for Recording {
        fn post(&mut self, event: Event) -> Result<()> {
            self.0.push(event);
            Ok(())
        }
    }
    pub fn display() -> Display {
        Display {
            id: 1,
            bounds: Bounds {
                left: -1600,
                top: -900,
                width: 1600,
                height: 900,
            },
            builtin: false,
            main: true,
        }
    }

    #[test]
    fn fractional_motion_deltas_match_total_cursor_displacement() {
        for left in [-1600, 100] {
            for speed in [0.25, 0.5, 1.25] {
                let mut screen = display();
                screen.bounds.left = left;
                let mut i = Input::new(Recording::default(), screen, speed, true);
                i.apply(Message::Enter { epoch: 1, y: 32768 }).unwrap();
                let start = (i.x, i.y);
                i.injector.0.clear();
                for _ in 0..100 {
                    i.apply(Message::Move { dx: -1, dy: 1 }).unwrap();
                }
                let (dx, dy) = i.injector.0.iter().fold((0, 0), |(x, y), e| match e {
                    Event::Motion { dx, dy, .. } => (x + dx, y + dy),
                    _ => panic!("unexpected event"),
                });
                assert_eq!(dx, i.x.round() as i64 - start.0.round() as i64);
                assert_eq!(dy, i.y.round() as i64 - start.1.round() as i64);
                assert_eq!(i.x, start.0 - 100.0 * speed);
                assert_eq!(i.y, start.1 + 100.0 * speed);
            }
        }
    }

    #[test]
    fn queued_hover_reduces_posts_but_preserves_every_samples_position_effect() {
        for count in [2000, 8000] {
            let mut i = Input::new(Recording::default(), display(), 0.25, true);
            let mut direct = Input::new(Recording::default(), display(), 0.25, true);
            for input in [&mut i, &mut direct] {
                input.apply(Message::Enter { epoch: 1, y: 32768 }).unwrap();
                input.injector.0.clear();
            }
            for n in 0..count {
                // Alternating directions also exercise fractional accumulation.
                let message = Message::Move {
                    dx: if n % 4 == 3 { 1 } else { -1 },
                    dy: 0,
                };
                i.apply_queued(message).unwrap();
                direct.apply(message).unwrap();
                if (n + 1) % shared::connection::MAX_BATCH_MESSAGES == 0 {
                    i.flush_motion().unwrap();
                }
            }
            i.flush_motion().unwrap();
            assert_eq!((i.x, i.y, i.epoch), (direct.x, direct.y, direct.epoch));
            assert_eq!(
                i.injector.0.len(),
                count.div_ceil(shared::connection::MAX_BATCH_MESSAGES)
            );
            assert_eq!(direct.injector.0.len(), count);
            let sum = |events: &[Event]| {
                events
                    .iter()
                    .map(|e| match e {
                        Event::Motion { dx, .. } => *dx,
                        _ => 0,
                    })
                    .sum::<i64>()
            };
            assert_eq!(sum(&i.injector.0), sum(&direct.injector.0));
        }
    }

    #[test]
    fn queued_motion_flushes_at_barriers_and_keeps_the_entire_drag_path() {
        let mut i = Input::new(Recording::default(), display(), 1.0, true);
        i.apply(Message::Enter { epoch: 1, y: 0 }).unwrap();
        i.injector.0.clear();
        for message in [
            Message::Move { dx: -1, dy: 0 },
            Message::Move { dx: -2, dy: 0 },
            Message::Key {
                vk: 16,
                scan: 42,
                extended: false,
                down: true,
            },
            Message::Move { dx: -3, dy: 0 },
            Message::Button {
                button: 0,
                down: true,
            },
            Message::Move { dx: -4, dy: 1 },
            Message::Move { dx: 2, dy: 1 },
            Message::Button {
                button: 0,
                down: false,
            },
            Message::Move { dx: -5, dy: 0 },
            Message::Scroll {
                horizontal: false,
                delta: 120,
            },
            Message::Move { dx: -6, dy: 0 },
            Message::ReleaseAll,
        ] {
            i.apply_queued(message).unwrap();
        }
        let events = &i.injector.0;
        assert_eq!(events.len(), 11);
        assert!(matches!(
            events[0],
            Event::Motion {
                x: -4.0,
                dx: -3,
                flags: 0,
                ..
            }
        ));
        assert!(matches!(events[1], Event::Key { down: true, .. }));
        assert!(matches!(
            events[2],
            Event::Motion {
                x: -7.0,
                dx: -3,
                flags,
                ..
            } if flags == keyboard::modifier(56)
        ));
        assert!(matches!(
            events[3],
            Event::Button {
                x: -7.0,
                down: true,
                ..
            }
        ));
        assert!(matches!(
            events[4],
            Event::Motion {
                x: -11.0,
                drag: Some(0),
                ..
            }
        ));
        assert!(matches!(
            events[5],
            Event::Motion {
                x: -9.0,
                drag: Some(0),
                ..
            }
        ));
        assert!(matches!(
            events[6],
            Event::Button {
                x: -9.0,
                down: false,
                ..
            }
        ));
        assert!(matches!(
            events[7],
            Event::Motion {
                x: -14.0,
                drag: None,
                ..
            }
        ));
        assert!(matches!(events[8], Event::Scroll { .. }));
        assert!(matches!(events[9], Event::Motion { x: -20.0, .. }));
        assert!(matches!(events[10], Event::Key { down: false, .. }));
        assert_eq!(i.held_count(), 0);
        assert!(i.pending_motion.is_none());
    }

    #[test]
    fn queued_motion_cannot_hide_clamping_or_a_return_crossing() {
        let mut i = Input::new(Recording::default(), display(), 1.0, true);
        i.apply(Message::Enter { epoch: 7, y: 0 }).unwrap();
        i.apply_queued(Message::Move {
            dx: i32::MIN,
            dy: i32::MAX,
        })
        .unwrap();
        i.apply_queued(Message::Move { dx: 1, dy: -1 }).unwrap();
        i.flush_motion().unwrap();
        assert_eq!((i.x, i.y), (-1599.0, -2.0));
        assert!(i
            .apply_queued(Message::Move { dx: 1599, dy: 0 })
            .unwrap()
            .is_some());
        // This reversal would conceal the return if raw deltas were summed first.
        i.apply_queued(Message::Move { dx: -1599, dy: 0 }).unwrap();
        i.flush_motion().unwrap();
        assert_eq!(i.epoch, None);
        assert_eq!((i.x, i.y), (-1599.0, -2.0));
    }

    #[test]
    fn lone_move_posts_immediately_and_stationary_reports_do_not_post() {
        let mut i = Input::new(Recording::default(), display(), 1.0, true);
        i.apply(Message::Enter { epoch: 1, y: 0 }).unwrap();
        i.injector.0.clear();
        i.apply(Message::Move { dx: -1, dy: 0 }).unwrap();
        assert_eq!(i.injector.0.len(), 1);
        assert!(i.pending_motion.is_none());
        i.apply(Message::Move { dx: 0, dy: 0 }).unwrap();
        i.apply(Message::Move { dx: 0, dy: -1 }).unwrap(); // top clamp
        assert_eq!(i.injector.0.len(), 1);
    }

    #[test]
    fn failed_pending_motion_still_releases_held_keys() {
        #[derive(Default)]
        struct Failing {
            recording: Recording,
            fail_motion: bool,
        }
        impl Injector for Failing {
            fn post(&mut self, event: Event) -> Result<()> {
                if self.fail_motion && matches!(event, Event::Motion { .. }) {
                    anyhow::bail!("synthetic motion failure");
                }
                self.recording.post(event)
            }
        }
        let mut i = Input::new(Failing::default(), display(), 1.0, true);
        i.apply(Message::Enter { epoch: 1, y: 0 }).unwrap();
        i.apply(Message::Key {
            vk: 16,
            scan: 42,
            extended: false,
            down: true,
        })
        .unwrap();
        i.apply_queued(Message::Move { dx: -10, dy: 0 }).unwrap();
        i.injector.fail_motion = true;
        assert!(i.apply_queued(Message::ReleaseAll).is_err());
        assert_eq!(i.held_count(), 0);
        assert_eq!(i.epoch, None);
        assert!(i.pending_motion.is_none());
        assert!(matches!(
            i.injector.recording.0.last(),
            Some(Event::Key {
                down: false,
                flags: 0,
                ..
            })
        ));
    }

    #[test]
    fn new_entry_flushes_old_motion_without_replaying_it() {
        let mut i = Input::new(Recording::default(), display(), 1.0, true);
        i.apply(Message::Enter { epoch: 1, y: 0 }).unwrap();
        i.injector.0.clear();
        i.apply_queued(Message::Move { dx: -10, dy: 0 }).unwrap();
        i.apply_queued(Message::Enter { epoch: 2, y: 65535 })
            .unwrap();
        i.flush_motion().unwrap();
        assert_eq!(i.epoch, Some(2));
        assert_eq!(i.injector.0.len(), 2);
        assert!(matches!(
            i.injector.0[0],
            Event::Motion {
                x: -11.0,
                y: -900.0,
                dx: -10,
                ..
            }
        ));
        assert!(matches!(
            i.injector.0[1],
            Event::Motion {
                x: -1.0,
                y: -1.0,
                dx: 0,
                ..
            }
        ));
        assert!(i.pending_motion.is_none());
    }

    #[test]
    fn scaled_negative_origin_drag_return_and_release_order() {
        let mut i = Input::new(Recording::default(), display(), 0.5, true);
        i.apply(Message::Enter { epoch: 8, y: 65535 }).unwrap();
        assert_eq!((i.x, i.y), (-1.0, -1.0));
        i.apply(Message::Key {
            vk: 0x5b,
            scan: 0x5b,
            extended: true,
            down: true,
        })
        .unwrap();
        i.apply(Message::Key {
            vk: 65,
            scan: 0x1e,
            extended: false,
            down: true,
        })
        .unwrap();
        i.apply(Message::Button {
            button: 0,
            down: true,
        })
        .unwrap();
        i.apply(Message::Move {
            dx: -100,
            dy: i32::MIN,
        })
        .unwrap();
        assert_eq!((i.x, i.y), (-51.0, -900.0));
        assert!(matches!(
            i.injector.0.last(),
            Some(Event::Motion {
                drag: Some(0),
                flags: keyboard::LEFT_COMMAND,
                ..
            })
        ));
        let reply = i.apply(Message::Move { dx: 101, dy: 0 }).unwrap();
        assert!(reply == Some(Message::Return { epoch: 8, y: 0 }));
        assert_eq!(i.held_count(), 0);
        let events = &i.injector.0;
        assert!(matches!(
            events[events.len() - 3],
            Event::Key {
                code: 0,
                down: false,
                flags: keyboard::LEFT_COMMAND,
                ..
            }
        ));
        assert!(matches!(
            events[events.len() - 2],
            Event::Key {
                code: 55,
                down: false,
                flags: 0,
                ..
            }
        ));
        let n = events.len();
        i.apply(Message::Key {
            vk: 65,
            scan: 0x1e,
            extended: false,
            down: true,
        })
        .unwrap();
        assert_eq!(i.injector.0.len(), n);
    }
    #[test]
    fn repeats_two_shifts_scroll_and_new_entry() {
        let mut i = Input::new(Recording::default(), display(), 1.0, true);
        i.apply(Message::Enter { epoch: 1, y: 32768 }).unwrap();
        for (scan, down) in [(0x2a, true), (0x36, true), (0x2a, false)] {
            i.apply(Message::Key {
                vk: 0x10,
                scan,
                extended: false,
                down,
            })
            .unwrap();
        }
        assert!(matches!(
            i.injector.0.last(),
            Some(Event::Key {
                flags: keyboard::RIGHT_SHIFT,
                ..
            })
        ));
        for _ in 0..2 {
            i.apply(Message::Key {
                vk: 65,
                scan: 0x1e,
                extended: false,
                down: true,
            })
            .unwrap();
        }
        assert!(matches!(
            i.injector.0.last(),
            Some(Event::Key { repeat: true, .. })
        ));
        i.apply(Message::Scroll {
            horizontal: true,
            delta: 120,
        })
        .unwrap();
        assert!(matches!(
            i.injector.0.last(),
            Some(Event::Scroll {
                points: -30,
                flags: keyboard::RIGHT_SHIFT,
                ..
            })
        ));
        i.apply(Message::Enter { epoch: 2, y: 0 }).unwrap();
        assert_eq!(i.held_count(), 0);
        assert_eq!(i.epoch, Some(2));
        i.apply(Message::Move {
            dx: i32::MIN,
            dy: i32::MAX,
        })
        .unwrap();
        assert_eq!((i.x, i.y), (-1600.0, -1.0));
    }
    #[test]
    fn display_fallback() {
        let main = display();
        let external = Display {
            id: 2,
            main: false,
            ..main
        };
        let builtin = Display {
            id: 3,
            builtin: true,
            main: false,
            ..main
        };
        let all = [main, external, builtin];
        assert_eq!(select_display(&all, Some(2)), Some(external));
        assert_eq!(select_display(&all, Some(3)), Some(main));
        assert_eq!(select_display(&all, Some(99)), Some(main));
    }

    #[test]
    fn high_resolution_wheels_caps_and_all_buttons_balance() {
        let mut i = Input::new(Recording::default(), display(), 1.0, true);
        i.apply(Message::Enter { epoch: 1, y: 0 }).unwrap();
        for _ in 0..4 {
            for horizontal in [false, true] {
                i.apply(Message::Scroll {
                    horizontal,
                    delta: 1,
                })
                .unwrap();
            }
        }
        let scrolls: Vec<_> = i
            .injector
            .0
            .iter()
            .filter_map(|e| match e {
                Event::Scroll {
                    horizontal, points, ..
                } => Some((*horizontal, *points)),
                _ => None,
            })
            .collect();
        assert_eq!(scrolls, [(false, 1), (true, -1)]);
        for down in [true, true, false] {
            i.apply(Message::Key {
                vk: 0x14,
                scan: 0x3a,
                extended: false,
                down,
            })
            .unwrap();
        }
        for button in 0..5 {
            i.apply(Message::Button { button, down: true }).unwrap();
        }
        assert_eq!(i.held_count(), 5);
        i.apply(Message::ReleaseAll).unwrap();
        assert_eq!(i.held_count(), 0);
        assert_eq!(
            i.injector
                .0
                .iter()
                .filter(|e| matches!(
                    e,
                    Event::Key {
                        code: 57,
                        down: true,
                        ..
                    }
                ))
                .count(),
            1
        );
        assert!(i.injector.0.iter().any(|e| matches!(
            e,
            Event::Key {
                code: 57,
                down: false,
                flags: 0,
                ..
            }
        )));
        for button in 0..5 {
            assert_eq!(i.injector.0.iter().filter(|e| matches!(e, Event::Button { button: b, down: false, flags: 0, .. } if *b == button)).count(), 1);
        }
    }

    #[test]
    fn native_failure_does_not_skip_other_releases() {
        struct Failing(Recording);
        impl Injector for Failing {
            fn post(&mut self, event: Event) -> Result<()> {
                if matches!(
                    event,
                    Event::Key {
                        code: 0,
                        down: false,
                        ..
                    }
                ) {
                    anyhow::bail!("synthetic allocation failure");
                }
                self.0.post(event)
            }
        }
        let mut i = Input::new(Failing(Recording::default()), display(), 1.0, true);
        i.apply(Message::Enter { epoch: 1, y: 0 }).unwrap();
        for (vk, scan) in [(0x5b, 0x5b), (65, 0x1e)] {
            i.apply(Message::Key {
                vk,
                scan,
                extended: vk == 0x5b,
                down: true,
            })
            .unwrap();
        }
        i.apply(Message::Button {
            button: 0,
            down: true,
        })
        .unwrap();
        assert!(i.release().is_err());
        assert_eq!(i.epoch, None);
        assert_eq!(i.held_count(), 0);
        assert!(matches!(
            i.injector.0 .0.last(),
            Some(Event::Button {
                button: 0,
                down: false,
                ..
            })
        ));
        assert!(i.injector.0 .0.iter().any(|e| matches!(
            e,
            Event::Key {
                code: 55,
                down: false,
                flags: 0,
                ..
            }
        )));
    }

    #[test]
    fn aliased_keys_do_not_release_each_other() {
        let mut i = Input::new(Recording::default(), display(), 1.0, true);
        i.apply(Message::Enter { epoch: 1, y: 0 }).unwrap();
        for (vk, scan, down) in [(0x2c, 0x37, true), (0x7c, 0x64, true), (0x2c, 0x37, false)] {
            i.apply(Message::Key {
                vk,
                scan,
                extended: true,
                down,
            })
            .unwrap();
        }
        assert_eq!(i.held_count(), 1);
        assert_eq!(
            i.injector
                .0
                .iter()
                .filter(|e| matches!(
                    e,
                    Event::Key {
                        code: 105,
                        down: false,
                        ..
                    }
                ))
                .count(),
            0
        );
        i.apply(Message::Key {
            vk: 0x7c,
            scan: 0x64,
            extended: true,
            down: false,
        })
        .unwrap();
        assert_eq!(i.held_count(), 0);
        assert!(matches!(
            i.injector.0.last(),
            Some(Event::Key {
                code: 105,
                down: false,
                ..
            })
        ));
    }

    #[test]
    fn virtual_key_change_does_not_strand_a_physical_key() {
        let mut i = Input::new(Recording::default(), display(), 1.0, true);
        i.apply(Message::Enter { epoch: 1, y: 0 }).unwrap();
        i.apply(Message::Key {
            vk: 0x68,
            scan: 0x48,
            extended: false,
            down: true,
        })
        .unwrap();
        // Windows reports keypad Up rather than keypad 8 with NumLock off.
        i.apply(Message::Key {
            vk: 0x26,
            scan: 0x48,
            extended: false,
            down: false,
        })
        .unwrap();
        assert_eq!(i.held_count(), 0);
        assert!(matches!(
            i.injector.0.last(),
            Some(Event::Key {
                code: 91,
                down: false,
                ..
            })
        ));
    }

    #[test]
    fn only_right_crossing_returns_and_entry_keeps_vertical_position() {
        let mut i = Input::new(Recording::default(), display(), 1.0, true);
        for y in [0, 32768, 65535] {
            i.apply(Message::Enter { epoch: 4, y }).unwrap();
            assert_eq!(i.x, -1.0);
            assert_eq!(i.y, f64::from(display().bounds.denormalize_y(y)));
            assert!(matches!(
                i.injector.0.last(),
                Some(Event::Motion {
                    x: -1.0,
                    drag: None,
                    ..
                })
            ));
            assert!(i
                .apply(Message::Move {
                    dx: i32::MIN,
                    dy: 0
                })
                .unwrap()
                .is_none());
            assert_eq!(i.x, -1600.0);
            assert_eq!(i.epoch, Some(4));
            assert!(i
                .apply(Message::Move { dx: 1599, dy: 0 })
                .unwrap()
                .is_none());
            assert_eq!(i.x, -1.0);
            assert!(i.apply(Message::Move { dx: 0, dy: 0 }).unwrap().is_none());
            let reply = i.apply(Message::Move { dx: 1, dy: 0 }).unwrap();
            assert!(
                matches!(reply, Some(Message::Return { epoch: 4, y: returned }) if returned.abs_diff(y) < 100)
            );
        }
    }
}
