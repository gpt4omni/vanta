//! Runtime non-destructive asset mod overlay (`<profile>/mods/<pack>/`).
//!
//! Built once at startup from `<profile>/mods/` and `<profile>/mods-enabled.json`
//! via [`vanta_shell::mods::build_mod_index`]. On the hot `AAssetManager_open`
//! path, when no mods are enabled (or `VANTA_NO_MODS=1`), [`lookup`] performs a
//! single relaxed `AtomicBool` load and zero heap allocations.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};

/// Fast gate checked before touching any lock or string allocation on misses.
static HAS_MODS: AtomicBool = AtomicBool::new(false);

/// Immutable snapshot of enabled mod file mappings (`relative/asset -> host PathBuf`).
static MOD_INDEX: RwLock<Option<Arc<HashMap<String, PathBuf>>>> = RwLock::new(None);

/// Pure check for the `VANTA_NO_MODS` kill switch.
pub fn disabled_by_env_for(value: Option<&str>) -> bool {
    matches!(
        value.map(|s| s.trim().to_ascii_lowercase()).as_deref(),
        Some("1") | Some("true") | Some("yes") | Some("on")
    )
}

/// Whether `VANTA_NO_MODS` is currently set to disable all mod overlays.
pub fn disabled_by_env() -> bool {
    disabled_by_env_for(std::env::var("VANTA_NO_MODS").ok().as_deref())
}

/// Initialise the mod index from the active profile (`crate::profile::active()`).
///
/// Called once at startup before the engine opens its first asset. Returns the
/// number of overlaid asset files indexed.
pub fn init_from_active_profile() -> usize {
    let profile = crate::profile::active();
    init_for_profile(&profile)
}

/// Build and install the mod index for `profile_dir`, respecting `VANTA_NO_MODS=1`.
pub fn init_for_profile(profile_dir: &Path) -> usize {
    init_for_profile_with_kill_switch(profile_dir, disabled_by_env())
}

fn init_for_profile_with_kill_switch(profile_dir: &Path, kill_switch: bool) -> usize {
    if kill_switch {
        clear();
        println!("  mods: disabled (VANTA_NO_MODS)");
        return 0;
    }
    let map = vanta_shell::mods::build_mod_index(profile_dir);
    let count = map.len();
    install_index(map);
    if count > 0 {
        println!(
            "  mods: indexed {count} overlaid asset file(s) from {}",
            vanta_shell::mods::mods_dir(profile_dir).display()
        );
    }
    count
}

fn install_index(map: HashMap<String, PathBuf>) {
    let non_empty = !map.is_empty();
    if let Ok(mut guard) = MOD_INDEX.write() {
        *guard = if non_empty { Some(Arc::new(map)) } else { None };
    }
    HAS_MODS.store(non_empty, Ordering::Release);
}

/// Clear all loaded mod overlays (primarily for tests and `VANTA_NO_MODS=1`).
pub fn clear() {
    HAS_MODS.store(false, Ordering::Release);
    if let Ok(mut guard) = MOD_INDEX.write() {
        *guard = None;
    }
}

/// Look up an asset name (`content/sounds/ouch.ogg`) in the active mod index.
///
/// Zero allocations and zero locks when no mods are active. When mods are
/// active, validates `filename` in place with zero heap allocations and probes
/// `MOD_INDEX` directly by `&str`. Rejects path traversal (`..`), absolute
/// paths, NUL bytes, backslashes, `//`, and `.` segments.
pub fn lookup(filename: &str) -> Option<PathBuf> {
    if !HAS_MODS.load(Ordering::Relaxed) {
        return None;
    }
    let normalized = vanta_shell::mods::validate_asset_name(filename)?;
    let guard = MOD_INDEX.read().ok()?;
    guard.as_ref()?.get(normalized).cloned()
}

/// Serialise tests that install into the process-wide `MOD_INDEX`.
#[cfg(test)]
pub(crate) static TEST_GUARD: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("vanta-rt-mods-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn kill_switch_disables_all_overlays_cleanly() {
        let _g = TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        let profile = scratch("killswitch");
        let pack = vanta_shell::mods::mods_dir(&profile).join("pack-a");
        std::fs::create_dir_all(pack.join("content/sounds")).unwrap();
        std::fs::write(pack.join("content/sounds/ouch.ogg"), b"modded").unwrap();

        assert!(disabled_by_env_for(Some("1")));
        assert!(disabled_by_env_for(Some("true")));
        assert!(!disabled_by_env_for(Some("0")));
        assert!(!disabled_by_env_for(None));

        assert_eq!(init_for_profile_with_kill_switch(&profile, true), 0);
        assert!(!HAS_MODS.load(Ordering::Relaxed));
        assert_eq!(lookup("content/sounds/ouch.ogg"), None);

        assert_eq!(init_for_profile_with_kill_switch(&profile, false), 1);
        assert!(HAS_MODS.load(Ordering::Relaxed));
        assert!(lookup("content/sounds/ouch.ogg").is_some());

        clear();
        let _ = std::fs::remove_dir_all(&profile);
    }

    #[test]
    fn lookup_rejects_path_traversal_even_when_mods_are_loaded() {
        let _g = TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        let profile = scratch("traversal");
        let pack = vanta_shell::mods::mods_dir(&profile).join("pack-b");
        std::fs::create_dir_all(pack.join("content/fonts")).unwrap();
        std::fs::write(pack.join("content/fonts/custom.ttf"), b"TTF").unwrap();

        assert_eq!(init_for_profile_with_kill_switch(&profile, false), 1);
        assert!(lookup("content/fonts/custom.ttf").is_some());
        assert_eq!(lookup("../content/fonts/custom.ttf"), None);
        assert_eq!(lookup("content/../content/fonts/custom.ttf"), None);
        assert_eq!(lookup("/content/fonts/custom.ttf"), None);
        assert_eq!(lookup("content\\fonts\\custom.ttf"), None);
        assert_eq!(lookup("content//fonts/custom.ttf"), None);
        assert_eq!(lookup("content/./fonts/custom.ttf"), None);
        assert_eq!(lookup("content/fonts/missing.ttf"), None);

        clear();
        let _ = std::fs::remove_dir_all(&profile);
    }
}
