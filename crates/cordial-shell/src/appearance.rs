//! The launcher's obsidian palette. The runtime keeps the desktop theme.

use libadwaita as adw;
use libadwaita::gtk;
use libadwaita::prelude::*;

const SHEET: &str = include_str!("appearance.css");

pub fn apply(window: &gtk::Window) {
    let manager = adw::StyleManager::default();
    manager.set_color_scheme(adw::ColorScheme::ForceDark);
    window.add_css_class("vanta-launcher");

    let css = gtk::CssProvider::new();
    css.load_from_string(if manager.is_high_contrast() { "" } else { SHEET });
    gtk::style_context_add_provider_for_display(
        &WidgetExt::display(window),
        &css,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
    // A running launcher must honour an accessibility change, not just one
    // already enabled at startup. Native high-contrast controls win outright.
    manager.connect_high_contrast_notify(move |manager| {
        css.load_from_string(if manager.is_high_contrast() { "" } else { SHEET });
    });
}
