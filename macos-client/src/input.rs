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
    physical_keys: [Option<u16>; 256],
    buttons: [bool; 5],
    clicks: [i64; 5],
    last_click: [Option<(Instant, f64, f64)>; 5],
    caps: bool,
    scroll_remainder: [i32; 2],
    speed: f64,
    win_to_command: bool,
}
impl<I: Injector> Input<I> {
    pub fn new(injector: I, display: Display, speed: f64, win_to_command: bool) -> Self {
        Self {
            injector,
            display,
            epoch: None,
            x: f64::from(display.bounds.left),
            y: f64::from(display.bounds.top),
            keys: [false; 128],
            physical_keys: [None; 256],
            buttons: [false; 5],
            clicks: [0; 5],
            last_click: [None; 5],
            caps: false,
            scroll_remainder: [0; 2],
            speed,
            win_to_command,
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
        self.epoch = None;
        self.physical_keys.fill(None);
        let mut first_error = None;
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
        if let Message::Enter { epoch, y } = message {
            self.release()?;
            self.epoch = Some(epoch);
            self.x = f64::from(self.display.bounds.left);
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
                if next_x < f64::from(b.left) {
                    let reply = Message::Return {
                        epoch: self.epoch.unwrap(),
                        y: b.normalize_y(self.y.round() as i32),
                    };
                    self.release()?;
                    return Ok(Some(reply));
                }
                let old_x = self.x;
                self.x = next_x.min(f64::from(b.right()));
                self.injector.post(Event::Motion {
                    x: self.x,
                    y: self.y,
                    drag: self.buttons.iter().position(|b| *b).map(|b| b as u8),
                    dx: (self.x - old_x).round() as i64,
                    dy: (self.y - old_y).round() as i64,
                    flags: self.flags(),
                })?;
            }
            Message::Key {
                vk,
                scan,
                extended,
                down,
            } => {
                if let Some(code) = keyboard::translate(vk, scan, extended, self.win_to_command) {
                    // Distinguish generic left/right Windows modifiers and keep
                    // aliases (e.g. PrintScreen/F13) held until both are released.
                    let slot = usize::from(match code {
                        56 => 0xa0,
                        60 => 0xa1,
                        59 => 0xa2,
                        62 => 0xa3,
                        58 => 0xa4,
                        61 => 0xa5,
                        55 => 0x5b,
                        54 => 0x5c,
                        _ => vk,
                    });
                    ensure!(slot < 256, "invalid virtual key");
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
    fn scaled_negative_origin_drag_return_and_release_order() {
        let mut i = Input::new(Recording::default(), display(), 0.5, true);
        i.apply(Message::Enter { epoch: 8, y: 65535 }).unwrap();
        assert_eq!((i.x, i.y), (-1600.0, -1.0));
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
            dx: 100,
            dy: i32::MIN,
        })
        .unwrap();
        assert_eq!((i.x, i.y), (-1550.0, -900.0));
        assert!(matches!(
            i.injector.0.last(),
            Some(Event::Motion {
                drag: Some(0),
                flags: keyboard::LEFT_COMMAND,
                ..
            })
        ));
        let reply = i.apply(Message::Move { dx: -101, dy: 0 }).unwrap();
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
            dx: i32::MAX,
            dy: i32::MAX,
        })
        .unwrap();
        assert_eq!((i.x, i.y), (-1.0, -1.0));
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
}
