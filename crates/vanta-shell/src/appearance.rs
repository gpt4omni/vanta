//! The launcher's obsidian palette. The runtime keeps the desktop theme.
//!
//! Only applied when dark mode is active and high-contrast accessibility mode
//! is off, so light-theme users and high-contrast users keep native Adwaita
//! controls without forcing `ColorScheme::ForceDark` over `ShellConfig`.

use libadwaita as adw;
use libadwaita::gtk;
use libadwaita::prelude::*;

const SHEET: &str = include_str!("appearance.css");

fn use_obsidian(manager: &adw::StyleManager) -> bool {
    manager.is_dark() && !manager.is_high_contrast()
}

fn sync(window: &gtk::Window, css: &gtk::CssProvider, manager: &adw::StyleManager) {
    if use_obsidian(manager) {
        window.add_css_class("vanta-launcher");
        css.load_from_string(SHEET);
    } else {
        window.remove_css_class("vanta-launcher");
        css.load_from_string("");
    }
}

pub fn apply(window: &gtk::Window) {
    let manager = adw::StyleManager::default();
    let css = gtk::CssProvider::new();
    sync(window, &css, &manager);
    gtk::style_context_add_provider_for_display(
        &WidgetExt::display(window),
        &css,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );

    // Follow both dark-mode and high-contrast changes live. Hold a weak window
    // reference so closing the launcher never leaks the window through the
    // global `StyleManager`.
    let weak_hc = window.downgrade();
    let css_hc = css.clone();
    manager.connect_high_contrast_notify(move |manager| {
        if let Some(window) = weak_hc.upgrade() {
            sync(&window, &css_hc, manager);
        }
    });

    let weak_dark = window.downgrade();
    manager.connect_dark_notify(move |manager| {
        if let Some(window) = weak_dark.upgrade() {
            sync(&window, &css, manager);
        }
    });
}
