//! Layered FastFlag overrides, with provenance.
//!
//! Flags come from more than one place: Roblox's own settings document, the
//! user, and — once plugins exist — any plugin that needs a flag set to do its
//! job. Merging those into one file would be the obvious thing and it is wrong
//! in three specific ways:
//!
//! * a plugin could silently overwrite a value the user chose deliberately;
//! * uninstalling a plugin would leave its flags behind, because nothing records
//!   which line belonged to whom;
//! * "why is this flag set to that?" would have no answer.
//!
//! So each source owns its own file and they are resolved in a fixed order,
//! recording where every effective value came from.
//!
//! ```text
//! <profile>/flags.json                                user      (wins)
//! ~/.local/share/vanta/plugins/<id>/flags.json      plugin
//! the client-settings document from Roblox            base
//! ```
//!
//! **The user always wins.** An explicit setting is the one thing that must not
//! be overridable by software the user installed to do something else.
//!
//! **The user's file is per profile** (ADR-013). It used to be one file at
//! `~/.config/vanta/flags.json` for the whole machine, which meant a flag set
//! while debugging something on a test account was silently still set on the
//! account someone actually plays — and flags are exactly the setting people
//! change temporarily and forget. `migrate_legacy_user_file` moves an existing
//! one in rather than leaving it to be ignored, because a launch that quietly
//! stopped honouring overrides is indistinguishable from the overrides never
//! having worked.
//!
//! A plugin's own `flags.json` stays beside its installed code, which is global
//! — a plugin is installed once. That is a real asymmetry and it is recorded in
//! ADR-013's open question, because an installed plugin currently contributes
//! its flags in every profile whether or not it was granted anything there.
//!
//! **Plugin-against-plugin conflicts are reported, not resolved.** Two plugins
//! wanting the same flag set differently is a real disagreement, and picking one
//! by filesystem order would hide it. Both are named and the first-loaded wins,
//! so the behaviour is at least deterministic and visible.
//!
//! One property matters to anything built on this, and it is not obvious:
//! `FFlag`, `FInt` and `FString` are read once during engine startup, so a
//! plugin can only influence them if its flags are resolved *before the process
//! starts*. Only the `DFFlag`/`DFInt`/`DFString` family is re-read while the
//! client runs. A plugin loaded on demand, part-way through a session, cannot
//! change a startup flag no matter what it writes.
//!
//! **Being re-read is a cost, not a capability, and this paragraph used to read
//! as though it were the other way round.** The engine fetches Roblox's settings
//! document itself about two seconds in and applies it over the top, so a
//! `DF*` override of a key Roblox also sets is reverted to Roblox's value while
//! the client is still starting; see `client_settings::apply_overrides` for the
//! two-directional control that establishes it. The durable family is the one
//! read once. An override in either family is still worth writing — this layer
//! is how it reaches the engine at all — but a `DF*` one governs the first
//! couple of seconds and a measurement resting on it has to fall inside them.
//!
//! **A `DF*` override no longer stops holding after the engine's next refresh.**
//! The reloader refetches about every two minutes and reverts every `DF*` key
//! Roblox's document contains; `flag_reapply` hands the engine the document
//! again, with these overrides merged, once the engine's log says the refresh
//! has finished (ADR-051). That is why a flag set here is in force for the whole
//! run and not for two minutes of it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Where an effective flag value came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// The user's own `flags.json`.
    User,
    /// A plugin, by its directory name.
    Plugin(String),
    /// Vanta's own defaults, below every other layer.
    Builtin,
    /// The engine worker-pool flags the chosen [`Performance`] mode asks for.
    ///
    /// A source of its own rather than folded into [`Source::Builtin`],
    /// because the two answer different questions and the report is read to
    /// find out why a flag has the value it has. `Builtin` means "Vanta
    /// always sets this"; this means "you chose a mode in Settings and it set
    /// this". Sitting immediately above `Builtin` and below every plugin, it
    /// is a default a plugin or the user may still overrule -- which is right
    /// for a mode whose tables nothing on this project's hardware has
    /// measured.
    Performance,
    /// The Settings → Frame rate limit choice, or a measurement run's
    /// `VANTA_FRAME_RATE_LIMIT`. See [`FrameRateLimit`] for the whole of the
    /// reasoning, and [`collect`] for where it sits (above plugins, below the
    /// user's file); kept apart from [`Source::Performance`] for the same reason
    /// that is its own variant rather than folded into [`Source::Builtin`] --
    /// the report is read to find out why a flag has the value it has, and
    /// "you chose a frame rate cap" is a different answer from "you chose a
    /// worker-pool mode".
    FrameRateLimit,
}

impl Source {
    pub fn describe(&self) -> String {
        match self {
            Source::User => "user".into(),
            Source::Plugin(id) => format!("plugin:{id}"),
            Source::Builtin => "built-in".into(),
            Source::Performance => "performance mode".into(),
            Source::FrameRateLimit => "frame rate limit".into(),
        }
    }
}

/// One resolved override: the value that will be applied, and who asked for it.
#[derive(Debug, Clone)]
pub struct Resolved {
    pub value: String,
    pub source: Source,
    /// Other sources that wanted this flag and lost, in the order they were read.
    pub overridden: Vec<(Source, String)>,
}

/// A set of overrides read from one file.
#[derive(Clone)]
pub struct Layer {
    pub source: Source,
    pub values: BTreeMap<String, String>,
}

/// Read a flat JSON object of flag -> value.
///
/// Roblox stores every setting as a string, so booleans and numbers are
/// converted rather than refused — a config file should not require knowing
/// that. Returns `None` when the file is absent, and reports and skips when it
/// is present but malformed: a typo should not stop the client from starting.
///
/// **This conversion is not the reason an `FLog`/`DFLog` override can appear
/// to do nothing, or to silence a channel — established by experiment**
/// (`docs/analysis/flag-init.md` §22, corrected in the addendum after it).
/// Roblox's own settings document is entirely strings, and it is
/// *heterogeneous*: some logging channels are declared as a bare verbosity
/// number (`FLogNetwork = "7"`), others as a severity name with an optional
/// sub-level (`FLogAudio = "Info"`, `DFLogWebSocketTraceError = "Warning,6"`,
/// both seen directly in the cached document). Which shape a given channel
/// wants is a property of its own C++ declaration, invisible from here, and
/// this function does not try to guess it — Roblox's own document is not
/// internally consistent about it either, so guessing wrong here would be no
/// worse than guessing wrong there.
///
/// The failure mode when the wrong shape is sent looks exactly like this
/// function doing something wrong. §22 measured `FLogNativeDM` at `"1"`,
/// `"9"` and `"100"` — three bare numbers — and every one silenced a channel
/// that logs 12–30 lines when left unset, which read as "naming a channel can
/// quiet it". That does not survive a repeat with a severity name instead of
/// a number: `{"FLogNativeDM": "Verbose"}` left the channel at or above its
/// unset count (29 unset, 30 overridden, repeated), and the identical
/// override raised `FLogAppShellReporter` from 0 to 14–16 lines exactly as a
/// bare `"7"` or `"9"` already did elsewhere. So the conversion below was
/// never the fault: it passes `"7"` and `"Verbose"` through with equal
/// fidelity, which is all it should do. The engine's own parser, handed a
/// value of the wrong shape for a particular channel, fails safe to silent
/// rather than falling back to that channel's compiled default — and a
/// silenced channel is indistinguishable from a broken override unless the
/// shape is the first thing checked.
pub fn read_layer(path: &Path, source: Source) -> Option<Layer> {
    let text = std::fs::read_to_string(path).ok()?;
    let parsed: serde_json::Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(e) => {
            println!("  flags: {} is not valid JSON ({e}); ignoring", path.display());
            return None;
        }
    };
    let Some(obj) = parsed.as_object() else {
        println!("  flags: {} is not a JSON object; ignoring", path.display());
        return None;
    };
    let values = obj
        .iter()
        .map(|(k, v)| {
            let s = vanta_plugins::flag_document::normalise_json_value(k, v)
                .unwrap_or_else(|_| v.to_string());
            (k.clone(), s)
        })
        .collect();
    Some(Layer { source, values })
}

/// The FastFlag that overrides FRM quality level (`1..=21`).
pub const FRM_QUALITY_OVERRIDE_FLAG: &str = "DFIntDebugFRMQualityLevelOverride";

/// Roblox's Android settings document sets this to `"True"`, which causes
/// `FRM::getDistanceSquared()` to ignore `frm->distanceSquared` (`+0x110`, set
/// from the user's slider) and return `lockstepTable.Distance[qualityLevel]`
/// (500 studs at QL 1) whenever `DFIntDebugFRMQualityLevelOverride` is active.
pub const FRM_LOCKSTEP_FLAG: &str = "FFlagFRMLockstepOverridesEffective";

/// Preserve render distance when `DFIntDebugFRMQualityLevelOverride` is set to
/// a valid quality level (`1..=21`) and nothing explicitly set
/// `FFlagFRMLockstepOverridesEffective`.
fn preserve_frm_render_distance(resolved: &mut BTreeMap<String, Resolved>) {
    let valid_quality = resolved
        .get(FRM_QUALITY_OVERRIDE_FLAG)
        .and_then(|r| r.value.trim().parse::<i64>().ok())
        .is_some_and(|level| (1..=21).contains(&level));
    if valid_quality && !resolved.contains_key(FRM_LOCKSTEP_FLAG) {
        resolved.insert(
            FRM_LOCKSTEP_FLAG.to_string(),
            Resolved {
                value: "False".to_string(),
                source: Source::Builtin,
                overridden: Vec::new(),
            },
        );
    }
}

/// Resolve layers into one set of effective values.
///
/// `layers` is in precedence order, lowest first, so the user's layer goes last.
/// Everything a losing layer wanted is kept on the winner rather than discarded,
/// which is what makes the result explainable afterwards.
pub fn resolve(layers: Vec<Layer>) -> BTreeMap<String, Resolved> {
    let mut out: BTreeMap<String, Resolved> = BTreeMap::new();
    for layer in layers {
        for (key, value) in layer.values {
            match out.get_mut(&key) {
                Some(existing) => {
                    let previous = std::mem::replace(&mut existing.value, value);
                    let previous_source =
                        std::mem::replace(&mut existing.source, layer.source.clone());
                    existing.overridden.push((previous_source, previous));
                }
                None => {
                    out.insert(
                        key,
                        Resolved { value, source: layer.source.clone(), overridden: Vec::new() },
                    );
                }
            }
        }
    }
    preserve_frm_render_distance(&mut out);
    out
}

/// The user's overrides file, inside `profile_dir`.
///
/// `VANTA_FLAGS` still overrides it outright, which is how a one-off
/// experiment points at a file of its own. That override is global by nature —
/// it makes one file serve every profile — so it is a development switch, not a
/// supported arrangement.
pub fn user_path_in(profile_dir: &Path) -> PathBuf {
    // Delegated so the settings window, which cannot depend on this crate, can
    // name the identical file. See `vanta_plugins::flag_document::path_in`.
    vanta_plugins::flag_document::path_in(profile_dir)
}

/// The user's overrides file for the profile this instance is running.
pub fn user_path() -> PathBuf {
    user_path_in(&crate::profile::active())
}

/// Where the user's overrides lived before they were per profile.
pub fn legacy_user_path() -> PathBuf {
    config_dir().join("vanta/flags.json")
}

/// Move a pre-existing global overrides file into `profile_dir`, once.
///
/// Same guard and the same reasoning as `profile::migrate_legacy_layout` and
/// `vanta_plugins::grants::migrate_legacy_into`: only when the old file
/// exists and the new one does not, so it can never overwrite overrides already
/// made in this profile. Moved rather than copied, and into whichever profile
/// first goes looking, because there is no record of which profile a global
/// file was meant for — it was meant for all of them, which is the thing being
/// fixed. In practice that is `default`, where ADR-012's migration lands
/// existing storage.
pub fn migrate_legacy_user_file(profile_dir: &Path) -> Option<PathBuf> {
    let legacy = legacy_user_path();
    let target = user_path_in(profile_dir);
    if !legacy.is_file() || target.exists() {
        return None;
    }
    std::fs::create_dir_all(profile_dir).ok()?;
    match std::fs::rename(&legacy, &target) {
        Ok(()) => {
            println!(
                "  flags: moved {} to {} (ADR-013)",
                legacy.display(),
                target.display()
            );
            Some(target)
        }
        // A cross-device rename is the plausible failure. Leaving the old file
        // alone and saying so beats a half-written overrides document, which
        // would be reported as invalid JSON and ignored entirely.
        Err(e) => {
            println!(
                "  flags: could not move {} to {} ({e}); the old location is untouched",
                legacy.display(),
                target.display()
            );
            None
        }
    }
}

/// The **writable** plugin directory: where a plugin's own overrides live, one
/// subdirectory per plugin id. A plugin owns its file and nothing else writes
/// to it, so removing the plugin removes its flags.
///
/// This is the user's directory specifically, and the write path uses it
/// directly rather than searching — [`plugin_dirs`] is for *discovery*, and its
/// first entry is read-only.
pub fn plugin_dir() -> PathBuf {
    std::env::var_os("VANTA_PLUGIN_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| data_dir().join("vanta/plugins"))
}

/// Where first-party plugins ship, read-only, installed alongside the binary.
///
/// Delegates to [`vanta_plugins::manifest::system_plugin_root`], which is now
/// the single definition: the settings window has to list built-in plugins
/// beside user ones, and it reaches `vanta-plugins` rather than this crate.
/// This wrapper stays so the two-tier search path below still reads as one
/// idea in one place.
pub fn system_plugin_dir() -> PathBuf {
    vanta_plugins::manifest::system_plugin_root()
}

/// Every directory plugins are discovered from, **system first**.
///
/// The same two-tier arrangement GNOME Shell shows as "System Extensions" and
/// "User-Installed Extensions", and Flatpak as system and user installations.
///
/// **A user plugin may not shadow a first-party one, and this is deliberate.**
/// The usual XDG convention lets the user directory win, but a first-party
/// plugin ships a trusted opinion — the built-in web-view interceptor decides
/// that a Studio link means the software centre — and a same-id directory in a
/// writable location silently replacing that is an impersonation route, not a
/// customisation. So on a collision the system copy is used and the conflict is
/// **reported by name** rather than resolved quietly. See [`collect`].
///
/// Disabling a first-party plugin is a different thing and is built:
/// `vanta_plugins::enablement` records it per profile, the settings window
/// has a switch for every plugin in both tiers plus a master one, and
/// [`collect`] below skips a disabled plugin's layer. Refusing to shadow was
/// never a substitute for that, and no longer has to be.
pub fn plugin_dirs() -> Vec<PathBuf> {
    vec![system_plugin_dir(), plugin_dir()]
}

/// How large one plugin's own `flags.json` may grow.
///
/// Nothing here validates the *keys* a plugin sends — only Roblox's own C++
/// knows which FastFlag names are real, and an unrecognised one simply
/// resolves to nothing, harmlessly. The size cap exists for the reason
/// `settings.rs`'s does: an ordinary bug that keeps appending values must not
/// be able to fill a plugin's directory silently just because nothing else
/// was watching.
const MAX_PLUGIN_FLAGS_BYTES: usize = 256 * 1024;

/// Replace plugin `id`'s own `flags.json` with `values` — the effect behind
/// the `flags.write` capability (`plugin_host.rs`'s `dispatch`).
///
/// A whole-document replace, not a merge, for the same reason
/// `settings::Store::write` is: a plugin is the only writer of its own file,
/// so it always knows the complete set it means to leave in place, and a
/// merge would give it no way to withdraw a flag it has stopped wanting
/// overridden. Written to a sibling file and renamed in — the same
/// atomic-write shape every other per-plugin document in this project
/// uses — so a process killed mid-write leaves the previous, valid document
/// rather than one `read_layer` has to report and skip.
///
/// Takes effect at the **next launch only**: `FFlag`/`FInt`/`FString` are
/// read once during `nativeInitClientSettings`, and this file is one layer
/// `collect` reads before the engine process starts (see the module note) —
/// there is no live effect here to have, which is the whole reason
/// `flags.write` and `flags.write.dynamic` are two capabilities rather than
/// one (ADR-005).
pub fn write_plugin_layer(id: &str, values: &BTreeMap<String, String>) -> Result<(), String> {
    if !vanta_plugins::manifest::is_valid_id(id) {
        return Err(format!("{id:?} is not a usable plugin id"));
    }
    let text = serde_json::to_string_pretty(values).map_err(|e| e.to_string())?;
    if text.len() > MAX_PLUGIN_FLAGS_BYTES {
        return Err(format!(
            "{} bytes of flags is more than the {MAX_PLUGIN_FLAGS_BYTES} byte limit",
            text.len()
        ));
    }
    let dir = plugin_dir().join(id);
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let path = dir.join("flags.json");
    let tmp = path.with_extension("json.new");
    std::fs::write(&tmp, text).map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, &path).map_err(|e| format!("{}: {e}", path.display()))
}

/// Parse the text of a user flags document, the way [`read_layer`] would.
///
/// Delegates to [`vanta_plugins::flag_document::parse`], which is where the
/// format lives so that the settings window can use the identical rule --
/// `vanta-runtime` depends on `vanta-shell`, so the window cannot depend
/// back on this crate. That module's header has the argument in full; the
/// short version is that two implementations of "is this document valid" is
/// how a window comes to say "Saved 40 flags" about a file the client then
/// ignores.
pub fn parse_user_text(text: &str) -> Result<BTreeMap<String, String>, String> {
    vanta_plugins::flag_document::parse(text)
}

/// Replace the user's own `flags.json` for `profile_dir`.
pub fn write_user_layer(profile_dir: &Path, values: &BTreeMap<String, String>) -> Result<(), String> {
    vanta_plugins::flag_document::write(&user_path_in(profile_dir), values)
}

fn config_dir() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(std::env::temp_dir)
}

fn data_dir() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .unwrap_or_else(std::env::temp_dir)
}

/// Flags Vanta sets for itself, at the bottom of the stack.
///
/// **Empty, and the reason it is empty is worth keeping.**
///
/// It briefly carried `FStringGraphicsTextureManager2DenyPattern2 = ".*"`, taken
/// from a mocktail commit titled "Another fix for low quality texture" and
/// shipped here as a default on the strength of that title plus the flag's
/// absence from Roblox's own settings document. It was marked `INFERRED`,
/// because what was established was the flag's absence and its effect on
/// mocktail — never that it improved anything here.
///
/// Denying every pattern takes the engine off TextureManager 2 and onto the
/// legacy path, which a real session confirmed: the log prints
/// `[FLog::Graphics] Using TM1`. What that cost, reported from play, was a lobby
/// that refused to render and loading that was visibly slow. TM2 is the modern
/// manager and streams textures; the legacy path does not, so a large place pays
/// twice.
///
/// **And the blurring it was meant to fix has never been seen here.** Not once,
/// by anyone, on Vanta. The entire case for the flag was another project's
/// commit title. Whatever they were fixing may be specific to their stack, or
/// may have been something else with a texture-shaped name; either way this was
/// a remedy shipped for a symptom nobody in this project had reported.
///
/// One further observation, and it is why this is not filed as "TM1 is bad":
/// fullscreening the window made the lobby render correctly *with the deny in
/// place*. That points at the viewport rather than the texture manager, so the
/// render failure and the manager may not be the same fact at all. Untangling
/// that needs a side-by-side nobody has done.
///
/// The mistake was not adopting the flag. It was making an `INFERRED` change a
/// **default**. An inference belongs behind a switch somebody chooses, where
/// being wrong costs that person an experiment; as a default it costs everybody
/// a worse client and nobody knows why. Anything added here in future should be
/// something measured on this project's own hardware, not something read off
/// another project's commit title.
///
/// The flag itself still works through the ordinary layers — a user or plugin
/// can set it in `flags.json` — so nothing is lost except the default.
///
/// ## The one entry, and why it is Vanta's rather than a plugin's
///
/// `FFlagUserLaunchedWithBloxstrap` is how a game finds out that the launcher
/// it is running under implements BloxstrapRPC. An experience reads it with
/// `UserSettings():IsUserFeatureEnabled("UserLaunchedWithBloxstrap")` and
/// decides whether to bother printing presence at all, so without it a game
/// that gates on it stays silent and Vanta's whole BloxstrapRPC reader has
/// nothing to read.
///
/// **It is not an engine flag.** `LaunchedWithBloxstrap` does not appear
/// anywhere in `libroblox.so`; it is a name the launcher injects and the game
/// asks about. That it can be injected at all is established rather than
/// assumed — `client_settings`'s own call table records
/// `{"applicationSettings":{"FFlagNotARealFlag":"True"}}` returning `0`, so
/// the engine takes names it does not define.
///
/// **This belongs to Vanta and not to `discord-presence`.** The claim is
/// about the launcher, and it is Vanta's runtime that makes it true:
/// `bloxstrap_rpc` parses the protocol and `game_log` publishes
/// `vanta/game.presence` to whatever is listening, with no presence plugin
/// involved. Putting it in a plugin would mean a capability of the runtime
/// switching off when an unrelated plugin was disabled, two presence plugins
/// each setting it, and `discord-presence` needing `flags.write` for something
/// outside what it does -- ADR-007 brokers *effects*, and "tell games this
/// launcher speaks a protocol" is not a presence effect. `hide-gui` sets a
/// flag because that flag is its entire feature; this one is not.
///
/// **The name is a small lie and worth saying so.** Vanta is not Bloxstrap.
/// The flag is a de-facto protocol handshake that happens to be named after
/// the launcher that invented it, in the way a User-Agent claims Mozilla
/// compatibility, and no other name exists to send. The trade is that a game
/// can now tell it is running under a third-party launcher; that is a
/// fingerprinting signal and the reason this is worth a sentence rather than
/// a silent default. It is the lowest-precedence layer, so
/// `"FFlagUserLaunchedWithBloxstrap": "False"` in `flags.json` turns it off.
///
/// **INFERRED that it takes effect.** What is established is that the flag is
/// not the engine's, that the engine accepts unknown names, and that games ask
/// for it. That a game's `IsUserFeatureEnabled` then answers true has not been
/// seen here. Bloxstrap's own RPC test place reports it on a board and is the
/// instrument; until somebody stands in it and reads YES, this entry is a
/// reasoned guess and the paragraph above about measured-not-inferred applies
/// to it as much as to anything else.
const BUILTIN: &[(&str, &str)] = &[("FFlagUserLaunchedWithBloxstrap", "True")];

/// How hard to push the engine's own worker pools, as a choice rather than a
/// default.
///
/// Roblox exposes several thread-count and pipeline flags that mocktail's
/// `src/runtime/performance_policy.cc` scales to the machine (Apache-2.0; the
/// policy shape is adapted here, not its code). They are offered as modes for
/// the reason [`BUILTIN`] gives at length: an inference belongs behind a switch
/// somebody chooses, where being wrong costs that person an experiment, rather
/// than as a default where it costs everybody a worse client and nobody knows
/// why.
///
/// **None of these are measured on this project's hardware yet.** Two of them --
/// `FIntTaskSchedulerAutoThreadLimit` and `FIntTaskSchedulerThreadMin` -- were
/// tested against CPU on 2026-08-21 and moved it not at all, but CPU is not what
/// they are for and the frame rate was never measured. Until somebody runs the
/// comparison, [`Performance::Balanced`] sets nothing and is the default, so the
/// shipped behaviour is exactly what it was.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Performance {
    /// Desktop-tuned moderate scaling. Default.
    #[default]
    Balanced,
    /// More worker threads and parallel prerender, at the cost of using more of
    /// the machine. Aimed at a desktop with cores to spare.
    Throughput,
    /// Fewer threads and a smaller physics batch, for a machine where latency
    /// matters more than throughput or where cores are scarce.
    Latency,
    /// Aggressive weak/older-CPU optimisation bundle (libroblox.so-verified):
    /// capped workers, FRM QL 1 with preserved render distance, trimmed
    /// shadows/post-FX/textures/grass/particles/terrain/audio, and lighter
    /// Vulkan pooling. All flags sit in `Source::Performance` so user
    /// `flags.json` overrides still win.
    Speed,
}

/// The flags a mode asks for, given how many *physical* cores the machine has.
///
/// Physical rather than logical, and that distinction is not pedantry: on a
/// hybrid part, sizing a render worker pool by thread count spreads work onto
/// cores that are slower per thread, which is the opposite of the intent.
///
/// **Corrected.** This used to describe the machine it was written on as "six
/// performance cores and eight efficiency cores reporting sixteen threads".
/// That part is an i7-13620H and it is 6P+**4**E: twelve threads from six
/// hyperthreaded P-cores plus four unshared E-cores, so **ten** physical cores
/// and sixteen threads. Established by walking the same sysfs topology
/// [`physical_cores`] itself reads -- `cpu0..cpu11` give core_ids 0,4,8,12,16,20
/// twice each and `cpu12..cpu15` give 24,25,26,27 once each, ten distinct
/// (package, core) pairs -- and confirmed against `lscpu`, which reports
/// `Core(s) per socket: 10` and `CPU(s): 16`.
///
/// The function was always right; only this comment was wrong, and it was wrong
/// by four cores in the direction that matters, because every mode below sizes
/// its worker pools off this number. Anyone reasoning about those tables from
/// the comment rather than from the machine was reasoning from 14.
///
/// Pure, so the table can be tested without a machine to run it on.
pub fn performance_flags(mode: Performance, physical_cores: usize) -> Vec<(String, String)> {
    let workers = physical_cores.max(1);
    let owned = |k: &str, v: String| (k.to_string(), v);
    match mode {
        // Desktop-tuned balanced default: moderate multi-core scaling and
        // desktop Vulkan pooling without saturating every core.
        Performance::Balanced => vec![
            owned("FIntTaskSchedulerThreadMin", "0".into()),
            owned("FIntTaskSchedulerAsyncTasksMinimumThreadCount", workers.min(2).to_string()),
            owned("FIntTaskSchedulerAutoThreadLimit", workers.min(8).to_string()),
            owned("FIntSmoothClusterTaskQueueMaxParallelTasks", workers.div_ceil(2).max(1).to_string()),
            owned("FIntSmoothClusterTaskQueueMsPerCycle", "4".into()),
            owned("FIntOcclusionWorkerThreadCount", workers.div_ceil(4).max(1).to_string()),
            owned("FFlagMovePrerenderV2", "True".into()),
            owned("DFIntSimMidPhaseContactPipelineBatchSize", "64".into()),
            owned("FFlagGraphicsVulkanExtendedDynamicState", "True".into()),
            owned("FFlagVulkanAllocatorPoolSmallOnly", "False".into()),
            owned("FIntVulkanAllocatorKeepEmptyPages", "8".into()),
        ],
        // High-core desktop scaling: full physical-core concurrency across
        // TaskScheduler, cluster meshing, occlusion, parallel animators, and
        // larger physics batches.
        Performance::Throughput => vec![
            owned("FIntTaskSchedulerThreadMin", "0".into()),
            owned("FIntTaskSchedulerAsyncTasksMinimumThreadCount", workers.min(3).to_string()),
            owned("FIntTaskSchedulerAutoThreadLimit", workers.to_string()),
            owned("FIntSmoothClusterTaskQueueMaxParallelTasks", workers.to_string()),
            owned("FIntSmoothClusterTaskQueueMsPerCycle", "8".into()),
            owned("FIntOcclusionWorkerThreadCount", workers.div_ceil(2).max(1).to_string()),
            owned("FFlagMovePrerenderV2", "True".into()),
            owned("FFlagAnimatorParallelManagerUseLiveTaskSchedulerConcurrency", "True".into()),
            owned("FFlagHumanoidParallelManagerUseLiveTaskSchedulerConcurrency", "True".into()),
            owned("DFIntSimMidPhaseContactPipelineBatchSize", "128".into()),
            owned("FFlagGraphicsVulkanExtendedDynamicState", "True".into()),
            owned("FFlagVulkanAllocatorPoolSmallOnly", "False".into()),
            owned("FIntVulkanAllocatorKeepEmptyPages", "12".into()),
        ],
        // Low-latency mode: lower worker thread counts and a smaller physics
        // contact batch to minimise thread contention and frame jitter.
        Performance::Latency => vec![
            owned("FIntTaskSchedulerThreadMin", "0".into()),
            owned("FIntTaskSchedulerAsyncTasksMinimumThreadCount", "1".into()),
            owned("FIntTaskSchedulerAutoThreadLimit", workers.clamp(1, 4).to_string()),
            owned("FIntSmoothClusterTaskQueueMaxParallelTasks", "1".into()),
            owned("FIntSmoothClusterTaskQueueMsPerCycle", "2".into()),
            owned("FIntOcclusionWorkerThreadCount", "1".into()),
            owned("FFlagMovePrerenderV2", "True".into()),
            owned("DFIntSimMidPhaseContactPipelineBatchSize", "32".into()),
            owned("FFlagGraphicsVulkanExtendedDynamicState", "True".into()),
            owned("FFlagVulkanAllocatorPoolSmallOnly", "False".into()),
            owned("FIntVulkanAllocatorKeepEmptyPages", "4".into()),
        ],
        // Weak/old-CPU Speed mode: 100% libroblox.so-verified FastFlag bundle.
        Performance::Speed => vec![
            // CPU / TaskScheduler
            owned("FIntTaskSchedulerThreadMin", "0".into()),
            owned("FIntTaskSchedulerAutoThreadLimit", workers.clamp(1, 4).to_string()),
            owned("FIntTaskSchedulerAsyncTasksMinimumThreadCount", "1".into()),
            owned("FIntSmoothClusterTaskQueueMaxParallelTasks", "1".into()),
            owned("FIntSmoothClusterTaskQueueMsPerCycle", "1".into()),
            owned("FIntOcclusionWorkerThreadCount", "1".into()),
            owned("DFIntDebugVoxelJobManagerMaxThreads", "1".into()),
            owned("DFIntVoxelJobManagerDMJobTimeMicroSeconds", "1000".into()),
            owned("DFIntVoxelJobManagerRenderThreadJobTimeMicroSeconds", "1000".into()),
            owned("DFIntSimMidPhaseContactPipelineBatchSize", "32".into()),
            owned("FFlagMovePrerenderV2", "True".into()),
            owned("FFlagAnimatorParallelManagerUseLiveTaskSchedulerConcurrency", "True".into()),
            owned("FFlagHumanoidParallelManagerUseLiveTaskSchedulerConcurrency", "True".into()),
            // FRM Quality Lock (with Render Distance Preserved!) & Post-FX / Shadows / Lighting
            owned("DFIntDebugFRMQualityLevelOverride", "1".into()),
            owned("FFlagFRMLockstepOverridesEffective", "False".into()),
            owned("FIntDebugFRMOptionalMSAALevelOverride", "0".into()),
            owned("FIntBloomFrmCutoff", "22".into()),
            owned("FFlagFRMDisableCloudsAtLowQL", "True".into()),
            owned("DFFlagRenderConstantFogAtLowQL", "True".into()),
            owned("DFIntRenderConstantFogAtLowQLQLCutoff", "21".into()),
            owned("DFIntRenderPostFxBasePixelCount", "480000".into()),
            owned("FFlagRenderShadowResolutionScaling", "True".into()),
            owned("FStringRenderShadowResolutionQualityByRam", "1024,3,2048,3,4096,3".into()),
            owned("FFlagRenderAllocateShadowMapResourcesOnDemand", "True".into()),
            owned("FIntTM2ShadowMapMaxMips", "6".into()),
            owned("FIntTM2EnvMapMaxMips", "6".into()),
            owned("FIntEnvMapRaycastCount", "4".into()),
            owned("FIntEnvMapMaxRayLength", "64".into()),
            owned("FFlagRenderWaterDemoteSHQ", "True".into()),
            // Textures / Grass / Terrain / Particles / Audio / Vulkan
            owned("DFFlagDebugForceAnisoOff", "True".into()),
            owned("DFIntRenderClampRoughnessMax", "1".into()),
            owned("FIntDebugTextureManagerSkipMips", "1".into()),
            owned("DFIntDynamicTextureManagerPrepareBudgetMicroSeconds", "500".into()),
            owned("FIntFRMMinGrassDistance", "0".into()),
            owned("FIntFRMMaxGrassDistance", "0".into()),
            owned("FIntRenderGrassDensityMax", "0".into()),
            owned("FIntRenderGrassDensityMin", "0".into()),
            owned("DFFlagFRMUseParticleCountBudget", "True".into()),
            owned("DFFlagFRMUseGlobalParticleCountBudget", "True".into()),
            owned("DFIntFRMParticleBudgetMinLerp", "64".into()),
            owned("DFIntFRMGlobalParticleBudgetMinLerp", "256".into()),
            owned("FFlagRenderParticlesCapNonVisibleEmission", "True".into()),
            owned("FFlagRenderParticlesNonVisibleSimBudget", "True".into()),
            owned("DFIntRenderParticlesNonVisibleSimBudgetMicros", "100".into()),
            owned("DFIntParticlesFramesBetweenNonVisibleUpdates", "12".into()),
            owned("DFIntTerrainLodActive", "4".into()),
            owned("DFIntTerrainLodLevels", "2".into()),
            owned("DFIntTerrainPow2LodBiasPercent", "50".into()),
            owned("FFlagAudioEngineThrottleSoundServiceStep", "True".into()),
            owned("FIntAudioEngineAudibilityDepthLimit", "6".into()),
            owned("FFlagGraphicsVulkanExtendedDynamicState", "True".into()),
            owned("FFlagVulkanAllocatorPoolSmallOnly", "False".into()),
            owned("FIntVulkanAllocatorKeepEmptyPages", "4".into()),
        ],
    }
}

/// The environment variable the shell sets to choose a [`Performance`] mode.
///
/// Named and passed the same way [`DEVICE_PROFILE_ENV`] and `VANTA_GRAPHICS`
/// are, and for the same reason all three are environment rather than a file:
/// `FInt`/`FFlag` overrides are read once during engine startup, so the mode
/// has to be settled before the process starts. `crates/vanta-shell`'s
/// `launch.rs` sets it, and omits it entirely for the default -- an absent
/// variable is what leaves a plugin room to have an opinion, which is the
/// argument that block already makes about `VANTA_GRAPHICS`.
pub const PERFORMANCE_ENV: &str = "VANTA_PERFORMANCE";

impl Performance {
    pub fn parse(text: &str) -> Option<Performance> {
        match text.trim().to_ascii_lowercase().as_str() {
            "" | "balanced" => Some(Performance::Balanced),
            "throughput" => Some(Performance::Throughput),
            "latency" => Some(Performance::Latency),
            "speed" => Some(Performance::Speed),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Performance::Balanced => "balanced",
            Performance::Throughput => "throughput",
            Performance::Latency => "latency",
            Performance::Speed => "speed",
        }
    }
}

/// Which mode is in force, from [`PERFORMANCE_ENV`].
///
/// An unparseable value is reported and treated as [`Performance::Balanced`]
/// rather than silently ignored, matching `graphics::resolve` and
/// [`device_profile`]: a switch that looks set and does nothing is the failure
/// this project keeps writing rules against.
pub fn performance() -> Performance {
    let Ok(text) = std::env::var(PERFORMANCE_ENV) else {
        return Performance::Balanced;
    };
    if text.trim().is_empty() {
        return Performance::Balanced;
    }
    match Performance::parse(&text) {
        Some(m) => m,
        None => {
            println!(
                "  flags: {PERFORMANCE_ENV}={text:?} is not a performance mode; using balanced. \
                 Known: balanced, throughput, latency, speed"
            );
            Performance::Balanced
        }
    }
}

/// The layer [`performance`]'s mode contributes, empty on `Balanced`.
///
/// **Until this existed, [`performance_flags`] had no callers at all** — the
/// table, its tests and its whole doc comment were in the tree and nothing
/// ever asked for a flag from it, so choosing a mode was not possible by any
/// route. That is the "control that reports success and does not act" shape
/// this codebase keeps finding in its own settings pages, one layer down.
fn performance_layer() -> Layer {
    let mode = performance();
    let values: BTreeMap<String, String> =
        performance_flags(mode, physical_cores()).into_iter().collect();
    if !values.is_empty() {
        println!(
            "  flags: performance mode {} sets {} flag(s) for {} physical core(s)",
            mode.label(),
            values.len(),
            physical_cores()
        );
    }
    Layer { source: Source::Performance, values }
}

/// The Settings row's frame-rate cap, alongside the display's own refresh and
/// FIFO/MAILBOX ([`crate::android::vulkan`]) as the third lever `fastflags.md`
/// says raising the frame rate takes.
///
/// **The reset this has to survive, established rather than assumed.**
/// `client_settings.rs`'s `apply_overrides` doc comment and
/// `docs/analysis/flag-init.md` §47 measure the mechanism: the engine's own
/// `DynamicFastVariableReloader` fetches Roblox's settings document again and
/// *reapplies it*, which reverts a `DF*` override -- `DFIntTaskSchedulerTargetFps`
/// among them -- for a key that document contains. Logs of earlier runs
/// (signed out as well as signed in) show it finishing at about 120, 241 and
/// 362 seconds. `FIntTaskSchedulerTargetFps`, the same name in the durable
/// family, was measured to do nothing (§49), so there is no route around the
/// reloader for this flag.
///
/// The cure is not specific to this flag, so it does not live here:
/// [`crate::flag_reapply`] puts every user and plugin override back after each
/// refresh, and this layer is simply one more source of overrides for it to
/// keep (ADR-051).
///
/// There is no "unlimited" choice. The first draft had one, sending 9999, which
/// was extrapolated: §49 measured this flag tracking 10, 15 and 20 and nothing
/// above 45, and nobody has seen what the engine does with a cap that large. The
/// only measurement of a very high value (headless, 60 Hz output) presented at
/// about 36/s against 60/s at the display's own rate, which is a statement about
/// that environment rather than a safety result. A finite number says what is
/// being asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FrameRateLimit {
    /// Sets nothing. What Vanta has always done -- the engine's own default,
    /// which tracks the display refresh closely enough in ordinary use that
    /// nobody filed it as a cap until they went looking for one.
    #[default]
    Display,
    /// `DFIntTaskSchedulerTargetFps` set to this value, and kept there.
    Cap(u32),
}

impl FrameRateLimit {
    pub fn parse(text: &str) -> Option<FrameRateLimit> {
        match text.trim().to_ascii_lowercase().as_str() {
            "" | "display" | "auto" => Some(FrameRateLimit::Display),
            "uncapped" | "unlimited" => Some(FrameRateLimit::Cap(999)),
            other => other.parse::<u32>().ok().filter(|n| *n > 0).map(FrameRateLimit::Cap),
        }
    }

    pub fn label(self) -> String {
        match self {
            FrameRateLimit::Display => "display refresh".into(),
            FrameRateLimit::Cap(999) => "uncapped (999)".into(),
            FrameRateLimit::Cap(n) => format!("{n}"),
        }
    }
}

/// The environment variable the shell sets to choose a [`FrameRateLimit`],
/// following the same reasoning [`PERFORMANCE_ENV`]'s doc gives: the choice has
/// to be settled before the first settings document is built, and an absent
/// variable is what leaves a plugin room to have its own opinion through
/// [`FRAME_RATE_LIMIT_KEY`].
pub const FRAME_RATE_LIMIT_ENV: &str = "VANTA_FRAME_RATE_LIMIT";

/// The flag-layer key a plugin may set instead, on the same footing
/// `android::vulkan::PRESENT_MODE_KEY` gives `VantaPresentMode`: a
/// `Vanta`-prefixed name that rides this module's layering for precedence
/// and provenance and that `client_settings.rs::is_roblox_flag` filters back
/// out before anything reaches Roblox's settings document.
pub const FRAME_RATE_LIMIT_KEY: &str = "VantaFrameRateLimit";

/// What the shell's `settings.set` last asked for, if it has asked (ADR-044).
/// Beats the environment, because the environment is only what the client was
/// launched with and the message is the user changing their mind.
static LIVE_FRAME_RATE_LIMIT: std::sync::Mutex<Option<FrameRateLimit>> = std::sync::Mutex::new(None);

/// Records a live choice and returns whether it differs from the one in force
/// before it, which is the caller's cue to re-apply. `None` clears it, which
/// the tests use and nothing else does.
pub fn set_live_frame_rate_limit(choice: Option<FrameRateLimit>) -> bool {
    let before = frame_rate_limit();
    *LIVE_FRAME_RATE_LIMIT.lock().unwrap_or_else(|e| e.into_inner()) = choice;
    frame_rate_limit() != before
}

/// Which choice is in force: a live change first, then the environment (what
/// the shell sets at launch and a measurement run sets by hand), then the flag
/// layers, then [`FrameRateLimit::Display`] -- the order [`performance`] and
/// `android::vulkan::resolve_present_mode` already use, plus the socket on top.
fn frame_rate_limit_from(resolved: &BTreeMap<String, Resolved>) -> FrameRateLimit {
    if let Some(choice) = *LIVE_FRAME_RATE_LIMIT.lock().unwrap_or_else(|e| e.into_inner()) {
        return choice;
    }
    if let Ok(text) = std::env::var(FRAME_RATE_LIMIT_ENV) {
        if !text.trim().is_empty() {
            return match FrameRateLimit::parse(&text) {
                Some(choice) => choice,
                None => {
                    println!(
                        "  flags: {FRAME_RATE_LIMIT_ENV}={text:?} is not a frame rate limit; \
                         using display refresh. Known: display, or a number of fps"
                    );
                    FrameRateLimit::Display
                }
            };
        }
    }
    match resolved.get(FRAME_RATE_LIMIT_KEY).and_then(|r| FrameRateLimit::parse(&r.value)) {
        Some(choice) => choice,
        None => FrameRateLimit::Display,
    }
}

pub fn frame_rate_limit() -> FrameRateLimit {
    if let Some(choice) = *LIVE_FRAME_RATE_LIMIT.lock().unwrap_or_else(|e| e.into_inner()) {
        return choice;
    }
    if let Ok(text) = std::env::var(FRAME_RATE_LIMIT_ENV) {
        if !text.trim().is_empty() {
            return match FrameRateLimit::parse(&text) {
                Some(choice) => choice,
                None => {
                    println!(
                        "  flags: {FRAME_RATE_LIMIT_ENV}={text:?} is not a frame rate limit; \
                         using display refresh. Known: display, or a number of fps"
                    );
                    FrameRateLimit::Display
                }
            };
        }
    }
    let resolved = resolve(collect_without_frame_rate_limit());
    match resolved.get(FRAME_RATE_LIMIT_KEY).and_then(|r| FrameRateLimit::parse(&r.value)) {
        Some(choice) => choice,
        None => FrameRateLimit::Display,
    }
}

/// The `DFIntTaskSchedulerTargetFps` override a choice asks for, empty on
/// [`FrameRateLimit::Display`] -- exactly [`performance_flags`]'s shape.
pub fn frame_rate_limit_flags(choice: FrameRateLimit) -> Vec<(String, String)> {
    match choice {
        FrameRateLimit::Display => Vec::new(),
        FrameRateLimit::Cap(n) => {
            vec![("DFIntTaskSchedulerTargetFps".to_string(), n.to_string())]
        }
    }
}

/// [`frame_rate_limit_flags`] with the display's rate in hand, so that
/// [`FrameRateLimit::Display`] can mean what it says.
///
/// **Display used to set nothing and the engine did not follow the display.**
/// On a 120 Hz output with `nativePassCurrentDisplayRefreshRate 120` in the log
/// (`refresh.rs`), a 2.742 client sat at 60 until a number was chosen by hand;
/// the engine takes the rate as information, and its scheduler target is a
/// separate flag. Above 60 the target is the rate itself under a queueing
/// present mode, and [`crate::refresh::REPLACING_PRESENT_HEADROOM`] times it
/// under MAILBOX or IMMEDIATE, where a target of exactly the rate left one
/// refresh in five showing a repeated frame; at 60 or below, or before the
/// rate is known, it is still nothing.
pub fn frame_rate_limit_flags_for(
    choice: FrameRateLimit,
    display_hz: Option<f32>,
    present_replaces_frames: bool,
) -> Vec<(String, String)> {
    match choice {
        FrameRateLimit::Display => match crate::refresh::target_fps_with_headroom(display_hz, present_replaces_frames) {
            Some(fps) => vec![("DFIntTaskSchedulerTargetFps".to_string(), fps.to_string())],
            None => Vec::new(),
        },
        FrameRateLimit::Cap(_) => frame_rate_limit_flags(choice),
    }
}

/// The layer [`frame_rate_limit`]'s choice contributes, empty on `Display`
/// until the display's rate is known and above 60.
/// Above the plugins and below the user's own `flags.json`; see [`collect`].
fn frame_rate_limit_layer_from(resolved: &BTreeMap<String, Resolved>) -> Layer {
    let choice = frame_rate_limit_from(resolved);
    // When Settings is left on `Display` and an enabled plugin explicitly set
    // `DFIntTaskSchedulerTargetFps`, defer to the plugin rather than clobbering
    // it with an auto-derived display target.
    if choice == FrameRateLimit::Display
        && matches!(
            resolved.get("DFIntTaskSchedulerTargetFps").map(|r| &r.source),
            Some(Source::Plugin(_))
        )
    {
        return Layer { source: Source::FrameRateLimit, values: BTreeMap::new() };
    }
    let replaces = present_replaces_frames_from(resolved);
    let values: BTreeMap<String, String> =
        frame_rate_limit_flags_for(choice, crate::refresh::announced(), replaces)
            .into_iter()
            .collect();
    if !values.is_empty() {
        println!("  flags: frame rate limit set to {} ({} flag(s))", choice.label(), values.len());
    }
    Layer { source: Source::FrameRateLimit, values }
}

fn present_replaces_frames_from(resolved: &BTreeMap<String, Resolved>) -> bool {
    let from_flags = resolved
        .get(crate::android::vulkan::PRESENT_MODE_KEY)
        .map(|entry| (entry.source.describe(), entry.value.clone()));
    crate::android::vulkan::present_mode_replaces_frames(
        std::env::var(crate::android::vulkan::PRESENT_MODE_ENV).ok(),
        from_flags,
    )
}

/// Whether the present mode the environment and flag layers ask for replaces
/// frames, read from [`collect_without_frame_rate_limit`] for the reason that
/// function exists.
pub fn present_replaces_frames() -> bool {
    let resolved = resolve(collect_without_frame_rate_limit());
    present_replaces_frames_from(&resolved)
}

/// [`collect`] without the frame-rate-limit layer itself, so [`frame_rate_limit`]
/// can read the plugin/user layers for [`FRAME_RATE_LIMIT_KEY`] without calling
/// itself -- `collect` calls [`frame_rate_limit_layer`], which calls
/// [`frame_rate_limit`], and a plain `collect()` call here would recurse.
fn collect_without_frame_rate_limit() -> Vec<Layer> {
    let mut layers = vec![builtin_layer(), performance_layer()];
    let profile = crate::profile::active();
    let mut seen: std::collections::BTreeMap<String, PathBuf> = Default::default();
    for root in plugin_dirs() {
        let mut ids: Vec<String> = std::fs::read_dir(&root)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.path().is_dir())
            .filter_map(|e| e.file_name().into_string().ok())
            .collect();
        ids.sort();
        for id in ids {
            if !seen.contains_key(&id) {
                seen.insert(id, root.clone());
            }
        }
    }
    for (id, root) in seen {
        if !vanta_plugins::enablement::is_enabled(&profile, &id) {
            continue;
        }
        let shipped = root.join(&id).join("flags.json");
        let path = if shipped.is_file() { shipped } else { plugin_dir().join(&id).join("flags.json") };
        if let Some(layer) = read_layer(&path, Source::Plugin(id)) {
            layers.push(layer);
        }
    }
    if let Some(layer) = read_layer(&user_path(), Source::User) {
        layers.push(layer);
    }
    layers
}

/// Physical cores, counted from the kernel's own topology.
///
/// `available_parallelism` reports threads, which is the wrong number here. Each
/// CPU's `topology/core_id` is unique per physical core within a package, so the
/// distinct (package, core) pairs are the count. Falls back to the thread count
/// when `sysfs` is unreadable, which is the only honest answer available then.
pub fn physical_cores() -> usize {
    static CACHED: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *CACHED.get_or_init(|| {
        let mut seen = std::collections::BTreeSet::new();
        if let Ok(dir) = std::fs::read_dir("/sys/devices/system/cpu") {
            for entry in dir.flatten() {
                let base = entry.path().join("topology");
                let read = |name: &str| std::fs::read_to_string(base.join(name)).ok();
                if let (Some(pkg), Some(core)) =
                    (read("physical_package_id"), read("core_id"))
                {
                    seen.insert((pkg.trim().to_string(), core.trim().to_string()));
                }
            }
        }
        // Cgroups and affinity can leave this process fewer runnable threads than
        // the host-wide sysfs topology lists. Do not size engine worker pools above
        // that effective limit.
        let available = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
        if seen.is_empty() {
            available
        } else {
            seen.len().min(available)
        }
    })
}

fn builtin_layer() -> Layer {
    Layer {
        source: Source::Builtin,
        values: BUILTIN.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
    }
}

/// Every layer, in precedence order: Vanta's own defaults first so anything
/// can overrule them, then plugins (alphabetically, so the outcome does not
/// depend on directory iteration order), then the user.
pub fn collect() -> Vec<Layer> {
    // The move happens here because this is the one path every reader of the
    // user's overrides goes through, and the runtime has no single startup hook
    // that a launcher-started client and a hand-started one both pass. Once per
    // process: `collect` runs again for every `flags.list` a plugin makes, and
    // re-stating a file that has already moved on each of those is noise in a
    // log people read.
    static MIGRATED: std::sync::Once = std::sync::Once::new();
    MIGRATED.call_once(|| {
        migrate_legacy_user_file(&crate::profile::active());
    });

    let mut layers = vec![builtin_layer(), performance_layer()];

    // System first, so a first-party id is claimed before the user directory is
    // read and a same-id user directory cannot take it. Sorted within each root
    // so the outcome does not depend on directory iteration order.
    let mut seen: std::collections::BTreeMap<String, PathBuf> = Default::default();
    for root in plugin_dirs() {
        let mut ids: Vec<String> = std::fs::read_dir(&root)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.path().is_dir())
            .filter_map(|e| e.file_name().into_string().ok())
            .collect();
        ids.sort();
        for id in ids {
            if let Some(kept) = seen.get(&id) {
                // Loud, and naming both paths, because the quiet version of
                // this is indistinguishable from the plugin simply working.
                println!(
                    "  plugins: ignoring {} because the id {id} is already \
                     provided by {} -- a user plugin may not replace a \
                     first-party one, only be disabled",
                    root.join(&id).display(),
                    kept.display()
                );
                continue;
            }
            seen.insert(id, root.clone());
        }
    }

    // A plugin switched off in Settings contributes no flags either. This was
    // the half the disable switch did not cover: `plugin_host::start_all`
    // consulted `enablement` and refused to spawn the process, while this
    // function went on reading the same plugin's `flags.json` and handing its
    // overrides to the engine -- so "off" meant "its code does not run, but its
    // opinions about the renderer still do". Nobody looking at the switch could
    // have guessed that, which makes it the same class of defect as a stub that
    // reports success.
    let profile = crate::profile::active();
    for (id, root) in seen {
        if !vanta_plugins::enablement::is_enabled(&profile, &id) {
            println!("  plugins: {id} is switched off; its flags.json is not read");
            continue;
        }
        let shipped = root.join(&id).join("flags.json");
        let path = if shipped.is_file() { shipped } else { plugin_dir().join(&id).join("flags.json") };
        if let Some(layer) = read_layer(&path, Source::Plugin(id)) {
            layers.push(layer);
        }
    }

    // The Settings row sits above the plugins and below the user's own file. The
    // row is the user speaking in the launcher, so it beats a plugin that
    // carries a default for the same flag (FPS Flex ships a frame rate cap); a
    // row left on Display refresh asks for nothing, which leaves the plugin
    // alone. The file is still the one thing that beats it.
    let user_layer = read_layer(&user_path(), Source::User);
    // Compute the frame-rate-limit layer from the layers already read in this
    // pass instead of rescanning plugin directories and sysfs twice more.
    let mut base_for_resolve = layers.clone();
    if let Some(ref u) = user_layer {
        base_for_resolve.push(u.clone());
    }
    let resolved_base = resolve(base_for_resolve);
    layers.push(frame_rate_limit_layer_from(&resolved_base));

    if let Some(layer) = user_layer {
        layers.push(layer);
    }
    layers
}

/// Which device Vanta claims to be when the engine asks — the Android
/// tablet identity it has always sent, or the PC one an experiment wants to
/// try.
///
/// **Why this lives here rather than being a plain `getenv` in the C++ that
/// uses it.** `docs/analysis/flag-init.md` §13 records that mocktail — a
/// comparable third-party client — stays connected to the same place Vanta
/// is disconnected from after 60.6s with reason 304, and that mocktail
/// presents `device profile=pc-windows-11 class=pc model="Windows 11 PC"`
/// where Vanta presents an Android tablet (`native/init_params.cpp`,
/// commit 6d8c280). Whether that causes the 304 is **not established** — it
/// is a difference between a client that works and one that does not, and
/// this makes it a switch rather than a rewrite, so the experiment can be run
/// with a control in the same session.
///
/// The key follows the convention `client_settings.rs`'s `VANTA_KEY_PREFIX`
/// set up: a `Vanta`-prefixed name rides this module's layering for its
/// precedence and provenance, and `client_settings.rs::is_roblox_flag`
/// filters it back out before anything reaches Roblox's settings document —
/// the engine has no idea this key exists, same as `VantaGraphicsBackend`.
///
/// **The gap this does not close.** Nothing yet turns a resolved
/// `VantaDeviceProfile` into something `native/init_params.cpp` can see.
/// The C++ side is a separately-compiled translation unit with no live call
/// into this module; the one existing bridge for a comparable case —
/// `VANTA_ENGINE_VERSION`, set with `std::env::set_var` right before the
/// init-params call — lives in `crates/vanta-runtime/src/bin/load.rs`,
/// which this change does not touch. So today `device_profile` is reachable
/// from a `flags.json` entry and from tests, but the only thing that actually
/// reaches the engine is the environment variable [`DEVICE_PROFILE_ENV`]
/// itself, read directly by `native/init_params.cpp`'s `presenting_as_pc`.
/// Wiring `flags.json` through to it is exactly the `load.rs` change
/// described above, left for whoever owns that file next.
pub const DEVICE_PROFILE_KEY: &str = "VantaDeviceProfile";

/// The environment variable that actually reaches the engine today, read
/// directly by `native/init_params.cpp`. Named to match [`DEVICE_PROFILE_KEY`]
/// so the two are recognisably the same switch, not two different ones.
pub const DEVICE_PROFILE_ENV: &str = "VANTA_DEVICE_PROFILE";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceProfile {
    /// The bare Roblox app token and nothing else, the shape Sober sends —
    /// `RobloxApp/<version>(GlobalDist; Vanta)`, with no `Mozilla`, no
    /// WebKit clause, no platform words and no device block.
    ///
    /// **Not the default.** It is the only one of the three that does not claim
    /// a form factor Vanta has not got, and that is an argument rather than a
    /// measurement; the default stays `PcWindows11` until a frame rate says
    /// otherwise. See
    /// `native/init_params.cpp`'s `device_identity` for the measurement that
    /// says what each identity makes roblox.com serve, and for the
    /// `WebViewUserAgent` value it was copied from.
    RobloxApp,
    /// An Android tablet: `ROBLOX Android App … Tablet Hybrid()`, plus the
    /// device block, plus `InitParams.isTablet = true`.
    ///
    /// **The only identity that asserts a mobile form factor to the engine**,
    /// through `isTablet` — which is a field the engine reads, unlike the
    /// User-Agent, whose effect on graphics tiering nothing here has
    /// established. Reach for this one when mobile-tier defaults are what is
    /// actually wanted, and measure it.
    AndroidTablet,
    /// mocktail's identity, spelled `pc-windows-11` to match its own log line
    /// verbatim rather than a name invented here. **The default, since
    /// 2026-08-20.**
    ///
    /// Worth knowing before choosing it: roblox.com reads this one as
    /// `data-app-type="uwp"` — a Microsoft Store app, which Vanta is not.
    PcWindows11,
    /// The Quest build behind an OpenXR session: platform name `MetaOS`, no
    /// keyboard or mouse, the bare app token for a User-Agent. **The default
    /// under `vanta-run --guest-arm64`**, which sets
    /// [`DEVICE_PROFILE_ENV`] to it unless it is already set; under the PC
    /// identity the engine was told it ran on Windows beside `isVrDevice`,
    /// and games asking `UserInputService.VREnabled` got false.
    MetaQuest,
}

impl DeviceProfile {
    pub fn parse(text: &str) -> Option<DeviceProfile> {
        match text.trim().to_ascii_lowercase().as_str() {
            "" | "pc" | "pc-windows-11" | "windows" | "windows-11" => {
                Some(DeviceProfile::PcWindows11)
            }
            "roblox-app" | "app" | "roblox" => Some(DeviceProfile::RobloxApp),
            "android" | "android-tablet" | "tablet" => Some(DeviceProfile::AndroidTablet),
            "meta-quest" | "quest" | "metaos" => Some(DeviceProfile::MetaQuest),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            DeviceProfile::RobloxApp => "roblox-app",
            DeviceProfile::AndroidTablet => "android-tablet",
            DeviceProfile::PcWindows11 => "pc-windows-11",
            DeviceProfile::MetaQuest => "meta-quest",
        }
    }
}

impl Default for DeviceProfile {
    /// **`PcWindows11` since 2026-08-20**, `AndroidTablet` before that.
    ///
    /// `RobloxApp` was added on 2026-08-22 and deliberately did **not** take
    /// the default with it: a user had already reported a tablet identity
    /// breaking PC features, and nothing has measured what the bare token does
    /// to a frame rate.
    ///
    /// This must stay in step with `device_identity()` in
    /// `native/init_params.cpp`, which is the one that actually reaches the
    /// engine -- see [`DEVICE_PROFILE_KEY`] for why the flag layer does not.
    /// Two defaults that disagree is the same drift `vanta_build_user_agent`
    /// exists to prevent, and it would be invisible: this side would report one
    /// identity in the log while the engine sent the other.
    fn default() -> Self {
        DeviceProfile::PcWindows11
    }
}

/// What the flag layers say about [`DEVICE_PROFILE_KEY`], if anything.
///
/// Split from [`device_profile`] so a test can supply a resolved map directly
/// rather than writing through the filesystem, matching how this file's other
/// tests exercise `resolve` on synthetic layers.
fn device_profile_from_resolved(resolved: &BTreeMap<String, Resolved>) -> Option<DeviceProfile> {
    resolved.get(DEVICE_PROFILE_KEY).and_then(|r| DeviceProfile::parse(&r.value))
}

/// Resolve which device identity is in force.
///
/// The environment variable wins, because it is the one thing that reliably
/// reaches `native/init_params.cpp` today (see [`DEVICE_PROFILE_KEY`]'s
/// doc for why the flag-layer path does not, yet) — checking it first rather
/// than only in C++ means a caller in this crate gets the same answer the
/// engine will. An unparseable value is reported and treated as the default
/// rather than silently ignored, matching `graphics::resolve`'s reasoning: a
/// switch that looks set but does nothing is the failure this exists to
/// avoid.
pub fn device_profile() -> DeviceProfile {
    if let Ok(text) = std::env::var(DEVICE_PROFILE_ENV) {
        let trimmed = text.trim();
        if !trimmed.is_empty() {
            match DeviceProfile::parse(trimmed) {
                Some(p) => return p,
                None => {
                    // The wording and the fallback both match
                    // `native/init_params.cpp`'s `device_identity` exactly.
                    // They used to disagree -- this side fell back to
                    // android-tablet while the C++ fell back to the PC
                    // identity -- so a misspelt value produced a client whose
                    // own log named one identity while the engine was sent
                    // another. Both now name pc-windows-11 and both mean it.
                    println!(
                        "  flags: {DEVICE_PROFILE_ENV}={text:?} is not a device profile; \
                         using pc-windows-11. Known: roblox-app, android-tablet, \
                         pc-windows-11, meta-quest"
                    );
                    return DeviceProfile::PcWindows11;
                }
            }
        }
    }
    device_profile_from_resolved(&resolve(collect())).unwrap_or_default()
}

/// Report what was resolved, naming every conflict.
///
/// Printed rather than silent because a flag doing nothing — because something
/// else set it — is otherwise indistinguishable from a flag that had no effect.
pub fn report(resolved: &BTreeMap<String, Resolved>) {
    if resolved.is_empty() {
        return;
    }
    let conflicts: Vec<_> = resolved.iter().filter(|(_, r)| !r.overridden.is_empty()).collect();
    println!("  flags: {} override(s) applied", resolved.len());
    for (key, r) in conflicts {
        let losers = r
            .overridden
            .iter()
            .map(|(s, v)| format!("{}={v}", s.describe()))
            .collect::<Vec<_>>()
            .join(", ");
        println!(
            "  flags: {key} = {} from {} (overrides {losers})",
            r.value,
            r.source.describe()
        );
    }
    if resolved.get(FRM_LOCKSTEP_FLAG).is_some_and(|r| r.source == Source::Builtin) {
        println!(
            "  flags: added {FRM_LOCKSTEP_FLAG}=False to preserve render distance with {FRM_QUALITY_OVERRIDE_FLAG}"
        );
    }
}

#[cfg(test)]
pub(crate) mod tests {

    /// **Vanta tells games it speaks BloxstrapRPC, and the user can retract
    /// it.** Both halves matter. Without the flag a game that gates its
    /// presence output on it prints nothing and the whole BloxstrapRPC reader
    /// idles; and because the claim is a fingerprinting signal -- a game can
    /// tell it is under a third-party launcher -- somebody who does not want
    /// to make it must be able to say so, which works only if this sits in the
    /// lowest-precedence layer.
    #[test]
    fn the_bloxstrap_protocol_flag_is_a_default_the_user_can_overrule() {
        let builtin = super::builtin_layer();
        assert_eq!(
            builtin.values.get("FFlagUserLaunchedWithBloxstrap").map(String::as_str),
            Some("True"),
            "games gate their presence output on this"
        );

        let mut mine = std::collections::BTreeMap::new();
        mine.insert("FFlagUserLaunchedWithBloxstrap".to_string(), "False".to_string());
        let resolved = super::resolve(vec![
            builtin,
            super::Layer { source: super::Source::User, values: mine },
        ]);
        assert_eq!(
            resolved["FFlagUserLaunchedWithBloxstrap"].value, "False",
            "the user layer must win, or the default is not opt-out"
        );
    }
    use super::*;

    /// **What the editor writes, the loader reads back identically.**
    ///
    /// The one test that has to live here rather than beside the parser in
    /// `vanta_plugins::flag_document`: it spans both crates. The window
    /// validates and writes through that module; the client reads through
    /// `read_layer` in this one. Nothing else asserts that those two agree,
    /// and them disagreeing is the failure the shared module exists to stop --
    /// a page that says "Saved 40 flags" about a file the client then reports
    /// as malformed and ignores.
    #[test]
    fn a_written_document_round_trips_through_the_loader() {
        let dir = std::env::temp_dir().join(format!("vanta-flags-rt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let values = parse_user_text(r#"{"FFlagA": "True", "DFIntB": 12}"#).unwrap();
        write_user_layer(&dir, &values).expect("write");

        let layer = read_layer(&user_path_in(&dir), Source::User).expect("read back");
        assert_eq!(layer.values, values);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `read_layer` must not prefer one value shape over another: an `FLog`
    /// channel's own type, bare number or severity name, is not something
    /// this function can see (see its doc comment). A bare digit and a
    /// severity name written as a JSON string both have to survive
    /// unchanged, or a future "helpful" normalisation could reintroduce
    /// exactly the confusion `docs/analysis/flag-init.md` §22 records.
    #[test]
    fn a_flog_channels_value_survives_whichever_shape_it_was_written_in() {
        let dir = std::env::temp_dir().join("vanta-flags-test-flog-shape");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("flags.json");
        std::fs::write(
            &path,
            r#"{"FLogNativeDM":"Verbose","FLogAppShellReporter":9,"FLogNetwork":"7"}"#,
        )
        .unwrap();

        let layer = read_layer(&path, Source::User).expect("valid JSON should read");
        assert_eq!(layer.values["FLogNativeDM"], "Verbose");
        assert_eq!(layer.values["FLogAppShellReporter"], "9");
        assert_eq!(layer.values["FLogNetwork"], "7");
    }

    fn layer(source: Source, pairs: &[(&str, &str)]) -> Layer {
        Layer {
            source,
            values: pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
        }
    }

    #[test]
    fn the_user_beats_a_plugin() {
        let r = resolve(vec![
            layer(Source::Plugin("fps".into()), &[("DFFlagX", "false")]),
            layer(Source::User, &[("DFFlagX", "true")]),
        ]);
        assert_eq!(r["DFFlagX"].value, "true");
        assert_eq!(r["DFFlagX"].source, Source::User);
    }

    #[test]
    fn a_losing_value_is_kept_so_the_conflict_can_be_explained() {
        let r = resolve(vec![
            layer(Source::Plugin("fps".into()), &[("DFFlagX", "false")]),
            layer(Source::User, &[("DFFlagX", "true")]),
        ]);
        assert_eq!(
            r["DFFlagX"].overridden,
            vec![(Source::Plugin("fps".into()), "false".to_string())]
        );
    }

    #[test]
    fn two_plugins_disagreeing_is_recorded_not_hidden() {
        let r = resolve(vec![
            layer(Source::Plugin("a".into()), &[("FIntQ", "1")]),
            layer(Source::Plugin("b".into()), &[("FIntQ", "2")]),
        ]);
        // Deterministic — later layer wins — but the disagreement survives.
        assert_eq!(r["FIntQ"].value, "2");
        assert_eq!(r["FIntQ"].overridden.len(), 1);
        assert_eq!(r["FIntQ"].overridden[0].0, Source::Plugin("a".into()));
    }

    #[test]
    fn untouched_flags_carry_no_conflict() {
        let r = resolve(vec![layer(Source::User, &[("FFlagY", "true")])]);
        assert!(r["FFlagY"].overridden.is_empty());
        assert_eq!(r["FFlagY"].source, Source::User);
    }

    #[test]
    fn the_device_profile_key_carries_the_vanta_prefix_client_settings_filters_on() {
        // client_settings.rs's `is_roblox_flag` keeps this out of Roblox's
        // settings document by checking exactly this prefix. If this constant
        // ever drifted from "Vanta", the key would silently start reaching
        // the engine's own FastFlag document instead of staying Vanta's.
        assert!(DEVICE_PROFILE_KEY.starts_with("Vanta"));
    }

    #[test]
    fn device_profile_parses_every_spelling_and_nothing_else() {
        // The empty string means "the default", and the default is the PC
        // identity -- this used to assert the tablet, which was the drift
        // against `native/init_params.cpp` that the same change fixed.
        assert_eq!(DeviceProfile::parse(""), Some(DeviceProfile::PcWindows11));
        assert_eq!(DeviceProfile::parse("android-tablet"), Some(DeviceProfile::AndroidTablet));
        assert_eq!(DeviceProfile::parse("PC-Windows-11"), Some(DeviceProfile::PcWindows11));
        assert_eq!(DeviceProfile::parse("windows"), Some(DeviceProfile::PcWindows11));
        assert_eq!(DeviceProfile::parse("roblox-app"), Some(DeviceProfile::RobloxApp));
        assert_eq!(DeviceProfile::parse(" Roblox "), Some(DeviceProfile::RobloxApp));
        assert_eq!(DeviceProfile::parse("meta-quest"), Some(DeviceProfile::MetaQuest));
        assert_eq!(DeviceProfile::MetaQuest.label(), "meta-quest");
        assert_eq!(DeviceProfile::parse("ps5"), None);
    }

    #[test]
    fn the_default_profile_is_the_one_the_engine_gets_with_nothing_set() {
        // `native/init_params.cpp`'s `device_identity` returns PcWindows11 for
        // an absent `VANTA_DEVICE_PROFILE`, and this side must agree or the
        // client's own log names one identity while the engine is sent
        // another -- which is exactly what happened for unrecognised values
        // until 2026-08-22.
        assert_eq!(DeviceProfile::default(), DeviceProfile::PcWindows11);
    }

    #[test]
    fn an_absent_device_profile_flag_resolves_to_nothing_at_all() {
        // Nothing rather than a default, so the caller can tell "no layer
        // asked for an identity" from "a layer asked for the default one" --
        // `device_profile` is where the default is applied.
        let resolved = resolve(vec![layer(Source::User, &[("FFlagUnrelated", "true")])]);
        assert_eq!(device_profile_from_resolved(&resolved), None);
    }

    #[test]
    fn the_pc_identity_is_reachable_through_the_flag_layer_by_its_vanta_key() {
        let resolved =
            resolve(vec![layer(Source::User, &[(DEVICE_PROFILE_KEY, "pc-windows-11")])]);
        assert_eq!(device_profile_from_resolved(&resolved), Some(DeviceProfile::PcWindows11));
    }

    #[test]
    fn the_users_flag_file_beats_a_plugin_asking_for_the_other_identity() {
        // Same precedence rule as everything else in this module: the user's
        // own choice is the one thing a plugin must not override.
        let resolved = resolve(vec![
            layer(Source::Plugin("fps".into()), &[(DEVICE_PROFILE_KEY, "pc-windows-11")]),
            layer(Source::User, &[(DEVICE_PROFILE_KEY, "android-tablet")]),
        ]);
        assert_eq!(device_profile_from_resolved(&resolved), Some(DeviceProfile::AndroidTablet));
    }

    #[test]
    fn the_environment_variable_is_the_one_that_actually_reaches_the_engine_today() {
        // Mutex'd for the same reason `scratch` is: env vars are process-wide
        // and cargo runs tests in parallel threads of the one process. Only
        // the set-variable cases are exercised here — with it unset,
        // `device_profile` falls through to `collect()`, which reads real
        // profile directories; that fallthrough is already covered without
        // touching the filesystem by `device_profile_from_resolved` above, so
        // this test never needs to depend on what happens to be on disk.
        let _g = ENV.lock().unwrap_or_else(|e| e.into_inner());

        std::env::set_var(DEVICE_PROFILE_ENV, "pc-windows-11");
        assert_eq!(device_profile(), DeviceProfile::PcWindows11);

        std::env::set_var(DEVICE_PROFILE_ENV, "roblox-app");
        assert_eq!(device_profile(), DeviceProfile::RobloxApp);

        // An unparseable value falls back to the **default**, which is what
        // the C++ does. It used to fall back to the tablet here while
        // `device_identity` fell back to the PC identity, so `amiga-500`
        // produced a client whose log and whose engine disagreed.
        std::env::set_var(DEVICE_PROFILE_ENV, "amiga-500");
        assert_eq!(device_profile(), DeviceProfile::PcWindows11);

        std::env::remove_var(DEVICE_PROFILE_ENV);
    }

    /// `XDG_CONFIG_HOME`, `VANTA_FLAGS` and `VANTA_PLUGIN_DIR` are
    /// process-wide, and cargo runs these in parallel threads of one process.
    /// Same reasoning as `profile`'s own test mutex, which records that the
    /// unserialised version passed anyway on its first run.
    ///
    /// `pub(crate)` so `plugin_host.rs`'s tests can take the same lock before
    /// touching `VANTA_PLUGIN_DIR` themselves — two module-local mutexes
    /// guarding one process-wide variable would not actually exclude each
    /// other, which is the flake this is written to avoid rather than repeat.
    pub(crate) static ENV: std::sync::Mutex<()> = std::sync::Mutex::new(());

    // `pub(crate)` so `frame_rate_limit_tests` (this file, below) can share the
    // same scratch-and-lock helper rather than a second copy of it -- the same
    // reasoning `ENV`'s own `pub(crate)` already gives for `plugin_host.rs`.
    pub(crate) fn scratch(tag: &str) -> (PathBuf, std::sync::MutexGuard<'static, ()>) {
        let guard = ENV.lock().unwrap_or_else(|e| e.into_inner());
        let root = std::env::temp_dir().join(format!("vanta-flags-test-{tag}"));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::env::remove_var("VANTA_FLAGS");
        std::env::set_var("XDG_CONFIG_HOME", root.join("config"));
        (root, guard)
    }

    #[test]
    fn one_profiles_overrides_are_not_another_profiles() {
        // A flag set while debugging on a test account used to be set on the
        // account someone plays, because there was one file for the machine.
        let (root, _g) = scratch("isolation");
        let alt = root.join("profiles/alt");
        let main = root.join("profiles/main");
        std::fs::create_dir_all(&alt).unwrap();
        std::fs::create_dir_all(&main).unwrap();
        std::fs::write(user_path_in(&alt), r#"{"DFFlagDebugSomething":"true"}"#).unwrap();

        assert!(read_layer(&user_path_in(&alt), Source::User).is_some());
        assert!(
            read_layer(&user_path_in(&main), Source::User).is_none(),
            "an override made in another profile must not apply here"
        );
    }

    #[test]
    fn a_pre_existing_global_overrides_file_is_moved_into_the_profile() {
        // Leaving it to be ignored would look exactly like overrides never
        // having worked, which is a bug report about the wrong thing.
        let (root, _g) = scratch("migrate");
        let profile = root.join("profiles/default");
        std::fs::create_dir_all(&profile).unwrap();
        let legacy = legacy_user_path();
        std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        std::fs::write(&legacy, r#"{"FIntTaskSchedulerAutoThreadLimit":"8"}"#).unwrap();

        let moved = migrate_legacy_user_file(&profile).expect("the legacy file should have moved");
        assert_eq!(moved, user_path_in(&profile));
        assert!(!legacy.exists(), "the old file should be gone, not copied");
        let layer = read_layer(&moved, Source::User).expect("the moved file should read");
        assert_eq!(layer.values["FIntTaskSchedulerAutoThreadLimit"], "8");
    }

    #[test]
    fn migration_never_overwrites_overrides_this_profile_already_has() {
        let (root, _g) = scratch("migrate-existing");
        let profile = root.join("profiles/alt");
        std::fs::create_dir_all(&profile).unwrap();
        std::fs::write(user_path_in(&profile), r#"{"FIntQ":"1"}"#).unwrap();

        let legacy = legacy_user_path();
        std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        std::fs::write(&legacy, r#"{"FIntQ":"2"}"#).unwrap();

        assert!(migrate_legacy_user_file(&profile).is_none());
        let layer = read_layer(&user_path_in(&profile), Source::User).unwrap();
        assert_eq!(layer.values["FIntQ"], "1", "the profile's own file must win");
        assert!(legacy.exists(), "and the old file is left untouched");
    }

    fn scratch_plugin_dir(tag: &str) -> (PathBuf, std::sync::MutexGuard<'static, ()>) {
        let guard = ENV.lock().unwrap_or_else(|e| e.into_inner());
        let root = std::env::temp_dir().join(format!("vanta-flags-plugin-write-test-{tag}"));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::env::set_var("VANTA_PLUGIN_DIR", &root);
        (root, guard)
    }

    #[test]
    fn a_plugin_can_write_its_own_flags_layer_and_read_it_back() {
        let (root, _g) = scratch_plugin_dir("roundtrip");
        let values: BTreeMap<String, String> =
            [("FFlagFoo".to_string(), "true".to_string())].into_iter().collect();
        write_plugin_layer("tuner", &values).unwrap();

        let layer = read_layer(&root.join("tuner/flags.json"), Source::Plugin("tuner".into()))
            .expect("the written file should read back");
        assert_eq!(layer.values["FFlagFoo"], "true");
    }

    #[test]
    fn writing_a_plugins_flags_replaces_rather_than_merges() {
        // The plugin is the only writer of its own file, so it must be able
        // to drop a flag it has stopped wanting overridden — a merge would
        // leave it there with nothing able to remove it.
        let (root, _g) = scratch_plugin_dir("replace");
        let first: BTreeMap<String, String> =
            [("FFlagA".to_string(), "true".to_string()), ("FFlagStale".to_string(), "true".to_string())]
                .into_iter()
                .collect();
        write_plugin_layer("tuner", &first).unwrap();
        let second: BTreeMap<String, String> = [("FFlagA".to_string(), "false".to_string())].into_iter().collect();
        write_plugin_layer("tuner", &second).unwrap();

        let layer = read_layer(&root.join("tuner/flags.json"), Source::Plugin("tuner".into())).unwrap();
        assert_eq!(layer.values["FFlagA"], "false");
        assert!(layer.values.get("FFlagStale").is_none(), "{:?}", layer.values);
    }

    #[test]
    fn a_plugin_id_that_is_not_valid_is_refused_rather_than_used_as_a_path() {
        let (_root, _g) = scratch_plugin_dir("bad-id");
        let values = BTreeMap::new();
        for bad in ["..", "../../etc", "a/b", "/etc/passwd"] {
            assert!(write_plugin_layer(bad, &values).is_err(), "{bad:?} should be refused");
        }
    }

    #[test]
    fn an_oversized_flags_layer_is_refused() {
        let (_root, _g) = scratch_plugin_dir("oversized");
        let huge: BTreeMap<String, String> =
            [("FFlagHuge".to_string(), "x".repeat(MAX_PLUGIN_FLAGS_BYTES + 1))].into_iter().collect();
        assert!(write_plugin_layer("tuner", &huge).is_err());
    }

    #[test]
    fn one_plugins_write_does_not_touch_another_plugins_flags_json() {
        let (root, _g) = scratch_plugin_dir("isolation");
        let a: BTreeMap<String, String> = [("FFlagA".to_string(), "true".to_string())].into_iter().collect();
        let b: BTreeMap<String, String> = [("FFlagB".to_string(), "true".to_string())].into_iter().collect();
        write_plugin_layer("plugin-a", &a).unwrap();
        write_plugin_layer("plugin-b", &b).unwrap();

        let layer_a = read_layer(&root.join("plugin-a/flags.json"), Source::Plugin("plugin-a".into())).unwrap();
        let layer_b = read_layer(&root.join("plugin-b/flags.json"), Source::Plugin("plugin-b".into())).unwrap();
        assert!(layer_a.values.get("FFlagB").is_none());
        assert!(layer_b.values.get("FFlagA").is_none());
    }

    /// A user plugin may not take an id a first-party plugin already provides.
    ///
    /// The failure guarded here is not a wrong flag value; it is a plugin the
    /// user did not install answering as one they trust. Vanta ships the
    /// web-view interceptor that decides a Studio link means the software
    /// centre, and a same-id directory in a writable location must not become
    /// that. Disabling a first-party plugin stays a separate, legitimate thing.
    #[test]
    fn a_user_plugin_cannot_shadow_a_first_party_id() {
        let (root, _g) = scratch("shadow");
        let sys = root.join("sys");
        let usr = root.join("usr");
        std::fs::create_dir_all(sys.join("studio-links")).unwrap();
        std::fs::create_dir_all(usr.join("studio-links")).unwrap();
        std::fs::create_dir_all(usr.join("mine")).unwrap();
        std::fs::write(sys.join("studio-links/flags.json"), r#"{"FFlagFirstParty":"true"}"#)
            .unwrap();
        std::fs::write(usr.join("studio-links/flags.json"), r#"{"FFlagImposter":"true"}"#)
            .unwrap();
        std::fs::write(usr.join("mine/flags.json"), r#"{"FFlagMine":"true"}"#).unwrap();
        // SAFETY: `scratch` holds the lock these tests share for process env.
        unsafe {
            std::env::set_var("VANTA_SYSTEM_PLUGIN_DIR", &sys);
            std::env::set_var("VANTA_PLUGIN_DIR", &usr);
        }

        let resolved = resolve(collect());

        unsafe {
            std::env::remove_var("VANTA_SYSTEM_PLUGIN_DIR");
            std::env::remove_var("VANTA_PLUGIN_DIR");
        }
        assert!(resolved.contains_key("FFlagFirstParty"), "the system plugin should load");
        assert!(
            !resolved.contains_key("FFlagImposter"),
            "a user plugin must not take a first-party id"
        );
        assert!(resolved.contains_key("FFlagMine"), "a user plugin with its own id still loads");
    }

    #[test]
    fn read_layer_normalises_booleans_and_whole_floats_and_preserves_logs_and_strings() {
        let (root, _g) = scratch("normalise");
        let path = root.join("flags.json");
        std::fs::write(
            &path,
            r#"{
                "FFlagEnabled": true,
                "DFFlagDisabled": false,
                "DFIntTaskSchedulerTargetFps": 144.0,
                "FIntNegative": -4.0,
                "FLogNativeDM": "Verbose",
                "FLogNetwork": 7,
                "DFLogChannel": 7.0,
                "FStringRawBool": "true",
                "DFStringRawFloat": "144.0"
            }"#,
        )
        .unwrap();

        let layer = read_layer(&path, Source::User).expect("valid JSON should read");
        assert_eq!(layer.values["FFlagEnabled"], "True");
        assert_eq!(layer.values["DFFlagDisabled"], "False");
        assert_eq!(layer.values["DFIntTaskSchedulerTargetFps"], "144");
        assert_eq!(layer.values["FIntNegative"], "-4");
        assert_eq!(layer.values["FLogNativeDM"], "Verbose");
        assert_eq!(layer.values["FLogNetwork"], "7");
        assert_eq!(layer.values["DFLogChannel"], "7.0");
        assert_eq!(layer.values["FStringRawBool"], "true");
        assert_eq!(layer.values["DFStringRawFloat"], "144.0");
    }

    #[test]
    fn frm_quality_override_in_range_auto_injects_lockstep_false() {
        for valid in ["1", "10", "21"] {
            let resolved = resolve(vec![layer(
                Source::User,
                &[(FRM_QUALITY_OVERRIDE_FLAG, valid)],
            )]);
            let lockstep = resolved
                .get(FRM_LOCKSTEP_FLAG)
                .unwrap_or_else(|| panic!("expected {FRM_LOCKSTEP_FLAG} for QL={valid}"));
            assert_eq!(lockstep.value, "False");
            assert_eq!(lockstep.source, Source::Builtin);
        }
    }

    #[test]
    fn frm_quality_override_out_of_range_does_not_inject_lockstep() {
        for invalid in ["0", "-1", "22", "99", "notanumber"] {
            let resolved = resolve(vec![layer(
                Source::User,
                &[(FRM_QUALITY_OVERRIDE_FLAG, invalid)],
            )]);
            assert!(
                !resolved.contains_key(FRM_LOCKSTEP_FLAG),
                "QL={invalid} must not inject {FRM_LOCKSTEP_FLAG}"
            );
        }
    }

    #[test]
    fn explicit_user_lockstep_flag_wins_over_auto_injection() {
        let resolved = resolve(vec![layer(
            Source::User,
            &[(FRM_QUALITY_OVERRIDE_FLAG, "1"), (FRM_LOCKSTEP_FLAG, "True")],
        )]);
        let lockstep = &resolved[FRM_LOCKSTEP_FLAG];
        assert_eq!(lockstep.value, "True");
        assert_eq!(lockstep.source, Source::User);
    }

}

#[cfg(test)]
mod performance_tests {
    use super::*;

    fn to_map(v: Vec<(String, String)>) -> BTreeMap<String, String> {
        v.into_iter().collect()
    }

    #[test]
    fn balanced_throughput_latency_and_speed_are_distinct_and_free_of_dead_flags() {
        let balanced = to_map(performance_flags(Performance::Balanced, 16));
        let throughput = to_map(performance_flags(Performance::Throughput, 16));
        let latency = to_map(performance_flags(Performance::Latency, 16));
        let speed = to_map(performance_flags(Performance::Speed, 16));

        assert_eq!(Performance::default(), Performance::Balanced);
        assert!(!balanced.is_empty());
        assert_ne!(balanced, throughput, "Throughput must scale beyond Balanced");
        assert_ne!(balanced, latency, "Latency must differ from Balanced");
        assert_ne!(throughput, latency);
        assert_ne!(latency, speed);

        for (name, map) in [
            ("balanced", &balanced),
            ("throughput", &throughput),
            ("latency", &latency),
            ("speed", &speed),
        ] {
            assert!(
                !map.contains_key("FFlagGcInParallelWithRenderPrepare3"),
                "{name} must not emit removed flag FFlagGcInParallelWithRenderPrepare3"
            );
        }

        // Throughput scales higher than Balanced on a 16-core machine.
        assert_eq!(balanced["FIntTaskSchedulerAutoThreadLimit"], "8");
        assert_eq!(throughput["FIntTaskSchedulerAutoThreadLimit"], "16");
        assert_eq!(balanced["DFIntSimMidPhaseContactPipelineBatchSize"], "64");
        assert_eq!(throughput["DFIntSimMidPhaseContactPipelineBatchSize"], "128");

        // Latency uses genuinely lower thread counts and a 32-item physics batch.
        assert_eq!(latency["FIntTaskSchedulerAutoThreadLimit"], "4");
        assert_eq!(latency["FIntSmoothClusterTaskQueueMaxParallelTasks"], "1");
        assert_eq!(latency["FIntOcclusionWorkerThreadCount"], "1");
        assert_eq!(latency["DFIntSimMidPhaseContactPipelineBatchSize"], "32");
    }

    #[test]
    fn speed_mode_parses_case_insensitively_and_emits_verified_bundle() {
        assert_eq!(Performance::parse("speed"), Some(Performance::Speed));
        assert_eq!(Performance::parse("SPEED"), Some(Performance::Speed));
        assert_eq!(Performance::parse(" Speed "), Some(Performance::Speed));
        assert_eq!(Performance::Speed.label(), "speed");

        let speed_2c = to_map(performance_flags(Performance::Speed, 2));
        let speed_16c = to_map(performance_flags(Performance::Speed, 16));
        assert_eq!(speed_2c["FIntTaskSchedulerAutoThreadLimit"], "2");
        assert_eq!(speed_16c["FIntTaskSchedulerAutoThreadLimit"], "4");

        let expected: &[(&str, &str)] = &[
            ("FIntTaskSchedulerThreadMin", "0"),
            ("FIntTaskSchedulerAutoThreadLimit", "4"),
            ("FIntTaskSchedulerAsyncTasksMinimumThreadCount", "1"),
            ("FIntSmoothClusterTaskQueueMaxParallelTasks", "1"),
            ("FIntSmoothClusterTaskQueueMsPerCycle", "1"),
            ("FIntOcclusionWorkerThreadCount", "1"),
            ("DFIntDebugVoxelJobManagerMaxThreads", "1"),
            ("DFIntVoxelJobManagerDMJobTimeMicroSeconds", "1000"),
            ("DFIntVoxelJobManagerRenderThreadJobTimeMicroSeconds", "1000"),
            ("DFIntSimMidPhaseContactPipelineBatchSize", "32"),
            ("FFlagMovePrerenderV2", "True"),
            ("FFlagAnimatorParallelManagerUseLiveTaskSchedulerConcurrency", "True"),
            ("FFlagHumanoidParallelManagerUseLiveTaskSchedulerConcurrency", "True"),
            ("DFIntDebugFRMQualityLevelOverride", "1"),
            ("FFlagFRMLockstepOverridesEffective", "False"),
            ("FIntDebugFRMOptionalMSAALevelOverride", "0"),
            ("FIntBloomFrmCutoff", "22"),
            ("FFlagFRMDisableCloudsAtLowQL", "True"),
            ("DFFlagRenderConstantFogAtLowQL", "True"),
            ("DFIntRenderConstantFogAtLowQLQLCutoff", "21"),
            ("DFIntRenderPostFxBasePixelCount", "480000"),
            ("FFlagRenderShadowResolutionScaling", "True"),
            ("FStringRenderShadowResolutionQualityByRam", "1024,3,2048,3,4096,3"),
            ("FFlagRenderAllocateShadowMapResourcesOnDemand", "True"),
            ("FIntTM2ShadowMapMaxMips", "6"),
            ("FIntTM2EnvMapMaxMips", "6"),
            ("FIntEnvMapRaycastCount", "4"),
            ("FIntEnvMapMaxRayLength", "64"),
            ("FFlagRenderWaterDemoteSHQ", "True"),
            ("DFFlagDebugForceAnisoOff", "True"),
            ("DFIntRenderClampRoughnessMax", "1"),
            ("FIntDebugTextureManagerSkipMips", "1"),
            ("DFIntDynamicTextureManagerPrepareBudgetMicroSeconds", "500"),
            ("FIntFRMMinGrassDistance", "0"),
            ("FIntFRMMaxGrassDistance", "0"),
            ("FIntRenderGrassDensityMax", "0"),
            ("FIntRenderGrassDensityMin", "0"),
            ("DFFlagFRMUseParticleCountBudget", "True"),
            ("DFFlagFRMUseGlobalParticleCountBudget", "True"),
            ("DFIntFRMParticleBudgetMinLerp", "64"),
            ("DFIntFRMGlobalParticleBudgetMinLerp", "256"),
            ("FFlagRenderParticlesCapNonVisibleEmission", "True"),
            ("FFlagRenderParticlesNonVisibleSimBudget", "True"),
            ("DFIntRenderParticlesNonVisibleSimBudgetMicros", "100"),
            ("DFIntParticlesFramesBetweenNonVisibleUpdates", "12"),
            ("DFIntTerrainLodActive", "4"),
            ("DFIntTerrainLodLevels", "2"),
            ("DFIntTerrainPow2LodBiasPercent", "50"),
            ("FFlagAudioEngineThrottleSoundServiceStep", "True"),
            ("FIntAudioEngineAudibilityDepthLimit", "6"),
            ("FFlagGraphicsVulkanExtendedDynamicState", "True"),
            ("FFlagVulkanAllocatorPoolSmallOnly", "False"),
            ("FIntVulkanAllocatorKeepEmptyPages", "4"),
        ];
        assert_eq!(speed_16c.len(), expected.len());
        for (k, v) in expected {
            assert_eq!(speed_16c.get(*k).map(String::as_str), Some(*v), "Speed flag {k}");
        }

        // Speed flags live in Source::Performance, so user flags.json overrides still win.
        let perf_layer = Layer { source: Source::Performance, values: speed_16c };
        let user_layer = Layer {
            source: Source::User,
            values: [
                ("DFIntDebugFRMQualityLevelOverride".to_string(), "10".to_string()),
                ("FFlagFRMLockstepOverridesEffective".to_string(), "True".to_string()),
            ]
            .into_iter()
            .collect(),
        };
        let resolved = resolve(vec![perf_layer, user_layer]);
        assert_eq!(resolved["DFIntDebugFRMQualityLevelOverride"].value, "10");
        assert_eq!(resolved["DFIntDebugFRMQualityLevelOverride"].source, Source::User);
        assert_eq!(resolved["FFlagFRMLockstepOverridesEffective"].value, "True");
        assert_eq!(resolved["FFlagFRMLockstepOverridesEffective"].source, Source::User);
    }

    #[test]
    fn throughput_scales_with_cores_and_caps_the_async_floor() {
        let small = performance_flags(Performance::Throughput, 2);
        let big = performance_flags(Performance::Throughput, 16);
        let get = |v: &[(String, String)], k: &str| {
            v.iter().find(|(a, _)| a == k).map(|(_, b)| b.clone()).unwrap()
        };
        assert_eq!(get(&small, "FIntTaskSchedulerAutoThreadLimit"), "2");
        assert_eq!(get(&big, "FIntTaskSchedulerAutoThreadLimit"), "16");
        // Capped at three however many cores there are.
        assert_eq!(get(&small, "FIntTaskSchedulerAsyncTasksMinimumThreadCount"), "2");
        assert_eq!(get(&big, "FIntTaskSchedulerAsyncTasksMinimumThreadCount"), "3");
    }

    #[test]
    fn a_machine_reporting_no_cores_still_gets_a_usable_number() {
        // A pool sized zero would ask the engine for no threads at all, which
        // is a worse failure than a small pool. Every *sized* pool therefore
        // floors at one.
        //
        // `FIntTaskSchedulerThreadMin` is exempt and its zero is deliberate:
        // it is a floor rather than a size, and zero means "no floor, let the
        // scheduler decide", which is what mocktail's throughput policy sets.
        // This test asserted otherwise on its first run and was wrong -- the
        // distinction between a pool size and a pool floor is exactly the kind
        // of thing a copied table loses.
        let v = performance_flags(Performance::Throughput, 0);
        for (k, val) in &v {
            if k.starts_with("FInt") && k != "FIntTaskSchedulerThreadMin" {
                assert_ne!(val, "0", "{k} sizes a pool and must not be zero");
            }
        }
        let min = v.iter().find(|(k, _)| k == "FIntTaskSchedulerThreadMin").unwrap();
        assert_eq!(min.1, "0", "the floor stays zero on purpose");
    }

    #[test]
    fn physical_cores_is_at_least_one_and_no_more_than_the_thread_count() {
        let p = physical_cores();
        let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
        assert!(p >= 1, "{p}");
        assert!(p <= threads, "physical {p} cannot exceed threads {threads}");
    }
}

#[cfg(test)]
mod frame_rate_limit_tests {
    use super::*;

    #[test]
    fn display_is_the_default_and_sets_nothing() {
        assert_eq!(FrameRateLimit::default(), FrameRateLimit::Display);
        assert!(frame_rate_limit_flags(FrameRateLimit::Display).is_empty());
    }

    #[test]
    fn display_follows_a_fast_display_and_leaves_a_sixty_hertz_one_alone() {
        let target = |hz| frame_rate_limit_flags_for(FrameRateLimit::Display, hz, false);
        assert_eq!(
            target(Some(120.0)),
            vec![("DFIntTaskSchedulerTargetFps".to_string(), "120".to_string())],
            "a 120 Hz output must not stay at the engine's 60"
        );
        assert_eq!(
            frame_rate_limit_flags_for(FrameRateLimit::Display, Some(100.0), true),
            vec![("DFIntTaskSchedulerTargetFps".to_string(), "400".to_string())],
            "MAILBOX on 100 Hz must target well above the refresh so the limiter does not throttle capable hardware"
        );
        // Controls: nothing to raise, or nothing known.
        assert!(target(Some(60.0)).is_empty());
        assert!(target(None).is_empty());
        // A number chosen by hand is not overridden by the display.
        assert_eq!(
            frame_rate_limit_flags_for(FrameRateLimit::Cap(90), Some(144.0), true),
            vec![("DFIntTaskSchedulerTargetFps".to_string(), "90".to_string())]
        );
    }

    #[test]
    fn a_cap_sets_exactly_the_target_fps_flag() {
        let v = frame_rate_limit_flags(FrameRateLimit::Cap(120));
        assert_eq!(v, vec![("DFIntTaskSchedulerTargetFps".to_string(), "120".to_string())]);
    }

    #[test]
    fn parse_recognises_every_spelling_the_setting_and_env_var_use() {
        assert_eq!(FrameRateLimit::parse(""), Some(FrameRateLimit::Display));
        assert_eq!(FrameRateLimit::parse("display"), Some(FrameRateLimit::Display));
        assert_eq!(FrameRateLimit::parse("Auto"), Some(FrameRateLimit::Display));
        assert_eq!(FrameRateLimit::parse("120"), Some(FrameRateLimit::Cap(120)));
        assert_eq!(FrameRateLimit::parse(" 90 "), Some(FrameRateLimit::Cap(90)));
        assert_eq!(FrameRateLimit::parse("240"), Some(FrameRateLimit::Cap(240)));
    }

    /// The words the shell's `FrameRateLimit::as_env` produces, written out
    /// here because `vanta-shell` cannot be depended on from this crate. Its
    /// `every_choice_names_itself_to_the_client` pins the same list on its side.
    #[test]
    fn the_shells_words_are_the_runtimes_words() {
        let shell = [
            ("display", FrameRateLimit::Display),
            ("90", FrameRateLimit::Cap(90)),
            ("120", FrameRateLimit::Cap(120)),
            ("144", FrameRateLimit::Cap(144)),
            ("165", FrameRateLimit::Cap(165)),
            ("240", FrameRateLimit::Cap(240)),
        ];
        for (word, want) in shell {
            assert_eq!(FrameRateLimit::parse(word), Some(want), "{word}");
        }
    }

    #[test]
    fn uncapped_spellings_and_invalid_values() {
        assert_eq!(FrameRateLimit::parse("uncapped"), Some(FrameRateLimit::Cap(999)));
        assert_eq!(FrameRateLimit::parse("unlimited"), Some(FrameRateLimit::Cap(999)));
        for word in ["off", "9999.5", "inf"] {
            assert_eq!(FrameRateLimit::parse(word), None, "{word}");
        }
    }

    #[test]
    fn a_zero_or_negative_cap_is_not_a_real_choice() {
        // Zero is not established to mean "uncapped" for this flag, and a
        // negative number is not a frame rate at all, so both must be refused
        // rather than silently becoming DFIntTaskSchedulerTargetFps=0, which
        // nothing here has measured the effect of.
        assert_eq!(FrameRateLimit::parse("0"), None);
        assert_eq!(FrameRateLimit::parse("-30"), None);
        assert_eq!(FrameRateLimit::parse("not-a-number"), None);
    }

    #[test]
    fn a_live_choice_beats_the_environment_and_reports_whether_it_changed_anything() {
        let (_root, _g) = super::tests::scratch("frame-rate-limit-live");
        std::env::set_var(FRAME_RATE_LIMIT_ENV, "144");
        assert!(set_live_frame_rate_limit(None) == false, "clearing nothing changes nothing");
        assert_eq!(frame_rate_limit(), FrameRateLimit::Cap(144));

        assert!(set_live_frame_rate_limit(Some(FrameRateLimit::Cap(240))), "144 to 240 is a change");
        assert_eq!(frame_rate_limit(), FrameRateLimit::Cap(240));
        assert_eq!(
            frame_rate_limit_flags(frame_rate_limit()),
            vec![("DFIntTaskSchedulerTargetFps".to_string(), "240".to_string())]
        );

        assert!(!set_live_frame_rate_limit(Some(FrameRateLimit::Cap(240))), "the same choice again is not");

        assert!(set_live_frame_rate_limit(Some(FrameRateLimit::Display)), "back to the display is a change");
        assert_eq!(frame_rate_limit(), FrameRateLimit::Display, "and it beats the environment's 144");
        assert!(frame_rate_limit_flags(frame_rate_limit()).is_empty());

        set_live_frame_rate_limit(None);
        std::env::remove_var(FRAME_RATE_LIMIT_ENV);
    }

    #[test]
    fn the_environment_variable_wins_over_the_flag_layer_which_wins_over_the_default() {
        let (root, _g) = super::tests::scratch("frame-rate-limit-precedence");
        std::env::set_var("VANTA_PROFILE_ROOT", &root);
        std::env::remove_var(FRAME_RATE_LIMIT_ENV);
        assert_eq!(frame_rate_limit(), FrameRateLimit::Display, "nothing set anywhere");

        let profile = crate::profile::active();
        std::fs::create_dir_all(&profile).unwrap();
        write_user_layer(
            &profile,
            &[(FRAME_RATE_LIMIT_KEY.to_string(), "144".to_string())].into_iter().collect(),
        )
        .unwrap();
        assert_eq!(
            frame_rate_limit(),
            FrameRateLimit::Cap(144),
            "the flag layer should be read once the env var is absent"
        );

        std::env::set_var(FRAME_RATE_LIMIT_ENV, "165");
        assert_eq!(
            frame_rate_limit(),
            FrameRateLimit::Cap(165),
            "the environment must win over a flags.json entry, the same order \
             performance() and resolve_present_mode() already use"
        );

        std::env::remove_var(FRAME_RATE_LIMIT_ENV);
        std::env::remove_var("VANTA_PROFILE_ROOT");
    }

    #[test]
    fn an_unparseable_env_value_falls_back_to_display_rather_than_the_flag_layer() {
        let _g = super::tests::ENV.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var(FRAME_RATE_LIMIT_ENV, "quite fast please");
        assert_eq!(frame_rate_limit(), FrameRateLimit::Display);
        std::env::remove_var(FRAME_RATE_LIMIT_ENV);
    }

    #[test]
    fn the_settings_row_beats_a_plugin_and_display_leaves_the_plugin_alone() {
        let (root, _g) = super::tests::scratch("frame-rate-limit-vs-plugin");
        std::env::set_var("VANTA_PROFILE_ROOT", &root);
        let plugins = root.join("plugins");
        std::fs::create_dir_all(plugins.join("cap-plugin")).unwrap();
        std::fs::write(
            plugins.join("cap-plugin/flags.json"),
            r#"{"DFIntTaskSchedulerTargetFps": 240}"#,
        )
        .unwrap();
        std::env::set_var("VANTA_PLUGIN_DIR", &plugins);

        std::env::set_var(FRAME_RATE_LIMIT_ENV, "display");
        let resolved = resolve(collect());
        assert_eq!(resolved["DFIntTaskSchedulerTargetFps"].value, "240");
        assert_eq!(resolved["DFIntTaskSchedulerTargetFps"].source, Source::Plugin("cap-plugin".into()));

        std::env::set_var(FRAME_RATE_LIMIT_ENV, "120");
        let resolved = resolve(collect());
        assert_eq!(resolved["DFIntTaskSchedulerTargetFps"].value, "120", "the row is the user's choice");
        assert_eq!(resolved["DFIntTaskSchedulerTargetFps"].source, Source::FrameRateLimit);
        assert_eq!(
            resolved["DFIntTaskSchedulerTargetFps"].overridden,
            vec![(Source::Plugin("cap-plugin".into()), "240".to_string())],
            "and the report can say what it displaced"
        );

        std::env::remove_var(FRAME_RATE_LIMIT_ENV);
        std::env::remove_var("VANTA_PLUGIN_DIR");
        std::env::remove_var("VANTA_PROFILE_ROOT");
    }

    #[test]
    fn collect_includes_the_frame_rate_limit_layer_and_the_user_still_wins() {
        let (root, _g) = super::tests::scratch("frame-rate-limit-collect");
        std::env::set_var("VANTA_PROFILE_ROOT", &root);
        std::env::set_var(FRAME_RATE_LIMIT_ENV, "90");

        let resolved = resolve(collect());
        assert_eq!(resolved["DFIntTaskSchedulerTargetFps"].value, "90");
        assert_eq!(resolved["DFIntTaskSchedulerTargetFps"].source, Source::FrameRateLimit);

        // A user override of the same key still wins -- the same precedence
        // `the_user_beats_a_plugin` asserts for a plugin, one layer over.
        let profile = crate::profile::active();
        std::fs::create_dir_all(&profile).unwrap();
        write_user_layer(
            &profile,
            &[("DFIntTaskSchedulerTargetFps".to_string(), "30".to_string())].into_iter().collect(),
        )
        .unwrap();
        let resolved = resolve(collect());
        assert_eq!(resolved["DFIntTaskSchedulerTargetFps"].value, "30");
        assert_eq!(resolved["DFIntTaskSchedulerTargetFps"].source, Source::User);

        std::env::remove_var(FRAME_RATE_LIMIT_ENV);
        std::env::remove_var("VANTA_PROFILE_ROOT");
    }
}
