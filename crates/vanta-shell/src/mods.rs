//! Non-destructive asset mod pack discovery, enablement, import, and indexing.
//!
//! Mod packs live under `<profile>/mods/<pack-name>/` and mirror Roblox's
//! `assets/` layout (for example `content/sounds/ouch.ogg`,
//! `content/fonts/...`, `content/textures/cursors/...`).
//!
//! No manifest is required: dropping any plain folder of asset files into
//! `<profile>/mods/<pack>/` works out of the box. An optional `mod.json` in
//! the pack root may supply a display name, description, default enablement,
//! and numeric priority (higher priority wins on file collisions).
//!
//! Per-profile enablement overrides are stored in `<profile>/mods-enabled.json`
//! so toggling packs never mutates pack files or manifests.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

/// Optional manifest placed at `<profile>/mods/<pack>/mod.json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModManifest {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub priority: i32,
}

/// One discovered mod pack inside `<profile>/mods/`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModPack {
    /// Folder name under `<profile>/mods/`.
    pub id: String,
    /// Display name from `mod.json`, or `id` when absent.
    pub name: String,
    /// Optional description from `mod.json`.
    pub description: String,
    /// Effective enabled state (`mods-enabled.json` overrides `mod.json`, default `true`).
    pub enabled: bool,
    /// Merge priority: higher numbers win when two enabled packs supply the same asset.
    pub priority: i32,
    /// Number of valid asset files inside this pack (excluding root `mod.json`).
    pub file_count: usize,
    /// Absolute path to `<profile>/mods/<id>`.
    pub root: PathBuf,
}

/// Directory containing mod packs for `profile_dir`: `<profile>/mods`.
pub fn mods_dir(profile_dir: &Path) -> PathBuf {
    profile_dir.join("mods")
}

/// Per-profile enablement map: `<profile>/mods-enabled.json`.
pub fn enablement_path(profile_dir: &Path) -> PathBuf {
    profile_dir.join("mods-enabled.json")
}

/// Read `<profile>/mods-enabled.json`, returning an empty map if absent or unreadable.
pub fn load_enablement(profile_dir: &Path) -> BTreeMap<String, bool> {
    let path = enablement_path(profile_dir);
    let Ok(text) = std::fs::read_to_string(&path) else {
        return BTreeMap::new();
    };
    serde_json::from_str(&text).unwrap_or_default()
}

/// Persist `<profile>/mods-enabled.json` atomically.
pub fn save_enablement(profile_dir: &Path, map: &BTreeMap<String, bool>) -> Result<(), String> {
    std::fs::create_dir_all(profile_dir)
        .map_err(|e| format!("could not create {}: {e}", profile_dir.display()))?;
    let path = enablement_path(profile_dir);
    let tmp = path.with_extension(format!("json.tmp-{}", std::process::id()));
    let json = serde_json::to_string_pretty(map).map_err(|e| e.to_string())?;
    std::fs::write(&tmp, json).map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, &path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("{}: {e}", path.display())
    })
}

/// Validate a pack folder name: single normal path component, no traversal.
pub fn is_valid_pack_id(id: &str) -> bool {
    if id.is_empty() || id.starts_with('.') || id.contains('\0') {
        return false;
    }
    let mut comps = Path::new(id).components();
    matches!(comps.next(), Some(Component::Normal(_))) && comps.next().is_none()
}

/// Enable or disable one mod pack in `<profile>/mods-enabled.json`.
pub fn set_pack_enabled(profile_dir: &Path, pack_id: &str, enabled: bool) -> Result<(), String> {
    if !is_valid_pack_id(pack_id) {
        return Err(format!("invalid mod pack id: {pack_id:?}"));
    }
    let mut map = load_enablement(profile_dir);
    map.insert(pack_id.to_string(), enabled);
    save_enablement(profile_dir, &map)
}

/// Canonicalize a relative asset lookup name, rejecting path traversal (`..`),
/// absolute paths, backslashes, NUL bytes, and empty paths.
pub fn normalize_asset_name(raw: &str) -> Option<String> {
    if raw.is_empty() || raw.contains('\0') || raw.contains('\\') || raw.starts_with('/') {
        return None;
    }
    let mut parts = Vec::new();
    for segment in raw.split('/') {
        if segment.is_empty() || segment == "." || segment == ".." {
            return None;
        }
        parts.push(segment);
    }
    if parts.is_empty() {
        return None;
    }
    Some(parts.join("/"))
}

/// Walk all regular asset files contained strictly inside `pack_root`.
///
/// Canonicalizes every entry and drops symlinks or paths that escape `pack_root`.
/// Skips `mod.json` at the pack root so pack metadata is never served as an asset.
fn walk_pack_files(pack_root: &Path) -> Vec<(String, PathBuf)> {
    let Ok(canon_root) = pack_root.canonicalize() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut stack = vec![canon_root.clone()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(real) = path.canonicalize() else {
                continue;
            };
            if !real.starts_with(&canon_root) {
                continue;
            }
            if real.is_dir() {
                stack.push(real);
                continue;
            }
            if !real.is_file() {
                continue;
            }
            let Ok(rel) = real.strip_prefix(&canon_root) else {
                continue;
            };
            if rel == Path::new("mod.json") {
                continue;
            }
            let mut segments = Vec::new();
            let mut valid = true;
            for c in rel.components() {
                match c {
                    Component::Normal(s) => match s.to_str() {
                        Some(seg) if !seg.is_empty() && seg != ".." && !seg.contains('\\') => {
                            segments.push(seg);
                        }
                        _ => {
                            valid = false;
                            break;
                        }
                    },
                    _ => {
                        valid = false;
                        break;
                    }
                }
            }
            if valid && !segments.is_empty() {
                out.push((segments.join("/"), real));
            }
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// Discover all mod packs under `<profile>/mods/`, ordered by ascending
/// `(priority, id)` so later entries win during overlay merging.
pub fn discover_packs(profile_dir: &Path) -> Vec<ModPack> {
    let root = mods_dir(profile_dir);
    let Ok(entries) = std::fs::read_dir(&root) else {
        return Vec::new();
    };
    let overrides = load_enablement(profile_dir);
    let mut packs = Vec::new();

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(id) = path.file_name().and_then(|n| n.to_str()).map(str::to_string) else {
            continue;
        };
        if !is_valid_pack_id(&id) {
            continue;
        }
        let manifest = std::fs::read_to_string(path.join("mod.json"))
            .ok()
            .and_then(|text| serde_json::from_str::<ModManifest>(&text).ok())
            .unwrap_or_default();

        let name = manifest
            .name
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(&id)
            .to_string();
        let description = manifest.description.unwrap_or_default().trim().to_string();
        let default_enabled = manifest.enabled.unwrap_or(true);
        let enabled = overrides.get(&id).copied().unwrap_or(default_enabled);
        let file_count = walk_pack_files(&path).len();

        packs.push(ModPack {
            id,
            name,
            description,
            enabled,
            priority: manifest.priority,
            file_count,
            root: path,
        });
    }

    packs.sort_by(|a, b| a.priority.cmp(&b.priority).then_with(|| a.id.cmp(&b.id)));
    packs
}

/// Build the merged asset lookup table (`relative/asset/path -> host PathBuf`)
/// for all enabled mod packs under `profile_dir`.
///
/// Packs are applied in ascending `(priority, id)` order, so higher priority
/// packs win on collisions. Every file is canonicalized and verified to stay
/// inside its pack root.
pub fn build_mod_index(profile_dir: &Path) -> HashMap<String, PathBuf> {
    let packs = discover_packs(profile_dir);
    let mut index = HashMap::new();
    for pack in packs.into_iter().filter(|p| p.enabled) {
        for (rel, real_path) in walk_pack_files(&pack.root) {
            index.insert(rel, real_path);
        }
    }
    index
}

/// Pick a non-colliding pack folder name under `mods_root` derived from `stem`.
fn allocate_pack_dir(mods_root: &Path, stem: &str) -> Result<(String, PathBuf), String> {
    std::fs::create_dir_all(mods_root)
        .map_err(|e| format!("could not create {}: {e}", mods_root.display()))?;
    let clean: String = stem
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | ' ') { c } else { '-' })
        .collect();
    let base = clean.trim().trim_start_matches('.').to_string();
    let base = if base.is_empty() { "mod-pack".to_string() } else { base };

    let mut candidate = base.clone();
    let mut n = 2u32;
    while mods_root.join(&candidate).exists() {
        candidate = format!("{base}-{n}");
        n += 1;
    }
    let dest = mods_root.join(&candidate);
    Ok((candidate, dest))
}

/// Copy an existing folder into `<profile>/mods/<pack>/`, skipping symlinks
/// that escape `src_dir`. Returns the created pack id.
pub fn import_mod_dir(profile_dir: &Path, src_dir: &Path) -> Result<String, String> {
    let canon_src = src_dir
        .canonicalize()
        .map_err(|e| format!("{}: {e}", src_dir.display()))?;
    if !canon_src.is_dir() {
        return Err(format!("{} is not a folder", src_dir.display()));
    }
    let stem = canon_src
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("mod-pack");
    let mods_root = mods_dir(profile_dir);
    let (pack_id, dest_root) = allocate_pack_dir(&mods_root, stem)?;
    std::fs::create_dir_all(&dest_root)
        .map_err(|e| format!("{}: {e}", dest_root.display()))?;

    let mut stack = vec![canon_src.clone()];
    let mut copied = 0usize;
    while let Some(dir) = stack.pop() {
        let entries = std::fs::read_dir(&dir)
            .map_err(|e| format!("{}: {e}", dir.display()))?;
        for entry in entries.flatten() {
            let Ok(real) = entry.path().canonicalize() else { continue };
            if !real.starts_with(&canon_src) {
                continue;
            }
            if real.is_dir() {
                stack.push(real);
                continue;
            }
            if !real.is_file() {
                continue;
            }
            let Ok(rel) = real.strip_prefix(&canon_src) else { continue };
            let target = dest_root.join(rel);
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("{}: {e}", parent.display()))?;
            }
            std::fs::copy(&real, &target)
                .map_err(|e| format!("{}: {e}", target.display()))?;
            copied += 1;
        }
    }
    if copied == 0 {
        let _ = std::fs::remove_dir_all(&dest_root);
        return Err("the selected folder contains no files".to_string());
    }
    Ok(pack_id)
}

/// Extract a `.zip` mod archive safely into `<profile>/mods/<pack>/`.
///
/// Rejects zip-slip paths via `enclosed_name()`, and strips a single common
/// top-level wrapper folder (if all entries sit inside it and it is not
/// `content`/`extra` itself) so archives packaged with a root folder work
/// seamlessly. Returns the created pack id.
pub fn import_mod_zip(profile_dir: &Path, zip_path: &Path) -> Result<String, String> {
    let file = File::open(zip_path).map_err(|e| format!("{}: {e}", zip_path.display()))?;
    let mut archive = zip::ZipArchive::new(file)
        .map_err(|e| format!("could not read {}: {e}", zip_path.display()))?;

    // Collect safe file entry indices and their enclosed paths.
    let mut entries: Vec<(usize, PathBuf)> = Vec::new();
    for i in 0..archive.len() {
        let entry = archive.by_index(i).map_err(|e| e.to_string())?;
        if entry.is_dir() {
            continue;
        }
        let Some(rel) = entry.enclosed_name() else {
            continue;
        };
        entries.push((i, rel));
    }
    if entries.is_empty() {
        return Err("the archive contains no usable files".to_string());
    }

    // Strip a single non-asset wrapper directory (e.g. `MySoundPack/content/...`).
    let strip_prefix: Option<PathBuf> = {
        let first_root = entries
            .first()
            .and_then(|(_, p)| p.components().next())
            .and_then(|c| match c {
                Component::Normal(s) => Some(s.to_os_string()),
                _ => None,
            });
        match first_root {
            Some(ref top)
                if top != "content"
                    && top != "extra"
                    && entries.iter().all(|(_, p)| {
                        let mut it = p.components();
                        matches!(it.next(), Some(Component::Normal(s)) if s == top.as_os_str())
                            && it.next().is_some()
                    }) =>
            {
                Some(PathBuf::from(top))
            }
            _ => None,
        }
    };

    let stem = zip_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("mod-pack");
    let mods_root = mods_dir(profile_dir);
    let (pack_id, dest_root) = allocate_pack_dir(&mods_root, stem)?;
    std::fs::create_dir_all(&dest_root)
        .map_err(|e| format!("{}: {e}", dest_root.display()))?;

    for (idx, rel) in entries {
        let rel = match &strip_prefix {
            Some(prefix) => rel.strip_prefix(prefix).unwrap_or(&rel),
            None => &rel,
        };
        if rel.as_os_str().is_empty() {
            continue;
        }
        let target = dest_root.join(rel);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        let mut entry = archive.by_index(idx).map_err(|e| e.to_string())?;
        let mut bytes = Vec::with_capacity(entry.size().min(32 * 1024 * 1024) as usize);
        entry.read_to_end(&mut bytes).map_err(|e| e.to_string())?;
        std::fs::write(&target, &bytes).map_err(|e| format!("{}: {e}", target.display()))?;
    }
    Ok(pack_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn scratch(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("vanta-mods-test-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn normalize_rejects_traversal_and_unsafe_paths() {
        assert_eq!(
            normalize_asset_name("content/sounds/ouch.ogg"),
            Some("content/sounds/ouch.ogg".to_string())
        );
        for bad in [
            "",
            "/etc/passwd",
            "../secret",
            "content/../../etc/passwd",
            "content/./sounds/ouch.ogg",
            "content//sounds/ouch.ogg",
            "content\\sounds\\ouch.ogg",
            "content/sounds/ouch\0.ogg",
        ] {
            assert_eq!(normalize_asset_name(bad), None, "{bad:?} must be rejected");
        }
    }

    #[test]
    fn plain_folder_without_manifest_works_and_skips_mod_json() {
        let profile = scratch("plain");
        let pack = mods_dir(&profile).join("classic-oof");
        std::fs::create_dir_all(pack.join("content/sounds")).unwrap();
        std::fs::write(pack.join("content/sounds/ouch.ogg"), b"OOG").unwrap();

        let packs = discover_packs(&profile);
        assert_eq!(packs.len(), 1);
        assert_eq!(packs[0].id, "classic-oof");
        assert_eq!(packs[0].name, "classic-oof");
        assert!(packs[0].enabled);
        assert_eq!(packs[0].priority, 0);
        assert_eq!(packs[0].file_count, 1);

        let idx = build_mod_index(&profile);
        assert_eq!(idx.len(), 1);
        assert!(idx.contains_key("content/sounds/ouch.ogg"));
        let _ = std::fs::remove_dir_all(&profile);
    }

    #[test]
    fn priority_order_and_enablement_filtering_resolve_collisions() {
        let profile = scratch("priority");
        let low = mods_dir(&profile).join("low-pack");
        let high = mods_dir(&profile).join("high-pack");
        std::fs::create_dir_all(low.join("content/sounds")).unwrap();
        std::fs::create_dir_all(high.join("content/sounds")).unwrap();

        std::fs::write(
            low.join("mod.json"),
            r#"{"name":"Low","description":"base","enabled":true,"priority":1}"#,
        )
        .unwrap();
        std::fs::write(low.join("content/sounds/ouch.ogg"), b"low").unwrap();

        std::fs::write(
            high.join("mod.json"),
            r#"{"name":"High","description":"override","enabled":true,"priority":10}"#,
        )
        .unwrap();
        std::fs::write(high.join("content/sounds/ouch.ogg"), b"high").unwrap();

        // High priority wins and mod.json is never indexed as an asset.
        let idx = build_mod_index(&profile);
        assert!(!idx.contains_key("mod.json"));
        assert_eq!(std::fs::read(&idx["content/sounds/ouch.ogg"]).unwrap(), b"high");

        // Disabling high-pack via mods-enabled.json falls back cleanly to low-pack.
        set_pack_enabled(&profile, "high-pack", false).unwrap();
        let idx = build_mod_index(&profile);
        assert_eq!(std::fs::read(&idx["content/sounds/ouch.ogg"]).unwrap(), b"low");

        // Disabling low-pack too empties the index.
        set_pack_enabled(&profile, "low-pack", false).unwrap();
        assert!(build_mod_index(&profile).is_empty());

        let _ = std::fs::remove_dir_all(&profile);
    }

    #[test]
    fn symlink_escaping_pack_root_is_dropped() {
        let profile = scratch("symlink");
        let outside = profile.join("outside.txt");
        std::fs::write(&outside, b"secret").unwrap();

        let pack = mods_dir(&profile).join("escape-pack");
        std::fs::create_dir_all(pack.join("content/sounds")).unwrap();
        std::os::unix::fs::symlink(&outside, pack.join("content/sounds/escape.ogg")).unwrap();
        std::fs::write(pack.join("content/sounds/safe.ogg"), b"safe").unwrap();

        let idx = build_mod_index(&profile);
        assert!(!idx.contains_key("content/sounds/escape.ogg"));
        assert!(idx.contains_key("content/sounds/safe.ogg"));

        let _ = std::fs::remove_dir_all(&profile);
    }

    #[test]
    fn import_zip_strips_wrapper_and_rejects_zip_slip() {
        let profile = scratch("zip-import");
        let zip_path = profile.join("custom-sounds.zip");
        {
            let file = File::create(&zip_path).unwrap();
            let mut w = zip::ZipWriter::new(file);
            let opts = zip::write::SimpleFileOptions::default();
            w.start_file("MyPack/content/sounds/ouch.ogg", opts).unwrap();
            w.write_all(b"zipped-oof").unwrap();
            w.start_file("../../evil.txt", opts).unwrap();
            w.write_all(b"nope").unwrap();
            w.finish().unwrap();
        }

        let pack_id = import_mod_zip(&profile, &zip_path).unwrap();
        assert_eq!(pack_id, "custom-sounds");
        let idx = build_mod_index(&profile);
        assert_eq!(std::fs::read(&idx["content/sounds/ouch.ogg"]).unwrap(), b"zipped-oof");
        assert!(!profile.join("evil.txt").exists());

        let _ = std::fs::remove_dir_all(&profile);
    }
}
