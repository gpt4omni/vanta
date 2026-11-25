//! The chooser — ADR-002's T1 surface, painted before the plugin host is up.
//!
//! It has to be real `AdwPreferencesGroup`/`AdwActionRow` widgets rather than
//! hand-drawn boxes, for the same reason the rest of this crate is libadwaita:
//! this is the first thing anyone launching Vanta sees, and getting the
//! chrome (row hover states, focus rings, spacing against the header bar) to
//! merely *look* like libadwaita by hand would be more work than using it.
//!
//! **One entry, and it works.** There used to be two, and the second — Roblox
//! Studio — did nothing when pressed, because ADR-002 puts Studio out of scope
//! for this runtime. A row that looks live and is not is the interface version
//! of a stub returning success, so it is gone rather than disabled. The
//! `EntrySource` seam below stays: it is where plugin-contributed entries will
//! arrive over `cap:core.launcher.register` once there is a plugin host to ask,
//! and core still resolves and launches the target itself.
//!
//! **One entry is a button; several are a list.** The single entry used to be
//! drawn the same way a list of them would be — an `AdwPreferencesGroup` titled
//! "Launch", holding one `AdwActionRow` subtitled "Start the client" with a
//! chevron after it. Three pieces of chrome around one verb, in a window whose
//! entire job is that verb, and the result read as a settings page rather than
//! a launcher. GNOME's HIG has a control for the one thing a window exists to
//! do and it is a `suggested-action` button, so that is what a lone entry
//! becomes. The list is still what more than one produces, because at that
//! point the user is choosing between them rather than confirming the only
//! option, and a row per entry is how that choice is shown.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;

use libadwaita as adw;
use libadwaita::gtk;
use libadwaita::gtk::{gio, glib};
use libadwaita::prelude::*;

use crate::install;
use crate::settings::persist;
use crate::shell_config::{GraphicsOptimization, ShellConfig};

/// One entry in the chooser.
///
/// The shape both sides will agree on once entries arrive from the plugin host
/// (ADR-002, "the plugin declares *what*, core decides *how*"). It does not
/// know or care where it came from.
pub struct Entry {
    pub id: String,
    pub title: String,
    pub subtitle: Option<String>,
    pub icon_name: Option<String>,
}

/// Where the chooser gets its entries.
pub trait EntrySource {
    fn entries(&self) -> Vec<Entry>;
}

/// The one thing this shell knows how to start.
pub const ROBLOX: &str = "roblox";

/// Core's own entry: the Roblox client, launched by `launch::spawn`.
pub struct VantaSource;

impl EntrySource for VantaSource {
    fn entries(&self) -> Vec<Entry> {
        vec![Entry {
            id: ROBLOX.into(),
            title: "Roblox".into(),
            // No second line. It said "Start the client", which is what the
            // control it sits on already says and what the window is for. The
            // field stays because a plugin contributing two entries may need it
            // to tell them apart; core's own entry does not.
            subtitle: None,
            icon_name: Some("applications-games-symbolic".into()),
        }]
    }
}

/// Builds the chooser: an `AdwStatusPage` when there is nothing to launch, a
/// single `suggested-action` button when there is one thing, and an
/// `AdwPreferencesGroup` of action rows when there is a choice to make.
///
/// The caller decides where this sits and how wide it is — see `window.rs`,
/// which clamps it and the profile row together so the two line up.
///
/// `on_activate` is ADR-002's T2 — the user picking an entry. It is handed the
/// entry's id and nothing else; deciding what that id means, and refusing it if
/// it means nothing, is core's job and lives in `window.rs`.
pub fn build(source: &dyn EntrySource, on_activate: impl Fn(&str) + Clone + 'static) -> gtk::Widget {
    let mut entries = source.entries();

    // Reachable from any source that returns nothing, which is what a
    // plugin-contributed set will be before a plugin host exists. There was an
    // `EmptySource` here to exercise it, whose comment claimed it was covered by
    // tests; this file has no tests, so it was an unused stand-in with a comment
    // that lied about it, and both are gone.
    if entries.is_empty() {
        let status = adw::StatusPage::builder()
            .icon_name("applications-games-symbolic")
            .title("Nothing to launch yet")
            .description("Plugins that contribute launcher entries will appear here.")
            .vexpand(true)
            .build();
        return status.upcast();
    }

    if entries.len() == 1 {
        return button(entries.remove(0), on_activate);
    }

    // No group title. "Launch" over a list of things to launch is a header
    // restating the only thing the window does, and the rows below it already
    // name themselves.
    let group = adw::PreferencesGroup::new();

    for entry in entries {
        let row = adw::ActionRow::builder().title(entry.title).activatable(true).build();
        if let Some(subtitle) = &entry.subtitle {
            row.set_subtitle(subtitle);
        }
        if let Some(icon_name) = &entry.icon_name {
            let image = gtk::Image::from_icon_name(icon_name);
            image.set_icon_size(gtk::IconSize::Large);
            row.add_prefix(&image);
        }
        row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));

        let id = entry.id.clone();
        let on_activate = on_activate.clone();
        row.connect_activated(move |_| on_activate(&id));

        group.add(&row);
    }

    group.upcast()
}

#[allow(dead_code)]
pub fn hero(launch: gtk::Widget) -> gtk::Widget {
    hero_with_controls(launch, None, None)
}

/// The launcher hero card: brand mark, wordmark, tagline, optional segmented
/// Launch Mode strip, primary Launch button, and optional Quick Status Strip.
pub fn hero_with_controls(
    launch: gtk::Widget,
    mode_controls: Option<&gtk::Widget>,
    status_strip: Option<&gtk::Widget>,
) -> gtk::Widget {
    let column = gtk::Box::new(gtk::Orientation::Vertical, 10);
    column.add_css_class("vanta-hero-card");
    column.add_css_class("vanta-hero");

    // Respect Frostbite branding and decode at exact 2x HiDPI (224x224 for the
    // 112px hero mark) instead of 512x512 (~5.2x less CPU/GPU memory).
    let brand = vanta_shell::branding::current();
    let svg_bytes: &'static [u8] = match brand {
        vanta_shell::branding::Brand::Vanta => include_bytes!("../../../packaging/mark.svg"),
        vanta_shell::branding::Brand::Frostbite => include_bytes!(
            "../../../packaging/icons/hicolor/scalable/apps/io.github.gpt4omni.Vanta.Frostbite.svg"
        ),
    };
    let bytes = glib::Bytes::from_static(svg_bytes);
    let stream = gio::MemoryInputStream::from_bytes(&bytes);
    let mark = match gtk::gdk_pixbuf::Pixbuf::from_stream_at_scale(
        &stream,
        224,
        224,
        true,
        gio::Cancellable::NONE,
    ) {
        Ok(pixbuf) => gtk::Image::from_paintable(Some(&gtk::gdk::Texture::for_pixbuf(&pixbuf))),
        Err(_) => gtk::Image::from_icon_name(brand.icon()),
    };
    mark.set_pixel_size(112);
    mark.set_margin_bottom(4);
    column.append(&mark);

    let title = gtk::Label::new(Some(brand.name()));
    title.add_css_class("vanta-wordmark");
    column.append(&title);

    let tagline = gtk::Label::new(Some("Roblox, native on Linux."));
    tagline.add_css_class("vanta-tagline");
    tagline.set_wrap(true);
    tagline.set_justify(gtk::Justification::Center);
    column.append(&tagline);

    if let Some(controls) = mode_controls {
        column.append(controls);
    }
    column.append(&launch);
    if let Some(strip) = status_strip {
        column.append(strip);
    }
    column.upcast()
}

/// The segmented Launch Mode selector inside the hero card (`Balanced`,
/// `Performance`, `Speed`) plus a compact badge when one of Settings'
/// specialized modes (`RobloxApp`, `MobileTier`, `FewerCores`) is selected.
pub struct LaunchModeStrip {
    pub widget: gtk::Widget,
    /// Re-read `ShellConfig.graphics_optimization_mode` and sync the toggles.
    pub sync: Rc<dyn Fn()>,
}

/// Label for the badge shown when the user picked a specialized mode in Settings.
fn specialized_mode_note(mode: GraphicsOptimization) -> Option<String> {
    match mode {
        GraphicsOptimization::Balanced
        | GraphicsOptimization::MoreCores
        | GraphicsOptimization::Speed => None,
        other => Some(format!("Settings mode: {}", other.short_label())),
    }
}

pub fn build_mode_strip(
    config: Rc<RefCell<ShellConfig>>,
    config_path: Rc<PathBuf>,
    on_change: impl Fn() + 'static,
) -> LaunchModeStrip {
    let container = gtk::Box::new(gtk::Orientation::Vertical, 6);

    let strip = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    strip.set_homogeneous(true);
    strip.add_css_class("vanta-mode-strip");

    let balanced_btn = gtk::ToggleButton::with_label("Balanced");
    balanced_btn.set_tooltip_text(Some(GraphicsOptimization::Balanced.description()));

    let perf_btn = gtk::ToggleButton::with_label("Performance");
    perf_btn.set_tooltip_text(Some(GraphicsOptimization::MoreCores.description()));

    let speed_btn = gtk::ToggleButton::with_label("Speed");
    speed_btn.add_css_class("vanta-mode-speed");
    speed_btn.set_tooltip_text(Some(GraphicsOptimization::Speed.description()));

    strip.append(&balanced_btn);
    strip.append(&perf_btn);
    strip.append(&speed_btn);
    container.append(&strip);

    let note = gtk::Label::new(None);
    note.add_css_class("vanta-mode-note");
    note.set_halign(gtk::Align::Center);
    note.set_visible(false);
    container.append(&note);

    let syncing = Rc::new(Cell::new(false));
    let on_change: Rc<dyn Fn()> = Rc::new(on_change);

    let sync: Rc<dyn Fn()> = {
        let config = config.clone();
        let syncing = syncing.clone();
        let balanced_btn = balanced_btn.clone();
        let perf_btn = perf_btn.clone();
        let speed_btn = speed_btn.clone();
        let note = note.clone();
        Rc::new(move || {
            let mode = config.borrow().graphics_optimization_mode;
            syncing.set(true);
            balanced_btn.set_active(mode == GraphicsOptimization::Balanced);
            perf_btn.set_active(mode == GraphicsOptimization::MoreCores);
            speed_btn.set_active(mode == GraphicsOptimization::Speed);
            syncing.set(false);

            match specialized_mode_note(mode) {
                Some(text) => {
                    note.set_text(&text);
                    note.set_tooltip_text(Some(mode.description()));
                    note.set_visible(true);
                }
                None => {
                    note.set_visible(false);
                }
            }
        })
    };

    for (btn, target_mode) in [
        (&balanced_btn, GraphicsOptimization::Balanced),
        (&perf_btn, GraphicsOptimization::MoreCores),
        (&speed_btn, GraphicsOptimization::Speed),
    ] {
        let config = config.clone();
        let config_path = config_path.clone();
        let syncing = syncing.clone();
        let sync = sync.clone();
        let on_change = on_change.clone();
        btn.connect_toggled(move |button| {
            if syncing.get() {
                return;
            }
            if !button.is_active() {
                // Keep segmented radio behaviour: clicking the active segment
                // again leaves it selected rather than clearing all segments.
                if config.borrow().graphics_optimization_mode == target_mode {
                    syncing.set(true);
                    button.set_active(true);
                    syncing.set(false);
                }
                return;
            }
            config.borrow_mut().graphics_optimization_mode = target_mode;
            persist(&config, &config_path);
            sync();
            on_change();
        });
    }

    sync();

    LaunchModeStrip { widget: container.upcast(), sync }
}

/// Compact status line underneath the Launch button showing active profile,
/// active launch mode, and installed Roblox build version.
pub struct StatusStrip {
    pub widget: gtk::Widget,
    /// Re-read profile, launch mode, and (asynchronously) installed Roblox build.
    pub refresh: Rc<dyn Fn()>,
}

/// Pure formatting for the Quick Status Strip.
fn format_status_strip(
    profile: &str,
    mode: GraphicsOptimization,
    build_label: Option<&str>,
) -> String {
    let build = build_label.unwrap_or("Download needed");
    format!("{profile}  ·  {}  ·  {build}", mode.short_label())
}

fn resolve_build_label(profile_name: &str) -> Option<String> {
    if std::env::var_os(install::APK_OVERRIDE).is_some() {
        return Some("Custom APK".to_string());
    }
    if let Ok(dir) = vanta_shell::profile::dir(profile_name) {
        if let Some(pin) = vanta_shell::profile::pinned_version(&dir) {
            return Some(format!("Roblox {pin}"));
        }
    }
    install::installed_version().map(|v| format!("Roblox {v}"))
}

pub fn build_status_strip(config: Rc<RefCell<ShellConfig>>) -> StatusStrip {
    let label = gtk::Label::new(None);
    label.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
    label.set_halign(gtk::Align::Center);
    label.set_justify(gtk::Justification::Center);

    let strip = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    strip.set_halign(gtk::Align::Center);
    strip.add_css_class("vanta-status-strip");
    strip.append(&label);

    let cached_build: Rc<RefCell<Option<Option<String>>>> = Rc::new(RefCell::new(None));
    let generation = Rc::new(Cell::new(0u64));

    let refresh: Rc<dyn Fn()> = {
        let config = config.clone();
        let label = label.clone();
        let cached_build = cached_build.clone();
        let generation = generation.clone();
        Rc::new(move || {
            let (profile, mode) = {
                let cfg = config.borrow();
                (cfg.profile.clone(), cfg.graphics_optimization_mode)
            };
            let initial_build = cached_build
                .borrow()
                .as_ref()
                .and_then(|opt| opt.as_deref())
                .map(str::to_owned);
            let placeholder = if cached_build.borrow().is_some() {
                format_status_strip(&profile, mode, initial_build.as_deref())
            } else {
                format_status_strip(&profile, mode, Some("Checking build…"))
            };
            label.set_text(&placeholder);

            let ticket = generation.get().wrapping_add(1);
            generation.set(ticket);

            let weak_label = label.downgrade();
            let config = config.clone();
            let cached_build = cached_build.clone();
            let generation = generation.clone();
            let profile_for_worker = profile.clone();
            glib::spawn_future_local(async move {
                let resolved = gio::spawn_blocking(move || resolve_build_label(&profile_for_worker))
                    .await
                    .ok()
                    .flatten();
                if generation.get() != ticket {
                    return;
                }
                *cached_build.borrow_mut() = Some(resolved.clone());
                if let Some(label) = weak_label.upgrade() {
                    let (profile, mode) = {
                        let cfg = config.borrow();
                        (cfg.profile.clone(), cfg.graphics_optimization_mode)
                    };
                    label.set_text(&format_status_strip(&profile, mode, resolved.as_deref()));
                }
            });
        })
    };

    refresh();

    StatusStrip { widget: strip.upcast(), refresh }
}

/// The lone entry, as the button the window exists for.
///
/// The entry's own icon goes inside the button rather than above it as an
/// ornament, so nothing here invents a picture: what is drawn is exactly what
/// the entry declared, and an entry with no icon simply gets a label. The
/// subtitle, if a plugin set one, becomes the tooltip — dropping it outright
/// would lose the only thing a contributed entry has to explain itself with,
/// and putting it under the label would rebuild the two-line row this replaced.
fn button(entry: Entry, on_activate: impl Fn(&str) + 'static) -> gtk::Widget {
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    content.set_halign(gtk::Align::Center);
    if let Some(icon_name) = &entry.icon_name {
        content.append(&gtk::Image::from_icon_name(icon_name));
    }
    content.append(&gtk::Label::new(Some(&entry.title)));

    let button = gtk::Button::builder().child(&content).build();
    button.add_css_class("suggested-action");
    button.add_css_class("pill");
    button.add_css_class("vanta-launch");
    if let Some(subtitle) = &entry.subtitle {
        button.set_tooltip_text(Some(subtitle));
    }

    let id = entry.id;
    button.connect_clicked(move |_| on_activate(&id));
    button.upcast()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn specialized_modes_show_a_note_and_primary_segments_do_not() {
        assert_eq!(specialized_mode_note(GraphicsOptimization::Balanced), None);
        assert_eq!(specialized_mode_note(GraphicsOptimization::MoreCores), None);
        assert_eq!(specialized_mode_note(GraphicsOptimization::Speed), None);
        assert!(specialized_mode_note(GraphicsOptimization::MobileTier).unwrap().contains("Mobile Tier"));
        assert!(specialized_mode_note(GraphicsOptimization::RobloxApp).unwrap().contains("Roblox App"));
        assert!(specialized_mode_note(GraphicsOptimization::FewerCores).unwrap().contains("Fewer Cores"));
    }

    #[test]
    fn status_strip_formats_profile_mode_and_build() {
        assert_eq!(
            format_status_strip("default", GraphicsOptimization::Speed, Some("Roblox 2.738.0.1397")),
            "default  ·  Speed  ·  Roblox 2.738.0.1397"
        );
        assert_eq!(
            format_status_strip("alt", GraphicsOptimization::Balanced, None),
            "alt  ·  Balanced  ·  Download needed"
        );
    }

    #[test]
    #[ignore = "requires a GTK display"]
    fn the_hero_preserves_the_launch_button_as_the_focus_target() {
        adw::init().unwrap();
        let chooser = build(&VantaSource, |_| {});
        assert!(chooser.is::<gtk::Button>());
        let window = gtk::Window::new();
        window.set_child(Some(&hero(chooser.clone())));
        GtkWindowExt::set_focus(&window, Some(&chooser));
        assert_eq!(GtkWindowExt::focus(&window), Some(chooser));
        window.close();
    }
}
