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
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::File;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

/// Maximum decompressed size for a single file inside an imported `.zip` mod pack (64 MiB).
const MAX_MOD_FILE_BYTES: u64 = 64 * 1024 * 1024;
/// Maximum total decompressed size for an imported `.zip` mod pack (512 MiB).
const MAX_MOD_TOTAL_BYTES: u64 = 512 * 1024 * 1024;

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

/// Zero-allocation check for a canonical relative asset lookup name.
///
/// Returns `Some(raw)` when `raw` is already normalized (non-empty, no NUL,
/// no `\`, no leading `/`, no `//`, and no `.`/`..` segments) and `None`
/// otherwise.
pub fn validate_asset_name(raw: &str) -> Option<&str> {
    if raw.is_empty() || raw.contains('\0') || raw.contains('\\') || raw.starts_with('/') {
        return None;
    }
    let mut any = false;
    for segment in raw.split('/') {
        if segment.is_empty() || segment == "." || segment == ".." {
            return None;
        }
        any = true;
    }
    any.then_some(raw)
}

/// Canonicalize a relative asset lookup name, rejecting path traversal (`..`),
/// absolute paths, backslashes, NUL bytes, and empty paths.
pub fn normalize_asset_name(raw: &str) -> Option<String> {
    validate_asset_name(raw).map(str::to_string)
}

/// Walk all regular asset files contained strictly inside `pack_root`.
///
/// Canonicalizes every entry, drops symlinks or paths that escape `pack_root`,
/// and tracks visited canonicalized directories to guard against internal
/// symlink cycles. Skips `mod.json` at the pack root so pack metadata is never
/// served as an asset.
fn walk_pack_files(pack_root: &Path) -> Vec<(String, PathBuf)> {
    let Ok(canon_root) = pack_root.canonicalize() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut visited = HashSet::new();
    visited.insert(canon_root.clone());
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
                if visited.insert(real.clone()) {
                    stack.push(real);
                }
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

fn discover_packs_inner(profile_dir: &Path, count_files: bool) -> Vec<ModPack> {
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
        let file_count = if count_files { walk_pack_files(&path).len() } else { 0 };

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

/// Discover all mod packs under `<profile>/mods/`, ordered by ascending
/// `(priority, id)` so later entries win during overlay merging.
pub fn discover_packs(profile_dir: &Path) -> Vec<ModPack> {
    discover_packs_inner(profile_dir, true)
}

/// Build the merged asset lookup table (`relative/asset/path -> host PathBuf`)
/// for all enabled mod packs under `profile_dir`.
///
/// Packs are applied in ascending `(priority, id)` order, so higher priority
/// packs win on collisions. Every file is canonicalized and verified to stay
/// inside its pack root. Walks each enabled pack exactly once (skipping
/// disabled packs entirely).
pub fn build_mod_index(profile_dir: &Path) -> HashMap<String, PathBuf> {
    let packs = discover_packs_inner(profile_dir, false);
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
/// that escape `src_dir` and guarding against internal symlink cycles.
/// Removes any partial destination folder on error. Returns the created pack id.
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

    let copy_result: Result<usize, String> = (|| {
        let mut visited = HashSet::new();
        visited.insert(canon_src.clone());
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
                    if visited.insert(real.clone()) {
                        stack.push(real);
                    }
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
        Ok(copied)
    })();

    match copy_result {
        Ok(0) => {
            let _ = std::fs::remove_dir_all(&dest_root);
            Err("the selected folder contains no files".to_string())
        }
        Ok(_) => Ok(pack_id),
        Err(e) => {
            let _ = std::fs::remove_dir_all(&dest_root);
            Err(e)
        }
    }
}

/// Extract a `.zip` mod archive safely into `<profile>/mods/<pack>/`.
///
/// Rejects zip-slip paths via `enclosed_name()`, enforces per-file and total
/// decompressed byte caps via `Read::take` (defending against zip bombs with
/// forged header sizes), removes any partial destination on error, and strips
/// a single common top-level wrapper folder (if all entries sit inside it and
/// it is not `content`/`extra` itself). Returns the created pack id.
pub fn import_mod_zip(profile_dir: &Path, zip_path: &Path) -> Result<String, String> {
    import_mod_zip_with_limits(profile_dir, zip_path, MAX_MOD_FILE_BYTES, MAX_MOD_TOTAL_BYTES)
}

fn import_mod_zip_with_limits(
    profile_dir: &Path,
    zip_path: &Path,
    max_file_bytes: u64,
    max_total_bytes: u64,
) -> Result<String, String> {
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

    let extract_result: Result<(), String> = (|| {
        let mut total_written: u64 = 0;
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
            let remaining_total = max_total_bytes.saturating_sub(total_written);
            let effective_limit = max_file_bytes.min(remaining_total);
            let mut bytes = Vec::with_capacity(
                entry.size().min(effective_limit).min(4 * 1024 * 1024) as usize,
            );
            let read_len = (&mut entry)
                .take(effective_limit.saturating_add(1))
                .read_to_end(&mut bytes)
                .map_err(|e| e.to_string())? as u64;
            if read_len > max_file_bytes {
                return Err(format!(
                    "mod file '{}' exceeds the {} byte per-file size limit",
                    rel.display(),
                    max_file_bytes
                ));
            }
            if total_written.saturating_add(read_len) > max_total_bytes {
                return Err(format!(
                    "mod archive exceeds the {} byte total decompressed size limit",
                    max_total_bytes
                ));
            }
            total_written += read_len;
            std::fs::write(&target, &bytes).map_err(|e| format!("{}: {e}", target.display()))?;
        }
        Ok(())
    })();

    if let Err(e) = extract_result {
        let _ = std::fs::remove_dir_all(&dest_root);
        return Err(e);
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

    #[test]
    fn internal_symlink_cycle_does_not_loop_in_walk_or_import_dir() {
        let profile = scratch("symlink-cycle");
        let src = profile.join("src-cycle");
        std::fs::create_dir_all(src.join("content/sounds")).unwrap();
        std::fs::write(src.join("content/sounds/ouch.ogg"), b"loop-safe").unwrap();
        // Self-referencing directory symlink inside the pack.
        std::os::unix::fs::symlink(&src, src.join("content/loop_root")).unwrap();
        std::os::unix::fs::symlink(src.join("content"), src.join("content/sounds/loop_parent"))
            .unwrap();

        let pack_id = import_mod_dir(&profile, &src).unwrap();
        let pack_root = mods_dir(&profile).join(&pack_id);
        // Also place a cycle directly inside the installed pack to exercise walk_pack_files.
        std::os::unix::fs::symlink(&pack_root, pack_root.join("content/self_loop")).unwrap();

        let idx = build_mod_index(&profile);
        assert_eq!(idx.len(), 1);
        assert_eq!(std::fs::read(&idx["content/sounds/ouch.ogg"]).unwrap(), b"loop-safe");

        let _ = std::fs::remove_dir_all(&profile);
    }

    #[test]
    fn import_zip_enforces_decompressed_caps_and_cleans_up_partial_dest() {
        let profile = scratch("zip-bomb");
        let zip_path = profile.join("bomb.zip");
        {
            let file = File::create(&zip_path).unwrap();
            let mut w = zip::ZipWriter::new(file);
            let opts = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);
            w.start_file("content/a.bin", opts).unwrap();
            w.write_all(&vec![0u8; 2048]).unwrap();
            w.start_file("content/b.bin", opts).unwrap();
            w.write_all(&vec![0u8; 2048]).unwrap();
            w.finish().unwrap();
        }

        // Per-file cap exceeded on first file.
        let err = import_mod_zip_with_limits(&profile, &zip_path, 1024, 8192).unwrap_err();
        assert!(err.contains("per-file size limit"), "{err}");
        assert!(
            !mods_dir(&profile).join("bomb").exists(),
            "partial dest_root must be removed on per-file limit error"
        );

        // Total archive cap exceeded on second file (after first file was written!).
        let err = import_mod_zip_with_limits(&profile, &zip_path, 4096, 3000).unwrap_err();
        assert!(err.contains("total decompressed size limit"), "{err}");
        assert!(
            !mods_dir(&profile).join("bomb").exists(),
            "partial dest_root must be removed on total-size limit error"
        );

        let _ = std::fs::remove_dir_all(&profile);
    }

    #[test]
    fn validate_asset_name_borrows_input_without_allocation() {
        let input = "content/textures/cursors/arrow.png";
        let validated = validate_asset_name(input).expect("valid");
        assert!(std::ptr::eq(validated.as_ptr(), input.as_ptr()));
        assert_eq!(validate_asset_name("content//double.png"), None);
        assert_eq!(validate_asset_name("content/./dot.png"), None);
        assert_eq!(validate_asset_name("content/../up.png"), None);
        assert_eq!(validate_asset_name("/leading.png"), None);
        assert_eq!(validate_asset_name("trailing/"), None);
    }
}
