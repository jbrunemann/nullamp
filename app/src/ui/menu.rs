//! The native macOS menu bar: About and a shortcut to the config file.

/// What a menu click asks the app to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuAction {
    About,
    OpenConfig,
}

#[cfg(target_os = "macos")]
pub use imp::NativeMenu;

#[cfg(target_os = "macos")]
mod imp {
    use super::MenuAction;
    use crossbeam_channel::{Receiver, unbounded};
    use muda::{Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu};

    /// Owns the menu for the life of the app; dropping it would remove it from the menu bar.
    pub struct NativeMenu {
        _menu: Menu,
        actions: Receiver<MenuAction>,
    }

    impl NativeMenu {
        /// Replace the default menu bar. Must run on the main thread once the event loop is up
        /// (eframe's app creator qualifies); `ctx` is woken so a click is handled while idle.
        pub fn install(ctx: egui::Context) -> Option<NativeMenu> {
            let menu = Menu::new();
            let open_config = MenuItem::new("Open Config File", true, None);
            let about = MenuItem::new("About Nullamp", true, None);
            let app_menu = Submenu::with_items(
                "Nullamp",
                true,
                &[
                    &about,
                    &PredefinedMenuItem::separator(),
                    &open_config,
                    &PredefinedMenuItem::separator(),
                    &PredefinedMenuItem::services(None),
                    &PredefinedMenuItem::separator(),
                    &PredefinedMenuItem::hide(None),
                    &PredefinedMenuItem::hide_others(None),
                    &PredefinedMenuItem::show_all(None),
                    &PredefinedMenuItem::separator(),
                    &PredefinedMenuItem::quit(None),
                ],
            )
            .ok()?;
            let window_menu = Submenu::with_items("Window", true, &[&PredefinedMenuItem::minimize(None), &PredefinedMenuItem::maximize(None)]).ok()?;
            menu.append_items(&[&app_menu, &window_menu]).ok()?;
            menu.init_for_nsapp();
            window_menu.set_as_windows_menu_for_nsapp();

            let (tx, actions) = unbounded();
            let (about_id, open_config_id) = (about.id().clone(), open_config.id().clone());
            MenuEvent::set_event_handler(Some(move |ev: MenuEvent| {
                let action = if ev.id == about_id {
                    MenuAction::About
                } else if ev.id == open_config_id {
                    MenuAction::OpenConfig
                } else {
                    return;
                };
                let _ = tx.send(action);
                ctx.request_repaint();
            }));
            Some(NativeMenu { _menu: menu, actions })
        }

        pub fn poll(&self) -> Option<MenuAction> {
            self.actions.try_recv().ok()
        }
    }
}
