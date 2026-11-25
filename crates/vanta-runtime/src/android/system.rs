//! Android's `/system` tree, built from what the host already has.
//!
//! Roblox asks for `/system/fonts/NotoSansCJK-Regular.ttc` during app startup.
//! On Android that file is always there. On a Linux host `/system` does not
//! exist, and the engine converts the failed lookup into an empty path and
//! throws `Path does not exist: ""` — an exception that names nothing, because
//! by the time it is raised there is no path left to name.
//!
//! Providing `/system` is the runtime's job, the same way `AAssetManager` and
//! `ALooper` are. The redirect itself lives at the libc boundary in
//! `native/system_paths.cpp`; this module builds the directory it points at.
//!
//! `/` is read-only on an image-based host, so a real `/system` is not an
//! option even with privileges — which is the other reason to do this in the
//! symbol table rather than on the filesystem.

use std::collections::BTreeMap;
use std::ffi::CString;
use std::path::{Path, PathBuf};

extern "C" {
    fn vanta_set_system_root(root: *const std::ffi::c_char);
    fn vanta_set_files_dir(dir: *const std::ffi::c_char);
}

/// Tell the framework layer which files directory this profile uses.
///
/// `native/android_classes.cpp`'s `files_dir()` computed
/// `vanta/instances/default/data` — the layout ADR-012 replaced — and followed
/// no `--profile`, so `Context.getFilesDir()` answered about a directory the
/// client was not using, identically for every profile. Call this with the same
/// path handed to `nativeSetFilesDirectory`, and before anything asks: the C++
/// side caches on first use.
pub fn set_files_dir(dir: &Path) {
    if let Ok(c) = CString::new(dir.to_string_lossy().as_bytes()) {
        // SAFETY: the callee copies the string; `c` need not outlive the call.
        unsafe { vanta_set_files_dir(c.as_ptr()) };
    }
}

/// Where the host's fonts live. Searched in order; later entries do not
/// override earlier ones, so a user font never shadows a system one by accident.
const FONT_DIRS: &[&str] = &[
    "/usr/share/fonts",
    "/usr/local/share/fonts",
    "/run/host/usr/share/fonts",
];

/// Android font file names worth answering for, and the host fonts that will
/// stand in when the exact name is missing.
///
/// Only the first of these is confirmed to be requested — the engine asked for
/// it by name, which is how this whole path was found. The rest are the ordinary
/// AOSP font set; answering them costs a symlink each and turns a future hard
/// failure into a slightly wrong glyph.
const ALIASES: &[(&str, &[&str])] = &[
    (
        "NotoSansCJK-Regular.ttc",
        &["NotoSansCJK-Regular.ttc", "NotoSansCJKjp-Regular.otf"],
    ),
    ("Roboto-Regular.ttf", &["NotoSans-Regular.ttf", "DejaVuSans.ttf"]),
    ("Roboto-Bold.ttf", &["NotoSans-Bold.ttf", "DejaVuSans-Bold.ttf"]),
    ("DroidSans.ttf", &["NotoSans-Regular.ttf", "DejaVuSans.ttf"]),
    ("DroidSansMono.ttf", &["NotoSansMono-Regular.ttf", "DejaVuSansMono.ttf"]),
    ("DroidSansFallback.ttf", &["NotoSansCJK-Regular.ttc", "DejaVuSans.ttf"]),
    ("NotoColorEmoji.ttf", &["NotoColorEmoji.ttf"]),
];

fn cache_root() -> PathBuf {
    std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
        .unwrap_or_else(std::env::temp_dir)
        .join("vanta/system")
}

/// The host font roots scanned to build `/system/fonts`.
fn font_roots() -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = FONT_DIRS.iter().map(PathBuf::from).collect();
    if let Some(home) = std::env::var_os("HOME") {
        roots.push(PathBuf::from(&home).join(".local/share/fonts"));
        roots.push(PathBuf::from(&home).join(".fonts"));
    }
    roots
}

/// Fingerprint of the scanned font roots using each root's mtime (plus version
/// header so future alias table updates invalidate cleanly).
fn roots_stamp(roots: &[PathBuf]) -> String {
    use std::fmt::Write;
    let mut out = format!("v1;aliases={}\n", ALIASES.len());
    for root in roots {
        let mtime = std::fs::metadata(root)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| format!("{}.{:09}", d.as_secs(), d.subsec_nanos()))
            .unwrap_or_else(|| "missing".to_string());
        let _ = writeln!(out, "{}={mtime}", root.display());
    }
    out
}

/// Every font file under `roots`, by file name. First writer wins.
fn host_fonts_in(roots: &[PathBuf]) -> BTreeMap<String, PathBuf> {
    let mut found = BTreeMap::new();
    for root in roots {
        walk(root, &mut found, 0);
    }
    found
}

fn walk(dir: &Path, out: &mut BTreeMap<String, PathBuf>, depth: usize) {
    // Font trees are shallow. The bound is against a symlink loop, not against
    // any real directory layout.
    if depth > 6 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            walk(&p, out, depth + 1);
        } else if is_font(&p) {
            if let Some(name) = p.file_name().and_then(|n| n.to_str()) {
                out.entry(name.to_string()).or_insert(p.clone());
            }
        }
    }
}

fn is_font(p: &Path) -> bool {
    matches!(
        p.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).as_deref(),
        Some("ttf" | "ttc" | "otf" | "otc")
    )
}

/// Pick the host file that should answer for an Android font name.
fn resolve<'a>(
    candidates: &[&str],
    fonts: &'a BTreeMap<String, PathBuf>,
) -> Option<&'a PathBuf> {
    candidates.iter().find_map(|c| fonts.get(*c))
}

/// Build the tree and arm the redirect. Returns the directory serving `/system`.
///
/// Never fails the launch: a client that starts with the wrong glyphs is more
/// useful than one that refuses to start because a font was missing. If nothing
/// can be linked the redirect is still armed, so the engine's lookups land in a
/// directory Vanta controls and show up in `VANTA_TRACE_PATHS=1` as a miss
/// there — which is a far better diagnostic than a miss in a `/system` that
/// cannot exist.
/// Populate `fonts_dir` from `roots` unless `.stamp` already matches all
/// root mtimes. Returns `Some(linked)` when a rebuild ran, or `None` when the
/// warm stamp matched and the walk was skipped.
fn sync_fonts(fonts_dir: &Path, roots: &[PathBuf]) -> Option<usize> {
    let stamp_path = fonts_dir.join(".stamp");
    let expected = roots_stamp(roots);
    if std::fs::read_to_string(&stamp_path).ok().as_deref() == Some(expected.as_str()) {
        return None;
    }

    let fonts = host_fonts_in(roots);
    let mut linked = 0usize;

    // Every host font under its own name first, so an exact request for a font
    // the host happens to have resolves to the real thing rather than a stand-in.
    for (name, path) in &fonts {
        if link(path, &fonts_dir.join(name)) {
            linked += 1;
        }
    }
    // Then the AOSP names the host does not have.
    for (android, candidates) in ALIASES {
        let dest = fonts_dir.join(android);
        if dest.exists() {
            continue;
        }
        if let Some(src) = resolve(candidates, &fonts) {
            if link(src, &dest) {
                linked += 1;
            }
        }
    }
    let _ = std::fs::write(&stamp_path, expected);
    Some(linked)
}

pub fn install() -> PathBuf {
    let root = cache_root();
    let fonts_dir = root.join("fonts");
    let _ = std::fs::create_dir_all(&fonts_dir);

    let roots = font_roots();
    let rebuilt = sync_fonts(&fonts_dir, &roots);

    if std::env::var_os("VANTA_ANDROID_TRACE").is_some() {
        match rebuilt {
            Some(linked) => {
                println!("[android] /system served from {} ({linked} fonts)", root.display())
            }
            None => println!("[android] /system served from {} (warm stamp)", root.display()),
        }
    }

    if let Ok(c) = CString::new(root.to_string_lossy().as_bytes()) {
        // SAFETY: `vanta_set_system_root` copies the string; `c` need not
        // outlive the call.
        unsafe { vanta_set_system_root(c.as_ptr()) };
    }
    root
}

/// Symlink `src` to `dest`, replacing a stale link. Returns whether `dest` names
/// a usable font afterwards.
fn link(src: &Path, dest: &Path) -> bool {
    if let Ok(existing) = std::fs::read_link(dest) {
        if existing == src {
            return true;
        }
        let _ = std::fs::remove_file(dest);
    } else if dest.exists() {
        return true;
    }
    std::os::unix::fs::symlink(src, dest).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_font_files_are_indexed() {
        assert!(is_font(Path::new("/x/NotoSansCJK-Regular.ttc")));
        assert!(is_font(Path::new("/x/DejaVuSans.TTF")));
        assert!(!is_font(Path::new("/x/fonts.dir")));
        assert!(!is_font(Path::new("/x/README")));
    }

    #[test]
    fn an_alias_prefers_the_exact_name_over_a_stand_in() {
        let mut fonts = BTreeMap::new();
        fonts.insert("DejaVuSans.ttf".to_string(), PathBuf::from("/h/DejaVuSans.ttf"));
        fonts.insert(
            "NotoSans-Regular.ttf".to_string(),
            PathBuf::from("/h/NotoSans-Regular.ttf"),
        );
        // Roboto-Regular's candidate list puts NotoSans first, so it wins even
        // though DejaVu is also present.
        let got = resolve(&["NotoSans-Regular.ttf", "DejaVuSans.ttf"], &fonts);
        assert_eq!(got, Some(&PathBuf::from("/h/NotoSans-Regular.ttf")));
    }

    #[test]
    fn resolve_gives_up_rather_than_guessing() {
        let fonts = BTreeMap::new();
        assert!(resolve(&["NotoColorEmoji.ttf"], &fonts).is_none());
    }

    #[test]
    fn sync_fonts_skips_warm_walk_when_stamp_matches() {
        let base = std::env::temp_dir().join(format!("vanta-sys-stamp-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let src = base.join("host_fonts");
        let dst = base.join("out_fonts");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&dst).unwrap();
        std::fs::write(src.join("DejaVuSans.ttf"), b"font").unwrap();

        let roots = vec![src.clone()];
        let first = sync_fonts(&dst, &roots);
        assert!(first.is_some(), "cold run must populate symlinks");
        assert!(dst.join("DejaVuSans.ttf").exists());
        assert!(dst.join(".stamp").exists());

        let second = sync_fonts(&dst, &roots);
        assert_eq!(second, None, "warm run with unchanged root mtime must skip walk");

        let _ = std::fs::remove_dir_all(&base);
    }
}
