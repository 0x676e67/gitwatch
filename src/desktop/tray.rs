use std::sync::{
    Mutex, Once,
    mpsc::{self, Receiver, Sender},
};

use anyhow::ensure;
use eframe::egui::{Context, ViewportCommand};
use tray_icon::{
    Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent,
    menu::{Menu, MenuEvent, MenuItem},
};

use crate::{Result, i18n::Language};

/// Keeps the native icon and its menu alive on the desktop event-loop thread.
pub(super) struct Tray {
    events: Events,
    icon: TrayIcon,
    show: MenuItem,
    quit: MenuItem,
    language: Language,
    quitting: bool,
}

enum Event {
    Menu(MenuEvent),
    Icon(TrayIconEvent),
}

/// Routes process-wide native callbacks to the currently open desktop.
struct Events {
    receiver: Receiver<Event>,
}

static EVENT_TARGET: Mutex<Option<(Sender<Event>, Context)>> = Mutex::new(None);

// ===== impl Tray =====

impl Tray {
    pub fn new(language: Language, context: &Context) -> Result<Self> {
        let events = Events::new(context)?;
        let menu = Menu::new();
        let show = MenuItem::new(language.text("Show window"), true, None);
        let quit = MenuItem::new(language.text("Quit"), true, None);
        menu.append_items(&[&show, &quit])?;
        // Creation runs inside eframe's app callback, after the native loop starts.
        // Linux KSNI reports missing hosts instead of requiring a GTK event loop.
        // https://docs.rs/tray-icon/0.25.1/tray_icon/
        let icon = TrayIconBuilder::new()
            .with_tooltip("gitwatch")
            .with_icon(icon()?)
            .with_menu(Box::new(menu))
            .with_menu_on_left_click(false)
            .build()?;
        Ok(Self {
            events,
            icon,
            show,
            quit,
            language,
            quitting: false,
        })
    }

    pub fn quitting(&self) -> bool {
        self.quitting
    }

    /// Called from App::logic even while the main window is hidden.
    pub fn poll(&mut self, context: &Context) {
        for event in self.events.receiver.try_iter() {
            match event {
                Event::Menu(event) if event.id == *self.show.id() => reveal(context),
                Event::Menu(event) if event.id == *self.quit.id() => {
                    self.quitting = true;
                    context.send_viewport_cmd(ViewportCommand::Close);
                }
                Event::Icon(TrayIconEvent::Click {
                    id,
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                }) if id == *self.icon.id() => reveal(context),
                _ => {}
            }
        }
    }

    pub fn set_language(&mut self, language: Language) {
        if self.language != language {
            self.show.set_text(language.text("Show window"));
            self.quit.set_text(language.text("Quit"));
            self.language = language;
        }
    }
}

// ===== impl Events =====

impl Events {
    fn new(context: &Context) -> Result<Self> {
        let (sender, receiver) = mpsc::channel();
        let mut target = EVENT_TARGET
            .lock()
            .map_err(|_| anyhow::anyhow!("Tray event lock poisoned"))?;
        ensure!(target.is_none(), "A desktop tray is already active");
        *target = Some((sender, context.clone()));
        drop(target);
        // These handlers are installed once by the upstream OnceCell API.
        // Winit must be awakened when a tray event arrives, even with no visible window.
        // https://docs.rs/tray-icon/0.25.1/tray_icon/#note-for-winit-or-tao-users
        static HANDLERS: Once = Once::new();
        HANDLERS.call_once(|| {
            MenuEvent::set_event_handler(Some(Self::menu));
            TrayIconEvent::set_event_handler(Some(Self::icon));
        });
        Ok(Self { receiver })
    }

    fn menu(event: MenuEvent) {
        Self::send(Event::Menu(event));
    }

    fn icon(event: TrayIconEvent) {
        if matches!(
            event,
            TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            }
        ) {
            Self::send(Event::Icon(event));
        }
    }

    fn send(event: Event) {
        let context = EVENT_TARGET.lock().ok().and_then(|target| {
            let (sender, context) = target.as_ref()?;
            sender.send(event).ok()?;
            Some(context.clone())
        });
        if let Some(context) = context {
            context.request_repaint();
        }
    }
}

impl Drop for Events {
    fn drop(&mut self) {
        if let Ok(mut target) = EVENT_TARGET.lock() {
            *target = None;
        }
    }
}

fn reveal(context: &Context) {
    context.send_viewport_cmd(ViewportCommand::Visible(true));
    context.send_viewport_cmd(ViewportCommand::Minimized(false));
    context.send_viewport_cmd(ViewportCommand::Focus);
    context.request_repaint();
}

pub(super) fn hide(context: &Context) {
    context.send_viewport_cmd(ViewportCommand::Visible(false));
}

pub(super) fn close(context: &Context, quitting: bool) {
    if !quitting && context.input(|input| input.viewport().close_requested()) {
        context.send_viewport_cmd(ViewportCommand::CancelClose);
        hide(context);
    }
}

fn icon() -> Result<Icon> {
    let mut rgba = Vec::with_capacity(32 * 32 * 4);
    for y in 0_i32..32 {
        for x in 0_i32..32 {
            let node = [(10, 8), (10, 24), (23, 8)]
                .iter()
                .any(|(cx, cy)| (x - cx).pow(2) + (y - cy).pow(2) <= 12);
            let stem = (9..=11).contains(&x) && (8..=24).contains(&y);
            let branch = (10..=23).contains(&x) && (y - (31 - x)).abs() <= 1;
            rgba.extend_from_slice(if node || stem || branch {
                &[113, 219, 196, 255]
            } else {
                &[16, 23, 33, 255]
            });
        }
    }
    Ok(Icon::from_rgba(rgba, 32, 32)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_events_wake_the_current_desktop_without_pointer_input() {
        for _ in 0..2 {
            let context = Context::default();
            let events = Events::new(&context).unwrap();
            assert!(Events::new(&context).is_err());
            let (sender, receiver) = mpsc::channel();
            context.set_request_repaint_callback(move |request| {
                sender.send(request.delay).unwrap();
            });
            for menu in [true, false] {
                for _ in 0..3 {
                    let mut output = context.run_ui(Default::default(), |_| {});
                    output.textures_delta.clear();
                }
                while receiver.try_recv().is_ok() {}
                if menu {
                    Events::menu(MenuEvent { id: "quit".into() });
                } else {
                    Events::icon(TrayIconEvent::Click {
                        id: "gitwatch".into(),
                        position: (0.0, 0.0).into(),
                        rect: Default::default(),
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                    });
                }
                assert_eq!(receiver.try_recv().unwrap(), std::time::Duration::ZERO);
                assert!(matches!(
                    (menu, events.receiver.try_recv().unwrap()),
                    (true, Event::Menu(_)) | (false, Event::Icon(_))
                ));
            }
            drop(events);
            Events::menu(MenuEvent { id: "quit".into() });
            assert!(receiver.try_recv().is_err());
        }
    }

    #[test]
    fn tray_actions_restore_hidden_windows_and_only_hide_explicitly() {
        let context = Context::default();
        let mut hidden = context.run_ui(Default::default(), |ui| hide(ui.ctx()));
        hidden.textures_delta.clear();
        assert!(
            hidden.viewport_output[&eframe::egui::ViewportId::ROOT]
                .commands
                .contains(&ViewportCommand::Visible(false))
        );
        assert!(
            !hidden.viewport_output[&eframe::egui::ViewportId::ROOT]
                .commands
                .contains(&ViewportCommand::Minimized(false))
        );
        let mut shown = context.run_ui(Default::default(), |ui| reveal(ui.ctx()));
        shown.textures_delta.clear();
        let commands = &shown.viewport_output[&eframe::egui::ViewportId::ROOT].commands;
        assert!(commands.contains(&ViewportCommand::Visible(true)));
        assert!(commands.contains(&ViewportCommand::Minimized(false)));
        assert!(commands.contains(&ViewportCommand::Focus));
        for quitting in [false, true] {
            let mut input = eframe::egui::RawInput::default();
            input
                .viewports
                .get_mut(&eframe::egui::ViewportId::ROOT)
                .unwrap()
                .events
                .push(eframe::egui::ViewportEvent::Close);
            let mut output = context.run_ui(input, |ui| close(ui.ctx(), quitting));
            output.textures_delta.clear();
            let commands = &output.viewport_output[&eframe::egui::ViewportId::ROOT].commands;
            assert_eq!(commands.contains(&ViewportCommand::CancelClose), !quitting);
            assert_eq!(
                commands.contains(&ViewportCommand::Visible(false)),
                !quitting
            );
        }
    }
}
