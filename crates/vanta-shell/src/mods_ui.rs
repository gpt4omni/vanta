//! Settings → Mods page: non-destructive asset modding manager.
//!
//! Manages mod packs under `<profile>/mods/<pack>/` with per-profile enablement
//! in `<profile>/mods-enabled.json`, master toggle (`mods_enabled`), folder
//! launcher, and one-click folder/ZIP import.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use libadwaita as adw;
use libadwaita::gio;
use libadwaita::glib;
use libadwaita::gtk;
use libadwaita::prelude::*;

use crate::shell_config::{self, ShellConfig};
use vanta_shell::mods::{self, ModPack};

/// Open `dir` in the system's default file manager, falling back to `xdg-open`.
fn open_folder(dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dir)
        .map_err(|e| format!("could not create {}: {e}", dir.display()))?;
    let uri = glib::filename_to_uri(dir, None)
        .map_err(|e| format!("could not format file URI: {e}"))?;
    if gio::AppInfo::launch_default_for_uri(&uri, gio::AppLaunchContext::NONE).is_ok() {
        return Ok(());
    }
    std::process::Command::new("xdg-open")
        .arg(dir)
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("could not open folder: {e}"))
}

/// Build the Settings → Mods `AdwPreferencesPage`.
pub fn build_mods_page(
    parent: &impl IsA<gtk::Window>,
    dialog: &adw::PreferencesDialog,
    config: Rc<RefCell<ShellConfig>>,
    config_path: Rc<PathBuf>,
) -> adw::PreferencesPage {
    let page = adw::PreferencesPage::builder()
        .title("Mods")
        .name("mods")
        .icon_name("applications-graphics-symbolic")
        .build();

    let profile_name = config.borrow().profile.clone();
    let profile_dir = vanta_shell::profile::dir(&profile_name)
        .unwrap_or_else(|_| vanta_shell::profile::root().join(shell_config::DEFAULT_PROFILE));

    // ---- Master toggle & actions -------------------------------------------
    let controls_group = adw::PreferencesGroup::builder()
        .title("Asset Mods")
        .description(
            "Non-destructive file overlays loaded from your profile's mods folder. \
             Survives Roblox updates untouched without modifying base.apk.",
        )
        .build();

    let master_row = adw::SwitchRow::builder()
        .title("Enable asset mods")
        .subtitle("Zero overhead when no mod packs are active. Applies at next launch.")
        .active(config.borrow().mods_enabled)
        .build();
    master_row.set_subtitle_lines(2);
    {
        let config = config.clone();
        let config_path = config_path.clone();
        master_row.connect_active_notify(move |row| {
            config.borrow_mut().mods_enabled = row.is_active();
            if let Err(e) = shell_config::save(&config_path, &config.borrow()) {
                eprintln!("shell: could not save {}: {e}", config_path.display());
            }
        });
    }
    controls_group.add(&master_row);

    let actions_row = adw::ActionRow::builder()
        .title("Mod packs folder")
        .subtitle(mods::mods_dir(&profile_dir).display().to_string())
        .build();
    actions_row.set_subtitle_lines(2);

    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    buttons.set_valign(gtk::Align::Center);

    let open_btn = gtk::Button::builder()
        .label("Open Mods Folder")
        .build();
    {
        let dir = mods::mods_dir(&profile_dir);
        let dialog = dialog.clone();
        open_btn.connect_clicked(move |_| {
            if let Err(e) = open_folder(&dir) {
                dialog.add_toast(adw::Toast::new(&e));
            }
        });
    }

    let import_zip_btn = gtk::Button::builder()
        .label("Import ZIP…")
        .build();
    let import_dir_btn = gtk::Button::builder()
        .label("Import Folder…")
        .build();
    let refresh_btn = gtk::Button::builder()
        .icon_name("view-refresh-symbolic")
        .tooltip_text("Rescan mods folder")
        .build();

    buttons.append(&open_btn);
    buttons.append(&import_zip_btn);
    buttons.append(&import_dir_btn);
    buttons.append(&refresh_btn);
    actions_row.add_suffix(&buttons);
    controls_group.add(&actions_row);
    page.add(&controls_group);

    // ---- Discovered packs list ---------------------------------------------
    let packs_group = adw::PreferencesGroup::builder()
        .title("Installed Mod Packs")
        .build();
    let rows_holder: Rc<RefCell<Vec<gtk::Widget>>> = Rc::new(RefCell::new(Vec::new()));

    let rebuild_packs: Rc<dyn Fn()> = {
        let packs_group = packs_group.clone();
        let rows_holder = rows_holder.clone();
        let profile_dir = profile_dir.clone();
        let dialog = dialog.clone();
        Rc::new(move || {
            for old in rows_holder.borrow_mut().drain(..) {
                packs_group.remove(&old);
            }
            let packs = mods::discover_packs(&profile_dir);
            if packs.is_empty() {
                let empty = adw::ActionRow::builder()
                    .title("No mod packs installed yet")
                    .subtitle(
                        "Drop any folder with files like content/sounds/ouch.ogg, \
                         content/fonts/…, or content/textures/cursors/… into the mods folder \
                         (no mod.json required), or use Import ZIP / Import Folder above.",
                    )
                    .build();
                empty.set_subtitle_lines(3);
                packs_group.add(&empty);
                rows_holder.borrow_mut().push(empty.upcast());
                return;
            }
            for pack in packs {
                let row = build_pack_row(&pack, &profile_dir, &dialog);
                packs_group.add(&row);
                rows_holder.borrow_mut().push(row.upcast());
            }
        })
    };

    rebuild_packs();

    {
        let rebuild = rebuild_packs.clone();
        refresh_btn.connect_clicked(move |_| rebuild());
    }

    {
        let parent = parent.clone().upcast::<gtk::Window>();
        let dialog = dialog.clone();
        let profile_dir = profile_dir.clone();
        let rebuild = rebuild_packs.clone();
        import_zip_btn.connect_clicked(move |_| {
            let filter = gtk::FileFilter::new();
            filter.set_name(Some("ZIP mod archives"));
            filter.add_suffix("zip");
            let filters = gio::ListStore::new::<gtk::FileFilter>();
            filters.append(&filter);

            let picker = gtk::FileDialog::builder()
                .title("Import Mod ZIP")
                .modal(true)
                .filters(&filters)
                .build();
            let dialog = dialog.clone();
            let profile_dir = profile_dir.clone();
            let rebuild = rebuild.clone();
            picker.open(Some(&parent), gio::Cancellable::NONE, move |res| {
                let Ok(file) = res else { return };
                let Some(path) = file.path() else {
                    dialog.add_toast(adw::Toast::new("Selected archive has no local path."));
                    return;
                };
                match mods::import_mod_zip(&profile_dir, &path) {
                    Ok(pack_id) => {
                        rebuild();
                        dialog.add_toast(adw::Toast::new(&format!(
                            "Imported mod pack \"{pack_id}\". Applies at next launch."
                        )));
                    }
                    Err(e) => dialog.add_toast(adw::Toast::new(&format!("Import failed: {e}"))),
                }
            });
        });
    }

    {
        let parent = parent.clone().upcast::<gtk::Window>();
        let dialog = dialog.clone();
        let profile_dir = profile_dir.clone();
        let rebuild = rebuild_packs.clone();
        import_dir_btn.connect_clicked(move |_| {
            let picker = gtk::FileDialog::builder()
                .title("Import Mod Folder")
                .modal(true)
                .build();
            let dialog = dialog.clone();
            let profile_dir = profile_dir.clone();
            let rebuild = rebuild.clone();
            picker.select_folder(Some(&parent), gio::Cancellable::NONE, move |res| {
                let Ok(file) = res else { return };
                let Some(path) = file.path() else {
                    dialog.add_toast(adw::Toast::new("Selected folder has no local path."));
                    return;
                };
                match mods::import_mod_dir(&profile_dir, &path) {
                    Ok(pack_id) => {
                        rebuild();
                        dialog.add_toast(adw::Toast::new(&format!(
                            "Imported mod pack \"{pack_id}\". Applies at next launch."
                        )));
                    }
                    Err(e) => dialog.add_toast(adw::Toast::new(&format!("Import failed: {e}"))),
                }
            });
        });
    }

    page.add(&packs_group);
    page
}

fn build_pack_row(
    pack: &ModPack,
    profile_dir: &Path,
    dialog: &adw::PreferencesDialog,
) -> adw::ActionRow {
    let subtitle = if pack.description.is_empty() {
        format!("Folder: {}", pack.id)
    } else {
        pack.description.clone()
    };
    let row = adw::ActionRow::builder()
        .title(glib::markup_escape_text(&pack.name))
        .subtitle(glib::markup_escape_text(&subtitle))
        .build();
    row.set_subtitle_lines(2);

    let count_text = if pack.file_count == 1 {
        "1 file".to_string()
    } else {
        format!("{} files", pack.file_count)
    };
    let badge = gtk::Label::new(Some(&count_text));
    badge.add_css_class("caption");
    badge.add_css_class("dim-label");
    badge.set_valign(gtk::Align::Center);
    badge.set_margin_end(8);
    row.add_suffix(&badge);

    if pack.priority != 0 {
        let prio = gtk::Label::new(Some(&format!("P{}", pack.priority)));
        prio.add_css_class("caption");
        prio.add_css_class("dim-label");
        prio.set_valign(gtk::Align::Center);
        prio.set_margin_end(8);
        row.add_suffix(&prio);
    }

    let switch = gtk::Switch::builder()
        .active(pack.enabled)
        .valign(gtk::Align::Center)
        .build();
    {
        let profile_dir = profile_dir.to_path_buf();
        let pack_id = pack.id.clone();
        let dialog = dialog.clone();
        switch.connect_active_notify(move |sw| {
            if let Err(e) = mods::set_pack_enabled(&profile_dir, &pack_id, sw.is_active()) {
                dialog.add_toast(adw::Toast::new(&format!("Could not save mod state: {e}")));
            }
        });
    }
    row.add_suffix(&switch);
    row.set_activatable_widget(Some(&switch));
    row
}
