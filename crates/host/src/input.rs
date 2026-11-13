//! Эмуляция мыши и клавиатуры на macOS через CGEvent.
//! Нужно разрешение: Системные настройки -> Конфиденциальность и безопасность -> Универсальный доступ
//! (для программы, из которой запущен remote-host, например Terminal).

#[cfg(target_os = "macos")]
mod imp {
    use core_graphics::display::CGDisplay;
    use core_graphics::event::{
        CGEvent, CGEventFlags, CGEventTapLocation, CGEventType, CGMouseButton, EventField,
        ScrollEventUnit,
    };
    use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
    use core_graphics::geometry::CGPoint;
    use proto::{Button, Message};
    use std::collections::HashSet;
    use std::time::{Duration, Instant};

    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        fn AXIsProcessTrusted() -> bool;
    }

    pub struct Injector {
        source: CGEventSource,
        origin: (f64, f64),
        size: (f64, f64),
        pos: CGPoint,
        buttons: [bool; 3],
        keys_down: HashSet<u16>,
        last_click: Option<(Instant, Button, CGPoint)>,
        clicks: i64,
    }

    fn modifier_flag(code: u16) -> Option<CGEventFlags> {
        match code {
            56 | 60 => Some(CGEventFlags::CGEventFlagShift),
            59 | 62 => Some(CGEventFlags::CGEventFlagControl),
            58 | 61 => Some(CGEventFlags::CGEventFlagAlternate),
            55 | 54 => Some(CGEventFlags::CGEventFlagCommand),
            _ => None,
        }
    }

    impl Injector {
        pub fn new() -> Result<Injector, String> {
            let trusted = unsafe { AXIsProcessTrusted() };
            if !trusted {
                eprintln!(
                    "ВНИМАНИЕ: нет разрешения «Универсальный доступ» — события мыши и клавиатуры будут игнорироваться.\n\
                     Включите его для этого терминала в Системные настройки -> Конфиденциальность и безопасность."
                );
            }
            let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState)
                .map_err(|_| "не удалось создать источник событий".to_string())?;
            let b = CGDisplay::main().bounds();
            Ok(Injector {
                source,
                origin: (b.origin.x, b.origin.y),
                size: (b.size.width, b.size.height),
                pos: CGPoint::new(b.origin.x + b.size.width / 2.0, b.origin.y + b.size.height / 2.0),
                buttons: [false; 3],
                keys_down: HashSet::new(),
                last_click: None,
                clicks: 0,
            })
        }

        fn flags(&self) -> CGEventFlags {
            let mut f = CGEventFlags::empty();
            for k in &self.keys_down {
                if let Some(m) = modifier_flag(*k) {
                    f |= m;
                }
            }
            f
        }

        fn post(&self, ev: CGEvent) {
            ev.set_flags(self.flags());
            ev.post(CGEventTapLocation::HID);
        }

        pub fn handle(&mut self, msg: &Message) {
            match *msg {
                Message::MouseMove { x, y } => self.mouse_move(x, y),
                Message::MouseButton { button, down } => self.mouse_button(button, down),
                Message::Scroll { dx, dy } => self.scroll(dx, dy),
                Message::Key { code, down } => self.key(code, down),
                _ => {}
            }
        }

        fn mouse_move(&mut self, nx: f32, ny: f32) {
            let nx = (nx as f64).clamp(0.0, 1.0);
            let ny = (ny as f64).clamp(0.0, 1.0);
            // 0.5 px от правого/нижнего края, чтобы курсор не выходил за экран
            let x = self.origin.0 + (nx * (self.size.0 - 1.0)).max(0.0);
            let y = self.origin.1 + (ny * (self.size.1 - 1.0)).max(0.0);
            self.pos = CGPoint::new(x, y);

            let (ty, btn) = if self.buttons[0] {
                (CGEventType::LeftMouseDragged, CGMouseButton::Left)
            } else if self.buttons[1] {
                (CGEventType::RightMouseDragged, CGMouseButton::Right)
            } else if self.buttons[2] {
                (CGEventType::OtherMouseDragged, CGMouseButton::Center)
            } else {
                (CGEventType::MouseMoved, CGMouseButton::Left)
            };
            if let Ok(ev) = CGEvent::new_mouse_event(self.source.clone(), ty, self.pos, btn) {
                self.post(ev);
            }
        }

        fn mouse_button(&mut self, button: Button, down: bool) {
            let (ty, btn) = match (button, down) {
                (Button::Left, true) => (CGEventType::LeftMouseDown, CGMouseButton::Left),
                (Button::Left, false) => (CGEventType::LeftMouseUp, CGMouseButton::Left),
                (Button::Right, true) => (CGEventType::RightMouseDown, CGMouseButton::Right),
                (Button::Right, false) => (CGEventType::RightMouseUp, CGMouseButton::Right),
                (Button::Middle, true) => (CGEventType::OtherMouseDown, CGMouseButton::Center),
                (Button::Middle, false) => (CGEventType::OtherMouseUp, CGMouseButton::Center),
            };
            self.buttons[button.index()] = down;

            // счётчик кликов для двойного/тройного клика
            if down {
                let now = Instant::now();
                let continues = match self.last_click {
                    Some((t, b, p)) => {
                        b == button
                            && now.duration_since(t) < Duration::from_millis(400)
                            && (p.x - self.pos.x).abs() < 5.0
                            && (p.y - self.pos.y).abs() < 5.0
                    }
                    None => false,
                };
                self.clicks = if continues { self.clicks + 1 } else { 1 };
                self.last_click = Some((now, button, self.pos));
            }

            if let Ok(ev) = CGEvent::new_mouse_event(self.source.clone(), ty, self.pos, btn) {
                ev.set_integer_value_field(EventField::MOUSE_EVENT_CLICK_STATE, self.clicks);
                self.post(ev);
            }
        }

        fn scroll(&mut self, dx: i16, dy: i16) {
            if let Ok(ev) = CGEvent::new_scroll_event(
                self.source.clone(),
                ScrollEventUnit::PIXEL,
                2,
                dy as i32,
                dx as i32,
                0,
            ) {
                self.post(ev);
            }
        }

        fn key(&mut self, code: u16, down: bool) {
            if down {
                self.keys_down.insert(code);
            } else {
                self.keys_down.remove(&code);
            }
            if let Ok(ev) = CGEvent::new_keyboard_event(self.source.clone(), code, down) {
                self.post(ev);
            }
        }

        /// При отключении клиента отпускаем всё, что осталось зажатым.
        pub fn release_all(&mut self) {
            let held: Vec<u16> = self.keys_down.iter().copied().collect();
            for code in held {
                self.key(code, false);
            }
            for (i, b) in [Button::Left, Button::Right, Button::Middle].into_iter().enumerate() {
                if self.buttons[i] {
                    self.mouse_button(b, false);
                }
            }
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use proto::Message;

    /// Заглушка для сборки и проверки не на macOS: ввод только печатается.
    pub struct Injector {
        warned: bool,
    }

    impl Injector {
        pub fn new() -> Result<Injector, String> {
            eprintln!("ВНИМАНИЕ: хост собран не для macOS, ввод не эмулируется");
            Ok(Injector { warned: false })
        }

        pub fn handle(&mut self, msg: &Message) {
            if !self.warned {
                eprintln!("получено событие ввода (игнорируется): {msg:?}");
                self.warned = true;
            }
        }

        pub fn release_all(&mut self) {}
    }
}

pub use imp::Injector;
