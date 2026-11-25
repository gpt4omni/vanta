//! What the engine's own log says about joining and leaving a game.
//!
//! **`bloxstrap_rpc` has been in this crate since presence landed, with a
//! parser, tests, and no callers at all.** `grep 'bloxstrap_rpc::'` across the
//! workspace returned nothing: the protocol was implemented and nothing ever
//! read a line to hand it. This module is the missing half -- the thing that
//! watches the file -- and once it exists the same lines answer three other
//! questions Vanta could not answer before.
//!
//! Nothing is injected and nothing is hooked. The engine writes a log into a
//! directory Vanta created, and Vanta reads it. That is the argument
//! `bloxstrap_rpc`'s own header already makes about why this runs into neither
//! ADR-001 nor ADR-003, and it applies unchanged here.
//!
//! ## The markers, and where they came from
//!
//! Read off a real capture rather than guessed: Sober's own log of the same
//! engine on this host, `2.734.0.917_20260831T031139Z_Player_3e43f_last.log`,
//! 1,627 lines covering four joins and four leaves -- the parser run over the
//! whole of it finds all eight and nothing else. AGENTS.md's rule about
//! grepping a capture before reasoning from a binary is the same rule one
//! level up, and this is the capture.
//!
//! ```text
//! [FLog::GameJoinLoadTime] Report game_join_loadtime: sid:6baeb082-..., \
//!     clienttime:..., join_time:1.2011154180, referral_page:, \
//!     placeid:17625359962, userid:1826805362, universeid:6035872082,
//! [FLog::Network] UDMUX Address = 128.116.51.33, Port = 50363 | \
//!     RCC Server Address = 10.60.2.168, Port = 50363
//! [FLog::SingleSurfaceApp] leaveUGCGameInternal
//! ```
//!
//! **The log carries no game name, and this module does not invent one.** It
//! reports the place and universe ids, which is what is actually there;
//! resolving either to a title needs a request to Roblox's web API and belongs
//! to whoever wants the title, not to a log parser. Saying so matters because
//! the README currently states that Discord presence "cannot name the game yet
//! -- no core event carries which place is running", and that is still true of
//! *core events*: what changes is that the id is now available from somewhere
//! else, so the sentence is worth revisiting rather than deleting.
//!
//! **`UDMUX Address` is the server's address and not its location.** Turning
//! one into the other is a geo-IP lookup against a third party, which is a
//! network call on somebody's behalf and a privacy question, so this module
//! stops at the address.

use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Something worth knowing that happened in a game.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// A game was joined. Both ids come off the same line.
    ///
    /// `universe_id` is the one to resolve to a title -- Roblox's
    /// `games?universeIds=` takes universes, not places -- and `place_id` is
    /// the one a `roblox://` deep link uses. Neither is looked up here.
    Joined { place_id: u64, universe_id: u64, user_id: u64 },
    /// The server instance being joined, from the line before the join.
    ///
    /// `job_id` is what a `roblox://experiences/start?...&gameInstanceId=`
    /// deep link needs to put somebody in *this* server rather than a fresh
    /// one, which is how Bloxstrap's "Join server" button works -- a plain URL
    /// button, not Discord's join-secret machinery. Adapted from Bloxstrap
    /// (MIT, Bloxstrap Labs): the marker and the shape, not its implementation.
    Joining { job_id: String, place_id: u64 },
    /// The address the client is connected through, from the same join.
    ///
    /// Reported separately from [`Event::Joined`] because it arrives on its own
    /// line a few milliseconds later, and because a consumer that wants the
    /// join does not necessarily want the address.
    Server { address: String, port: u16 },
    /// The user left the game and went back to the app.
    ///
    /// `leaveUGCGameInternal`, not a disconnect. The log carries several
    /// disconnects per session -- a websocket giving up, a teleport between
    /// places, the peer going away -- and treating any of those as "left"
    /// would close the client mid-teleport. This one fires when the engine
    /// returns to its own home screen, twice in the capture and both times
    /// after the user actually left.
    Left,
    /// A `[BloxstrapRPC]` line, handed on verbatim for
    /// [`crate::bloxstrap_rpc::parse_line`] to decode.
    ///
    /// Passed through rather than parsed here so that this module stays "what
    /// the log said" and the protocol stays in the module that documents it.
    Rpc(String),
    /// The engine's settings reloader ran and applied Roblox's settings document.
    ///
    /// Not about a game at all, and here because this is the one place that
    /// already tails the engine's log. It is what `flag_reapply` waits for: the
    /// refresh reverts every `DF*` override, so the overrides go back in
    /// straight after it. The line is the last of the refresh, written after
    /// the flag cache is, and has been in every log that ran past two minutes
    /// at about 120, 241 and 362 seconds.
    ///
    /// `Could not fetch settings` means the fetch failed, which reverts nothing
    /// and is deliberately not this event. The `Skipping flag cache write`
    /// wording was found in a contributor's log (pull request 73).
    SettingsRefreshed,
}

/// One log line, or `None` if it is not one of the four.
///
/// Pure, and the whole of the format knowledge. Every field is found by name
/// rather than by position: the join line has eight comma-separated fields
/// today and a build that adds a ninth must not shift the meaning of the ones
/// before it.
pub fn parse_line(line: &str) -> Option<Event> {
    if line.contains(crate::bloxstrap_rpc::MARKER) {
        return Some(Event::Rpc(line.to_owned()));
    }
    if line.contains("[FLog::SingleSurfaceApp] leaveUGCGameInternal") {
        return Some(Event::Left);
    }
    // Two wordings of the end of a refresh: `finished flag fetch`, seen in every
    // log that ran past two minutes, and `Skipping flag cache write: ...`, which
    // a contributor's log shows in its place in a session on another channel.
    // The initial 2-4 s startup fetch also logs under `[DFLog::FlagCache]` when
    // writing or verifying the flag cache.
    // The failed fetch (`Could not fetch settings`) and the rollout-time line
    // after it apply no document and revert nothing, so they are not events.
    if line.contains("[FLog::DynamicFastVariableReloader]")
        && (line.contains("finished flag fetch") || line.contains("Skipping flag cache write"))
    {
        return Some(Event::SettingsRefreshed);
    }
    if line.contains("[DFLog::FlagCache]")
        && (line.contains("writeFlagCache: Successfully wrote")
            || line.contains("Signature matches last written signature"))
    {
        return Some(Event::SettingsRefreshed);
    }
    if line.contains("[FLog::GameJoinLoadTime]") && line.contains("game_join_loadtime") {
        // All three or none. A join with one id and not the others is a line
        // this parser does not understand, and reporting part of it would put
        // a zero in front of somebody as though it were a place or a person.
        let place_id = field_u64(line, "placeid:")?;
        let universe_id = field_u64(line, "universeid:")?;
        let user_id = field_u64(line, "userid:")?;
        return Some(Event::Joined { place_id, universe_id, user_id });
    }
    // `! Joining game '<jobid>' place <placeid> at <address>`. The job id is
    // the only place the server instance is named, and without it a "Join
    // server" button can only start a *new* server, which is not joining.
    if line.contains("! Joining game '") {
        let job_id = field_after(line, "! Joining game '", &['\''])?.to_owned();
        // A job id is a UUID. Checked rather than trusted because it goes into
        // a URL handed to another program: 36 characters of hex and dashes is
        // the same shape Bloxstrap's own pattern requires, and anything else
        // means this line is not what it looks like.
        if job_id.len() != 36 || !job_id.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-') {
            return None;
        }
        let place_id = field_u64(line, "' place ")?;
        return Some(Event::Joining { job_id, place_id });
    }
    if line.contains("[FLog::Network] UDMUX Address") {
        // The UDMUX address, not the "RCC Server Address" later on the same
        // line -- that one is 10.60.2.168 in the capture, a private address on
        // Roblox's own network that means nothing outside it.
        let address = field_after(line, "UDMUX Address = ", &[',', ' ', '|'])?;
        let rest = line.split_once("UDMUX Address")?.1;
        let port = field_after(rest, "Port = ", &[',', ' ', '|'])?.parse().ok()?;
        return Some(Event::Server { address: address.to_owned(), port });
    }
    if line.contains("Connection accepted from ") {
        if let Some((address, port)) = crate::server_region::parse_connection_accepted(line) {
            return Some(Event::Server { address, port });
        }
    }
    None
}

/// The digits following `key`, stopping at the first non-digit.
fn field_u64(line: &str, key: &str) -> Option<u64> {
    let rest = line.split_once(key)?.1;
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

/// The text following `key`, up to any of `stop`.
fn field_after<'a>(line: &'a str, key: &str, stop: &[char]) -> Option<&'a str> {
    let rest = line.split_once(key)?.1;
    let end = rest.find(|c| stop.contains(&c)).unwrap_or(rest.len());
    let value = &rest[..end];
    (!value.is_empty()).then_some(value)
}

/// How often the directory is re-scanned for a newer log file.
///
/// The engine opens exactly one log per session and appends to it, so this is
/// only looking for the moment that file first appears. A second is far more
/// often than that needs and still costs one `read_dir` of a directory with a
/// handful of entries.
const RESCAN: Duration = Duration::from_secs(1);

struct ActiveLog {
    path: PathBuf,
    file: Option<File>,
    offset: u64,
}

/// Tails the newest log in a directory, reporting whole lines as they land.
///
/// Deliberately poll-driven rather than inotify-backed: this is called from
/// `looper::pump`, which already runs continuously and must never block, and
/// adding a watch descriptor to that loop buys nothing when the caller is
/// polling anyway. It also means no new dependency and no second thread to
/// reason about against the engine's own.
pub struct Watcher {
    dir: PathBuf,
    /// The file being followed, its kept-open handle, and how far into it has
    /// been read.
    ///
    /// Keeping the `File` open across polls turns an idle check into a single
    /// `fstat` instead of `openat` + `fstat` + `close` + `PathBuf::clone`. The
    /// handle is reopened whenever the directory rescan finds a newer log, the
    /// open descriptor is unlinked (`nlink == 0`), or the file shrinks below
    /// `offset` (truncated under the same name).
    current: Option<ActiveLog>,
    last_scan: Option<Instant>,
    /// A partial final line, held until its newline arrives.
    ///
    /// The engine writes with its own buffering and a poll can land mid-line.
    /// Handing half a join line to `parse_line` would return `None` and the
    /// other half would never be seen, so the join would simply be missed --
    /// intermittently, and more often on a busy machine, which is the worst
    /// shape of bug to have here.
    partial: String,
}

impl Watcher {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into(), current: None, last_scan: None, partial: String::new() }
    }

    /// Whatever has been written since the last call.
    ///
    /// Never blocks and never errors: a missing directory, an unreadable file
    /// and a log that has not been created yet are all "nothing to report".
    /// The engine's log appears some seconds into startup, so "not there yet"
    /// is the ordinary case for the first few hundred calls.
    pub fn poll(&mut self) -> Vec<Event> {
        self.rescan_if_due();
        let Some(active) = self.current.as_mut() else { return Vec::new() };

        // Open on first use (or after a previous failure / unlink).
        if active.file.is_none() {
            match File::open(&active.path) {
                Ok(f) => active.file = Some(f),
                Err(_) => return Vec::new(),
            }
        }

        // Single `fstat` on the already-open descriptor.
        let (len, unlinked) = match active.file.as_ref().unwrap().metadata() {
            Ok(meta) => (meta.len(), meta.nlink() == 0),
            Err(_) => {
                active.file = None;
                return Vec::new();
            }
        };

        if unlinked {
            // Replaced or removed under the same name: drop the old fd and
            // reopen from byte 0 if a replacement exists at `active.path`.
            active.file = File::open(&active.path).ok();
            active.offset = 0;
            self.partial.clear();
            let Some(f) = active.file.as_ref() else { return Vec::new() };
            let new_len = f.metadata().map(|m| m.len()).unwrap_or(0);
            if new_len == 0 {
                return Vec::new();
            }
        } else if len < active.offset {
            // Truncated in place under the same name. Start again rather than
            // reading from an offset that no longer means anything.
            active.file = File::open(&active.path).ok();
            active.offset = 0;
            self.partial.clear();
            return Vec::new();
        } else if len == active.offset {
            return Vec::new();
        }

        let Some(file) = active.file.as_mut() else { return Vec::new() };
        if file.seek(SeekFrom::Start(active.offset)).is_err() {
            active.file = None;
            return Vec::new();
        }
        let mut reader = BufReader::new(file);

        let mut events = Vec::new();
        let mut read = active.offset;
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) => break,
                Ok(n) => {
                    read += n as u64;
                    if line.ends_with('\n') {
                        let whole = std::mem::take(&mut self.partial) + line.trim_end();
                        if let Some(event) = parse_line(&whole) {
                            events.push(event);
                        }
                    } else {
                        // No newline: the writer is mid-line. Keep it and stop
                        // -- the offset below still counts it, so the rest
                        // arrives next poll and is joined onto this.
                        self.partial.push_str(&line);
                    }
                }
                Err(_) => break,
            }
        }
        active.offset = read;
        events
    }

    /// Adopt the newest `*.log` in the directory, if it is not the one already
    /// being followed.
    ///
    /// Newest by modification time rather than by the timestamp in the name.
    /// The name's stamp is the engine's own and is in a format this would have
    /// to parse; mtime is what the filesystem already knows and is what
    /// "currently being written" actually means.
    fn rescan_if_due(&mut self) {
        let now = Instant::now();
        if self.last_scan.is_some_and(|t| now.duration_since(t) < RESCAN) {
            return;
        }
        self.last_scan = Some(now);

        let Some(newest) = newest_log(&self.dir) else { return };
        if self.current.as_ref().is_some_and(|a| a.path == newest) {
            return;
        }
        // A log Vanta has not been following may already have content -- the
        // engine writes a few hundred lines before this ever runs. Start at
        // zero rather than at the end: a join that happened during startup is
        // still the join this session is in, and replaying a `Left` from
        // before Vanta was watching cannot happen, because the file is this
        // session's.
        let file = File::open(&newest).ok();
        self.current = Some(ActiveLog { path: newest, file, offset: 0 });
        self.partial.clear();
    }
}

/// The most recently modified `*.log` in `dir`.
fn newest_log(dir: &Path) -> Option<PathBuf> {
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|e| e != "log") {
            continue;
        }
        let Ok(modified) = entry.metadata().and_then(|m| m.modified()) else { continue };
        if best.as_ref().is_none_or(|(t, _)| modified > *t) {
            best = Some((modified, path));
        }
    }
    best.map(|(_, p)| p)
}

// ------------------------------------------------------------- the live one
//
// One watcher for the process, polled from `looper::pump` beside the cookie
// flush -- the same kind of cheap per-tick housekeeping, and on the same
// thread, so nothing here needs a lock against the engine.

/// Where the engine writes its logs, for this profile.
///
/// The same root `load.rs` builds the tree under, from the same function:
/// `VANTA_FILES_DIR` when set, otherwise `data/` under the profile's engine
/// root. This used to repeat the `format!`, which was fine while there was one
/// layout and would have read a Quest run's logs from the phone build's tree
/// once there were two (ADR-053).
fn logs_dir() -> PathBuf {
    crate::profile::engine_data().join("files/appData/logs")
}

/// `VANTA_CLOSE_ON_LEAVE=1` — exit when the user leaves a game.
///
/// Off unless asked for. Closing somebody's client is the least reversible
/// thing this crate does on its own initiative, and the person who wants it
/// wants it deliberately: they launched from a deep link to play one game and
/// have no use for the home screen afterwards. Somebody who did not ask and
/// meets it once has lost their session to a setting they did not know existed.
///
/// The environment gives the starting value; the shell's `settings.set`
/// can change it afterwards ([`set_close_on_leave`], ADR-044), which is safe to
/// read on the log poll's schedule because it is one relaxed atomic load.
fn close_on_leave() -> bool {
    close_on_leave_cell().load(std::sync::atomic::Ordering::Relaxed)
}

fn close_on_leave_cell() -> &'static std::sync::atomic::AtomicBool {
    static ON: OnceLock<std::sync::atomic::AtomicBool> = OnceLock::new();
    ON.get_or_init(|| {
        std::sync::atomic::AtomicBool::new(close_on_leave_for(
            std::env::var("VANTA_CLOSE_ON_LEAVE").ok().as_deref(),
        ))
    })
}

/// Change the quit-on-leave choice in a running client.
pub fn set_close_on_leave(on: bool) {
    close_on_leave_cell().store(on, std::sync::atomic::Ordering::Relaxed);
}

/// The value in force, for the live-settings `get` reply.
pub fn current_close_on_leave() -> bool {
    close_on_leave()
}

/// The gate's decision, split out so it can be tested -- `close_on_leave`
/// caches into a `OnceLock`, so a test that set the variable would decide the
/// answer for every other test in the binary.
fn close_on_leave_for(v: Option<&str>) -> bool {
    matches!(v, Some("1") | Some("true") | Some("yes"))
}

/// Poll the log once and act on whatever it said.
///
/// Called every pump tick. Costs one `read_dir` a second and, in between, one
/// `File::open` and a metadata read that usually finds nothing new -- the same
/// order of work as the cookie flush it sits beside.
/// The game's presence so far, folded from every `[BloxstrapRPC]` line seen.
///
/// One per process because there is one engine in it. Reset when the user
/// leaves, so a presence from the last experience does not outlive it.
static PRESENCE: Mutex<crate::bloxstrap_rpc::Presence> =
    Mutex::new(crate::bloxstrap_rpc::Presence::new());

/// What the client is doing, as the log has revealed it so far.
///
/// **Vanta keeps this so plugins do not each keep their own.** See
/// `vanta_plugins::state`: the alternative is every plugin folding the same
/// event stream into a private copy and getting it wrong in its own way. The
/// core events are published from the same updates that write this, so the
/// snapshot and the stream are one thing seen twice rather than two things
/// that can disagree.
static STATE: Mutex<vanta_plugins::state::SessionState> =
    Mutex::new(vanta_plugins::state::SessionState {
        place_id: None,
        universe_id: None,
        job_id: None,
        server_address: None,
        user_id: None,
        joined_at: None,
    });

/// A copy of the session state, for `state.get`.
pub fn session_state() -> vanta_plugins::state::SessionState {
    STATE.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// The presence payload plus what the buttons need.
///
/// Merged here rather than in `Presence::to_payload`, which is the parsed
/// BloxstrapRPC protocol and has no business knowing about a job id it never
/// sees.
/// The name of the Roblox badge asset in Vanta's Discord application.
///
/// Bloxstrap uses the literal key `roblox` for the same badge, and a Discord
/// asset key is a plain name rather than a URL -- so this renders only once an
/// image called `roblox` has been uploaded to the application's Rich Presence
/// assets in Discord's developer portal. That is a one-off manual step nobody
/// but the application's owner can take, and until it is taken Discord simply
/// draws no badge; nothing else about the presence changes.
const ROBLOX_BADGE_KEY: &str = "roblox";

/// What Vanta shows for a game that says nothing of its own.
///
/// Resolved once per experience by [`resolve_game_in_background`].
static DEFAULTS: Mutex<Option<crate::roblox_api::GameDetails>> = Mutex::new(None);

/// Monotonically increasing session generation counter, incremented on every
/// [`Event::Joined`] and [`Event::Left`]. Background HTTP lookups capture the
/// generation at spawn time and discard their result if the session changed
/// before the request finished (fast leave or game switch).
static SESSION_GEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn next_session_generation() -> u64 {
    SESSION_GEN.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1
}

fn current_session_generation() -> u64 {
    SESSION_GEN.load(std::sync::atomic::Ordering::SeqCst)
}

/// Commit a background game-metadata lookup only if `gen` still matches the
/// active session generation while holding `DEFAULTS`. Returns `true` when the
/// details were accepted and `false` when discarded as stale.
fn commit_resolved_game(gen: u64, details: Option<crate::roblox_api::GameDetails>) -> bool {
    let mut slot = DEFAULTS.lock().unwrap_or_else(|e| e.into_inner());
    if current_session_generation() != gen {
        return false;
    }
    *slot = details;
    true
}

/// Look up the experience and republish, off the poll thread.
///
/// **Spawned rather than called inline.** These are HTTP requests on the house
/// client's twenty-second worst case, and the log watcher's poll loop is not a
/// place to spend that -- a stalled watcher stops noticing BloxstrapRPC lines,
/// which is the one thing it exists to notice.
fn resolve_game_in_background(universe_id: u64, gen: u64) {
    std::thread::spawn(move || {
        let details = crate::roblox_api::resolve_game(universe_id);
        let log_line = details.as_ref().map(|d| format!("[vanta] game: {} by {}", d.name, d.creator));
        if !commit_resolved_game(gen, details) {
            return;
        }
        if let Some(line) = log_line {
            println!("{line}");
        }
        if current_session_generation() != gen {
            return;
        }
        // The join already published a presence without any of this; this is
        // the second one, with the name and the icon filled in.
        crate::plugin_host::publish_core(
            vanta_plugins::core_events::GAME_PRESENCE,
            presence_payload(),
        );
    });
}

/// The presence to publish: what the game asked for, over Vanta's defaults.
///
/// **The default set is Bloxstrap's, field for field**, because a user moving
/// between the two should not find the presence means something different --
/// the experience's name as `details`, `by <creator>` as `state`, the game's
/// icon as the large image with the name as its hover text, and the Roblox
/// badge as the small image. Read from `DiscordRichPresence.cs` (MIT,
/// Bloxstrap Labs); the shape adapted, not the code.
///
/// A game's own BloxstrapRPC wins wherever it said something, which is what
/// `Slot::Default` already means -- so this fills only the keys `to_payload`
/// left out, and never overwrites one it wrote.
fn presence_payload() -> serde_json::Value {
    let mut payload = PRESENCE.lock().unwrap_or_else(|e| e.into_inner()).to_payload();
    let state = STATE.lock().unwrap_or_else(|e| e.into_inner());
    let Some(map) = payload.as_object_mut() else {
        return payload;
    };
    let Some(place) = state.place_id else {
        return payload;
    };

    map.insert("place_id".into(), serde_json::json!(place));
    if let Some(job) = &state.job_id {
        map.insert("job_id".into(), serde_json::json!(job));
    }
    if let Some(joined) = state.joined_at {
        map.entry("start").or_insert_with(|| serde_json::json!(joined));
    }

    if let Some(universe) = state.universe_id {
        // The icon is only a default: a game that set its own large image has
        // already written the key, and `or_insert_with` leaves it alone.
        map.entry("large_image_key")
            .or_insert_with(|| serde_json::json!(crate::roblox_api::universe_key(universe)));
    }
    // **The small image is a Discord application asset, not a URL**, which is
    // what Bloxstrap does too -- its `SmallImageKey` is the literal string
    // `roblox`. It renders only if an asset by that name has been uploaded to
    // the Discord application the presence is published under; if it has not,
    // Discord shows no badge and everything else is unaffected.
    map.entry("small_image_key").or_insert_with(|| serde_json::json!(ROBLOX_BADGE_KEY));
    map.entry("small_text").or_insert_with(|| serde_json::json!("Roblox"));

    if let Some(details) = DEFAULTS.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
        map.entry("details").or_insert_with(|| serde_json::json!(details.name));
        map.entry("large_text").or_insert_with(|| serde_json::json!(details.name));
        if !details.creator.is_empty() {
            map.entry("state").or_insert_with(|| serde_json::json!(format!("by {}", details.creator)));
        }
    }
    payload
}

/// Minimum interval between global `game_log::poll()` file checks.
/// `looper::pump` throttles calls to 200 ms (5 Hz); keeping this slightly
/// shorter (180 ms) ensures direct callers stay rate-limited while never
/// causing `looper::pump`'s 200 ms ticks to skip alternate polls.
const POLL_INTERVAL: Duration = Duration::from_millis(180);

pub fn poll() {
    static LAST_POLL: Mutex<Option<Instant>> = Mutex::new(None);
    {
        let mut last = LAST_POLL.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        if last.is_some_and(|t| now.duration_since(t) < POLL_INTERVAL) {
            return;
        }
        *last = Some(now);
    }
    static WATCHER: Mutex<Option<Watcher>> = Mutex::new(None);
    let mut guard = WATCHER.lock().unwrap_or_else(|e| e.into_inner());
    let watcher = guard.get_or_insert_with(|| Watcher::new(logs_dir()));

    for event in watcher.poll() {
        match event {
            // Not a game event. Handed to the worker that owns the re-apply,
            // which does the work off this thread; see `flag_reapply`.
            Event::SettingsRefreshed => crate::flag_reapply::note_engine_refresh(),
            Event::Joined { place_id, universe_id, user_id } => {
                println!(
                    "[vanta] game: joined place {place_id} (universe {universe_id}) as {user_id}"
                );
                let gen = next_session_generation();
                let universe_changed = {
                    let mut state = STATE.lock().unwrap_or_else(|e| e.into_inner());
                    let changed = state.universe_id != Some(universe_id);
                    // A job id belongs to the place it was seen for. Keeping
                    // one across a teleport would name a server the player has
                    // left, and the join button would send people to it.
                    if state.place_id != Some(place_id) {
                        state.job_id = None;
                    }
                    state.place_id = Some(place_id);
                    state.universe_id = Some(universe_id);
                    state.user_id = Some(user_id);
                    // Set here rather than at `Joining`, because this is the
                    // line that means the join finished. `joined_at` is a
                    // timestamp and not a duration on purpose -- see
                    // `SessionState` -- so two plugins asking a second apart
                    // agree.
                    state.joined_at = Some(now_secs());
                    changed
                };
                if universe_changed {
                    *DEFAULTS.lock().unwrap_or_else(|e| e.into_inner()) = None;
                }
                // Told to the launcher from the same update that wrote the state,
                // so the snapshot it can ask for and the event it hears are one
                // thing seen twice (ADR-055).
                let snap = session_state();
                crate::server_region::on_joined(place_id, snap.joined_at);
                crate::control::game_joined(&snap);
                crate::plugin_host::publish_core(
                    vanta_plugins::core_events::GAME_PRESENCE,
                    presence_payload(),
                );
                resolve_game_in_background(universe_id, gen);
            }
            Event::Joining { job_id, place_id } => {
                println!("[vanta] game: joining server {job_id} of place {place_id}");
                crate::server_region::on_joining(place_id, &job_id);
                let mut state = STATE.lock().unwrap_or_else(|e| e.into_inner());
                // A different place means a different visit, so nothing from
                // the last one carries over. Same place is a rejoin of the
                // server we were told about, and the rest is still true.
                if state.place_id != Some(place_id) {
                    state.left();
                }
                state.place_id = Some(place_id);
                state.job_id = Some(job_id);
            }
            Event::Server { address, port } => {
                println!("[vanta] game: server {address}:{port}");
                crate::server_region::on_server(&address, port);
                STATE.lock().unwrap_or_else(|e| e.into_inner()).server_address = Some(address);
            }
            Event::Left => {
                println!("[vanta] game: left");
                let _ = next_session_generation();
                crate::server_region::on_left();
                // The experience's presence goes with the experience. Without
                // this, leaving a game that set a presence would leave its
                // details on the Discord profile until another game replaced
                // them -- and on the home screen that is simply wrong.
                *PRESENCE.lock().unwrap_or_else(|e| e.into_inner()) =
                    crate::bloxstrap_rpc::Presence::new();
                // The experience's name and creator go with the experience too.
                *DEFAULTS.lock().unwrap_or_else(|e| e.into_inner()) = None;
                STATE.lock().unwrap_or_else(|e| e.into_inner()).left();
                crate::control::game_left();
                crate::plugin_host::publish_core(
                    vanta_plugins::core_events::GAME_PRESENCE,
                    serde_json::json!({}),
                );
                if close_on_leave() {
                    // Through the same door the window's close button uses,
                    // not `process::exit`. The profile lock is released by
                    // exiting however it happens (see ADR-012), but the engine
                    // has a cookie jar and storage that a hard exit would drop
                    // mid-write, and `request_quit` is what every other way
                    // out of this client already goes through.
                    println!("[vanta] game: VANTA_CLOSE_ON_LEAVE is set; closing");
                    crate::android::looper::request_quit();
                }
            }
            Event::Rpc(line) => match crate::bloxstrap_rpc::parse_line(&line) {
                // **This is the first caller `bloxstrap_rpc` has ever had.**
                // Everything below the parse is still unwired: the presence
                // broker lives in `vanta-plugins` and the client does not
                // hold one yet, so for now the command is reported and
                // dropped. Reported rather than silently dropped, because a
                // game author testing their own presence needs to see that
                // Vanta read the line.
                Ok(Some(command)) => {
                    // **Folded, then published.** BloxstrapRPC is a stream of
                    // partial updates -- a game sends `details` on one line
                    // and `state` on another, and "leave this field alone" is
                    // a distinct value from "clear it". `Presence::apply` is
                    // what knows that, so the subscriber receives the merged
                    // picture rather than having to reimplement the folding.
                    let mut merged = PRESENCE.lock().unwrap_or_else(|e| e.into_inner());
                    if let Some(launch) = merged.apply(command) {
                        // The join button's payload. Carried no further yet:
                        // acting on it means building a launch URL and
                        // handing it to Discord as a join secret, which is a
                        // different capability from showing a presence and
                        // has not been asked for.
                        println!("[vanta] game: BloxstrapRPC launch data ({} bytes)", launch.len());
                    } else {
                        // Whatever pictures the game is now asking for have to
                        // be resolved before they can be shown, and that is
                        // HTTP -- so it happens on its own thread and the
                        // presence is published again when it lands. The first
                        // publish below carries the text immediately, which is
                        // the part a game changes often.
                        let wanted: Vec<String> = [merged.large_image.value(), merged.small_image.value()]
                            .into_iter()
                            .flatten()
                            .map(|image| image.asset_id.clone())
                            .collect();
                        drop(merged);
                        let gen = current_session_generation();
                        crate::plugin_host::publish_core(
                            vanta_plugins::core_events::GAME_PRESENCE,
                            presence_payload(),
                        );
                        if !wanted.is_empty() {
                            std::thread::spawn(move || {
                                for asset in &wanted {
                                    crate::roblox_api::resolve_asset(asset);
                                }
                                if current_session_generation() != gen {
                                    return;
                                }
                                crate::plugin_host::publish_core(
                                    vanta_plugins::core_events::GAME_PRESENCE,
                                    presence_payload(),
                                );
                            });
                        }
                    }
                }
                Ok(None) => {}
                Err(e) => println!("[vanta] game: unusable BloxstrapRPC line: {e}"),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Every line below is verbatim from the capture named in the module
    /// comment. Hand-written approximations of a log format are how a parser
    /// comes to work on the example and fail on the file.
    const JOIN: &str = "2026-08-31T03:14:18.878Z,159.878708,bf08c6c0,6 [FLog::GameJoinLoadTime] \
         Report game_join_loadtime: sid:6baeb082-4060-42cf-85f4-e8282c7a8bfd, \
         clienttime:1788146058.128000021, join_time:1.2011154180000005454, referral_page:, \
         placeid:17625359962, userid:1826805362, universeid:6035872082, ";
    const UDMUX: &str = "2026-08-31T03:14:18.884Z,159.884491,bf08c6c0,7 [FLog::Network] \
         UDMUX Address = 128.116.51.33, Port = 50363 | RCC Server Address = 10.60.2.168, Port = 50363";
    const LEAVE: &str =
        "2026-08-31T03:14:53.310Z,194.310760,bd3bd6c0,6 [FLog::SingleSurfaceApp] leaveUGCGameInternal";

    #[test]
    fn a_join_yields_every_id_on_the_line() {
        assert_eq!(
            parse_line(JOIN),
            Some(Event::Joined {
                place_id: 17625359962,
                universe_id: 6035872082,
                user_id: 1826805362,
            })
        );
    }

    /// **The server instance, which is what a join link needs.**
    ///
    /// Verbatim from the same capture. Without the job id a "Join server"
    /// button can only start a new server, which is the opposite of joining --
    /// so this line, not the `GameJoinLoadTime` one, is what makes the button
    /// possible.
    #[test]
    fn joining_yields_the_job_id() {
        let line = "2026-08-31T03:14:17.000Z,1,a,6 [FLog::Output] ! Joining game \
                    '3182c122-8e1c-4f50-b0ac-6fb67ba0082f' place 17625359962 at 10.60.2.168";
        assert_eq!(
            parse_line(line),
            Some(Event::Joining {
                job_id: "3182c122-8e1c-4f50-b0ac-6fb67ba0082f".into(),
                place_id: 17625359962,
            })
        );
    }

    /// A job id that is not a UUID is refused rather than put into a URL.
    ///
    /// This value ends up in a `roblox://` link handed to another program, so
    /// "it came out of a log line that looked right" is not enough.
    #[test]
    fn a_job_id_that_is_not_a_uuid_is_refused() {
        for bad in ["short", "../../etc/passwd", "3182c122 8e1c 4f50 b0ac 6fb67ba0082f!!"] {
            let line = format!("[FLog::Output] ! Joining game '{bad}' place 1 at 10.0.0.1");
            assert_eq!(parse_line(&line), None, "{bad}");
        }
    }

    /// **The public address, not the private one.**
    ///
    /// The same line carries `RCC Server Address = 10.60.2.168`, which is on
    /// Roblox's own network and means nothing outside it. A parser that took
    /// the last `Port = ` or searched the whole line for an address would pick
    /// that one, and it would look plausible right up until somebody tried to
    /// resolve it.
    #[test]
    fn the_server_is_the_udmux_address_and_not_the_rcc_one() {
        assert_eq!(
            parse_line(UDMUX),
            Some(Event::Server { address: "128.116.51.33".into(), port: 50363 })
        );
    }

    #[test]
    fn connection_accepted_from_real_2745_log_yields_server_event() {
        let line = "2026-11-21T13:47:22.467Z,105.467804,a5fbc6c0,6,Info [DFLog::NetworkClient] \
                    Connection accepted from 128.116.14.33|59539";
        assert_eq!(
            parse_line(line),
            Some(Event::Server { address: "128.116.14.33".into(), port: 59539 })
        );
    }

    #[test]
    fn leaving_is_recognised() {
        assert_eq!(parse_line(LEAVE), Some(Event::Left));
    }

    /// **A disconnect is not a leave, and confusing them closes the client
    /// during a teleport.**
    ///
    /// The capture has ten disconnect-shaped lines against two real leaves --
    /// a websocket 401, a peer disconnect, `Client:Disconnect` twice per
    /// session, a numbered `Disconnection Notification`. Close-on-leave acts
    /// on `Event::Left`, so every one of these must parse as nothing.
    #[test]
    fn a_disconnect_is_not_a_leave() {
        for line in [
            "2026-08-31T03:11:44.510Z,5.510133,409d16c0,6 [DFLog::SignalRCoreError] ID: 1 \
             Disconnected - Websocket error: 401 Unauthorized",
            "2026-08-31T03:14:53.338Z,194.338837,bf08c6c0,6,Info [DFLog::NetworkClient] \
             Client:Disconnect",
            "2026-08-31T03:16:55.333Z,316.333099,5b43f6c0,7 [FLog::Network] Connection lost: \
             connectMode: Peer Disconnected, timeMS:316332, connectionTime 200359",
            "2026-08-31T03:16:55.333Z,316.333221,5b43f6c0,7 [FLog::Network] Disconnection \
             Notification. Reason: 267",
            "2026-08-31T03:14:54.395Z,195.395447,bf08c6c0,7 \
             [DFLog::MegaReplicatorLogDisconnectCleanUpLog] Destroying MegaReplicator.",
        ] {
            assert_eq!(parse_line(line), None, "{line}");
        }
    }

    /// Ordinary log traffic must not look like anything.
    #[test]
    fn an_uninteresting_line_is_uninteresting() {
        for line in ["", "   ", "not a log line at all", "[FLog::Output] hello"] {
            assert_eq!(parse_line(line), None, "{line:?}");
        }
    }

    /// A `[BloxstrapRPC]` line is handed on whole, not decoded here.
    #[test]
    fn an_rpc_line_is_passed_through_for_the_protocol_module() {
        let line = "2026-08-31T03:14:18.878Z,1,a,6 [FLog::Output] [BloxstrapRPC] {\"command\":\"x\"}";
        assert_eq!(parse_line(line), Some(Event::Rpc(line.to_owned())));
    }

    /// A join line missing one of its two ids is not half a join.
    #[test]
    fn a_join_line_without_both_ids_is_not_a_join() {
        let no_universe = "[FLog::GameJoinLoadTime] Report game_join_loadtime: placeid:1, userid:2,";
        assert_eq!(parse_line(no_universe), None);
    }

    /// **A line split across two polls must still be seen.**
    ///
    /// The engine buffers its own writes and a poll can land mid-line. The
    /// failure this catches is silent and intermittent: half a join line
    /// parses as nothing, the other half parses as nothing, and the join is
    /// simply missed -- more often on a busy machine, which is exactly when
    /// somebody is playing.
    #[test]
    fn a_line_that_arrives_in_two_pieces_is_still_parsed() {
        let dir = std::env::temp_dir().join(format!("vanta-gamelog-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("session.log");

        let (head, tail) = JOIN.split_at(60);
        std::fs::write(&log, head).unwrap();

        let mut w = Watcher::new(&dir);
        assert_eq!(w.poll(), Vec::new(), "half a line is not an event yet");

        std::fs::write(&log, format!("{head}{tail}\n")).unwrap();
        // The rescan is rate-limited and this is the same file, so poll again
        // directly rather than waiting a second for a scan that changes nothing.
        assert_eq!(
            w.poll(),
            vec![Event::Joined {
                place_id: 17625359962,
                universe_id: 6035872082,
                user_id: 1826805362,
            }],
            "the two halves must join up"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Run the parser over a whole real log and say what it found.
    ///
    /// `#[ignore]` because it needs a file this repository does not and must
    /// not contain -- a log is Roblox's output and carries a user id. Point it
    /// at one and it becomes the strongest check available:
    ///
    /// ```text
    /// VANTA_GAME_LOG=~/.var/app/org.vinegarhq.Sober/data/sober/appData/logs/<file> \
    ///     cargo test -p vanta-runtime --lib whole_log -- --ignored --nocapture
    /// ```
    ///
    /// The assertions are deliberately weak -- at least one join and one leave
    /// -- because the point is the printed summary. A parser that silently
    /// stopped matching after a Roblox update would still pass every test
    /// above, all of which use lines frozen into this file.
    #[test]
    #[ignore = "needs a real engine log; set VANTA_GAME_LOG"]
    fn whole_log_from_a_real_session() {
        let Ok(path) = std::env::var("VANTA_GAME_LOG") else {
            panic!("set VANTA_GAME_LOG to a real *_Player_*.log");
        };
        let text = std::fs::read_to_string(&path).expect("read the log");
        let (mut joins, mut leaves, mut servers, mut rpc) = (0, 0, 0, 0);
        for line in text.lines() {
            match parse_line(line) {
                Some(Event::Joined { place_id, universe_id, user_id }) => {
                    joins += 1;
                    println!("join   place {place_id} universe {universe_id} user {user_id}");
                }
                Some(Event::Joining { job_id, place_id }) => {
                    println!("server {job_id} place {place_id}");
                }
                Some(Event::Server { address, port }) => {
                    servers += 1;
                    println!("server {address}:{port}");
                }
                Some(Event::Left) => {
                    leaves += 1;
                    println!("leave");
                }
                Some(Event::Rpc(_)) => rpc += 1,
                Some(Event::SettingsRefreshed) => println!("settings refresh finished"),
                None => {}
            }
        }
        println!("{} lines: {joins} joins, {leaves} leaves, {servers} servers, {rpc} rpc",
                 text.lines().count());
        assert!(joins > 0, "no join found in {path}");
        assert!(leaves > 0, "no leave found in {path}");
    }

    /// **Close-on-leave is off unless asked for, and only the explicit words
    /// turn it on.** Closing somebody's client is the least reversible thing
    /// this crate does unprompted; a typo in the variable must read as "no".
    #[test]
    fn closing_on_leave_is_opt_in() {
        assert!(!close_on_leave_for(None), "absent means no");
        assert!(!close_on_leave_for(Some("0")));
        assert!(!close_on_leave_for(Some("")));
        assert!(!close_on_leave_for(Some("off")));
        assert!(!close_on_leave_for(Some("no")));
        for on in ["1", "true", "yes"] {
            assert!(close_on_leave_for(Some(on)), "{on} should turn it on");
        }
    }

    /// **A game's own presence, from the line it printed to the payload a
    /// plugin receives.** The whole chain in one test, because it crosses three
    /// modules -- this one recognises the line, `bloxstrap_rpc` parses and
    /// folds it, and `Presence::to_payload` shapes what goes on the bus -- and
    /// a break anywhere in it looks identical from Discord: nothing happens.
    ///
    /// The two lines are the shape a game actually prints, wrapped the way the
    /// engine logs it. Partial updates on purpose: BloxstrapRPC sends fields
    /// one at a time and "leave this alone" is a distinct value from "clear
    /// it", so a subscriber seeing only the second line must still get the
    /// details from the first.
    #[test]
    fn a_games_bloxstrap_rpc_becomes_a_presence_payload() {
        let printed = [
            r#"2026-09-01T00:10:00.000Z,1,a,6 [FLog::Output] [BloxstrapRPC] {"command":"SetRichPresence","data":{"details":"Fighting the Boss","state":"Round 3 of 5"}}"#,
            r#"2026-09-01T00:10:05.000Z,1,a,6 [FLog::Output] [BloxstrapRPC] {"command":"SetRichPresence","data":{"timeStart":1788200000}}"#,
        ];
        let mut merged = crate::bloxstrap_rpc::Presence::new();
        for line in printed {
            let Some(Event::Rpc(raw)) = parse_line(line) else {
                panic!("the watcher did not recognise {line}");
            };
            let command = crate::bloxstrap_rpc::parse_line(&raw)
                .expect("parses")
                .expect("is a command");
            assert!(merged.apply(command).is_none(), "neither line is launch data");
        }

        let payload = merged.to_payload();
        assert_eq!(payload["details"], "Fighting the Boss");
        assert_eq!(payload["state"], "Round 3 of 5", "the first line must survive the second");
        assert_eq!(payload["start"], 1788200000);
        assert!(payload.get("end").is_none(), "a field the game never set is absent, not null");
    }

    /// Leaving clears it, so one experience's presence does not outlive it.
    #[test]
    fn a_fresh_presence_carries_nothing() {
        let payload = crate::bloxstrap_rpc::Presence::new().to_payload();
        assert_eq!(payload, serde_json::json!({}));
    }

    /// Lines the reloader logs around its 120 s refresh: the finished line
    /// verbatim from `2.734.0.917_20260824T023830Z_Player_ee712_last.log`, the
    /// failed fetch from `2.730.0.790_20260802T042333Z_Player_e5e7f_last.log`.
    /// The `rollout` line that follows a failed fetch is not one either.
    #[test]
    fn a_refresh_is_an_event_and_a_failed_fetch_is_not() {
        let finished = "2026-08-24T02:40:30.525Z,120.525764,c616d6c0,6,Info \
             [FLog::DynamicFastVariableReloader] DynamicFastVariableReloader finished flag fetch. \
             Tombstone status: invalid";
        let failed = "2026-08-02T04:25:33.438Z,120.438156,348886c0,6,Warning \
             [FLog::DynamicFastVariableReloader] Could not fetch settings";
        let rollout = "2026-08-02T04:25:33.438Z,120.438156,348886c0,6,Info \
             [FLog::DynamicFastVariableReloader] Setting rollout time to local time";
        assert_eq!(parse_line(finished), Some(Event::SettingsRefreshed));
        let skipped = "2026-09-30T22:33:02.260Z,120,260689,61bfb6c0,6,Info \
             [FLog::DynamicFastVariableReloader] Skipping flag cache write: server channel differs \
             from current session channel, preserving prefetched cache";
        assert_eq!(parse_line(skipped), Some(Event::SettingsRefreshed), "another wording of the same refresh");
        assert_eq!(parse_line(failed), None);
        assert_eq!(parse_line(rollout), None);
    }

    /// The same, through the tail: a watcher following a growing log reports
    /// the refresh once, when its line is complete.
    #[test]
    fn the_watcher_reports_a_refresh_when_the_line_lands() {
        let dir = std::env::temp_dir().join(format!("vanta-game-log-refresh-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("player.log");
        let line = "2026-08-24T02:40:30.525Z,120.525764,c616d6c0,6,Info \
             [FLog::DynamicFastVariableReloader] DynamicFastVariableReloader finished flag fetch. \
             Tombstone status: invalid";
        std::fs::write(&log, "2026-08-24T02:40:30.489Z,120.4,c6,6 [FLog::Output] Settings Date header\n").unwrap();
        let mut w = Watcher::new(&dir);
        assert_eq!(w.poll(), Vec::new());
        // Half a line first: the writer is mid-line, and the event must wait.
        let (head, tail) = line.split_at(40);
        std::fs::OpenOptions::new().append(true).open(&log).unwrap().write_all(head.as_bytes()).unwrap();
        assert_eq!(w.poll(), Vec::new());
        let mut f = std::fs::OpenOptions::new().append(true).open(&log).unwrap();
        f.write_all(tail.as_bytes()).unwrap();
        f.write_all(b"\n").unwrap();
        assert_eq!(w.poll(), vec![Event::SettingsRefreshed]);
        assert_eq!(w.poll(), Vec::new(), "reported once");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An absent directory is silence, not an error.
    ///
    /// This runs from the engine's own pump before the engine has written
    /// anything, which is the ordinary case for the first seconds of every
    /// launch. A `Watcher` that returned errors there would need handling at a
    /// call site that has nothing useful to do with them.
    #[test]
    fn nothing_to_watch_is_not_a_failure() {
        let mut w = Watcher::new("/nonexistent/vanta/logs");
        assert_eq!(w.poll(), Vec::new());
        assert_eq!(w.poll(), Vec::new());
    }

    #[test]
    fn watcher_reuses_open_fd_and_recovers_from_truncate_and_unlink() {
        use std::os::unix::io::AsRawFd;
        let dir = std::env::temp_dir().join(format!("vanta-gamelog-fd-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("session.log");
        std::fs::write(&log, format!("{JOIN}\n")).unwrap();

        let mut w = Watcher::new(&dir);
        assert_eq!(w.poll().len(), 1);
        let fd1 = w.current.as_ref().and_then(|a| a.file.as_ref()).unwrap().as_raw_fd();
        // Idle poll keeps the exact same open file descriptor.
        assert!(w.poll().is_empty());
        let fd2 = w.current.as_ref().and_then(|a| a.file.as_ref()).unwrap().as_raw_fd();
        assert_eq!(fd1, fd2, "idle poll must reuse open File handle");

        // Truncate shorter than offset: resets cleanly and reads new content.
        std::fs::write(&log, "short\n").unwrap();
        assert!(w.poll().is_empty(), "truncation resets offset");
        std::fs::OpenOptions::new().append(true).open(&log).unwrap().write_all(b"[FLog::SingleSurfaceApp] leaveUGCGameInternal\n").unwrap();
        assert_eq!(w.poll(), vec![Event::Left]);

        // Unlink and recreate under the same name: detected via nlink() == 0.
        std::fs::remove_file(&log).unwrap();
        std::fs::write(&log, "[FLog::SingleSurfaceApp] leaveUGCGameInternal\n").unwrap();
        assert_eq!(w.poll(), vec![Event::Left]);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn flag_cache_write_and_signature_match_lines_trigger_settings_refreshed() {
        let wrote = "2026-11-21T13:45:04.150Z,4.150123,c616d6c0,6,Info \
             [DFLog::FlagCache] writeFlagCache: Successfully wrote 372673 bytes";
        let sig_match = "2026-11-21T13:45:04.151Z,4.151000,c616d6c0,6,Info \
             [DFLog::FlagCache] Signature matches last written signature";
        let unrelated = "2026-11-21T13:45:01.010Z,1.010000,c616d6c0,6,Info \
             [DFLog::FlagCache] readFlagCache: cache missing";

        assert_eq!(parse_line(wrote), Some(Event::SettingsRefreshed));
        assert_eq!(parse_line(sig_match), Some(Event::SettingsRefreshed));
        assert_eq!(parse_line(unrelated), None);
    }

    #[test]
    fn stale_async_game_metadata_lookup_is_discarded_after_leave_or_switch() {
        let details = |name: &str, creator: &str| {
            Some(crate::roblox_api::GameDetails {
                name: name.to_string(),
                creator: creator.to_string(),
            })
        };

        // Case 1: Join game A, then leave before HTTP returns.
        *DEFAULTS.lock().unwrap_or_else(|e| e.into_inner()) = None;
        let gen_a = next_session_generation();
        let _gen_left = next_session_generation();
        *DEFAULTS.lock().unwrap_or_else(|e| e.into_inner()) = None;

        assert!(
            !commit_resolved_game(gen_a, details("Stale Game A", "Studio A")),
            "response from before Event::Left must be discarded"
        );
        assert!(
            DEFAULTS.lock().unwrap_or_else(|e| e.into_inner()).is_none(),
            "DEFAULTS must stay empty after leave"
        );

        // Case 2: Join game A, immediately switch to game B; B finishes first, then slow A arrives.
        let gen_first = next_session_generation();
        let gen_second = next_session_generation();

        assert!(commit_resolved_game(gen_second, details("Game B", "Studio B")));
        assert!(
            !commit_resolved_game(gen_first, details("Slow Game A", "Studio A")),
            "late response from previous game must not overwrite current game's DEFAULTS"
        );
        let guard = DEFAULTS.lock().unwrap_or_else(|e| e.into_inner());
        let active = guard.as_ref().expect("Game B details should remain");
        assert_eq!(active.name, "Game B");
        assert_eq!(active.creator, "Studio B");
        drop(guard);

        *DEFAULTS.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
}
