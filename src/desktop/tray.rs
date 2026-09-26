use eframe::egui::{Context, ViewportCommand};
use tray_icon::{
    Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent,
    menu::{Menu, MenuEvent, MenuItem},
};

use crate::{Result, i18n::Language};

/// Keeps the native icon and its menu alive on the desktop event-loop thread.
pub(super) struct Tray {
    icon: TrayIcon,
    show: MenuItem,
    quit: MenuItem,
    language: Language,
    quitting: bool,
}

impl Tray {
    pub fn new(language: Language) -> Result<Self> {
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
        for event in MenuEvent::receiver().try_iter() {
            if event.id == *self.show.id() {
                reveal(context);
            } else if event.id == *self.quit.id() {
                self.quitting = true;
                context.send_viewport_cmd(ViewportCommand::Close);
            }
        }
        for event in TrayIconEvent::receiver().try_iter() {
            if let TrayIconEvent::Click {
                id,
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
                && id == *self.icon.id()
            {
                reveal(context);
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

fn reveal(context: &Context) {
    context.send_viewport_cmd(ViewportCommand::Visible(true));
    context.send_viewport_cmd(ViewportCommand::Minimized(false));
    context.send_viewport_cmd(ViewportCommand::Focus);
    context.request_repaint();
}

pub(super) fn hide(context: &Context) {
    context.send_viewport_cmd(ViewportCommand::Visible(false));
    context.send_viewport_cmd(ViewportCommand::Minimized(false));
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
    fn tray_actions_restore_hidden_windows_and_only_hide_explicitly() {
        let context = Context::default();
        let mut hidden = context.run_ui(Default::default(), |ui| hide(ui.ctx()));
        hidden.textures_delta.clear();
        assert!(
            hidden.viewport_output[&eframe::egui::ViewportId::ROOT]
                .commands
                .contains(&ViewportCommand::Visible(false))
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
