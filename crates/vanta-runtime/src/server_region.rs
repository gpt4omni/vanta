//! Local Roblox server region detector, invite link builder, and quick rejoin.
//!
//! Resolves Roblox AS22697 (`128.116.0.0/17`) server IPs strictly locally via a
//! compiled CIDR table — zero network requests, zero third-party GeoIP tracking.
//! Server connection details are fed from the engine's own log file via
//! [`crate::game_log`], staying 100% compliant with ADR-001.

use std::net::Ipv4Addr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

/// One CIDR-to-region entry inside Roblox's AS22697 (`128.116.0.0/17`) space.
#[derive(Debug, Clone, Copy)]
struct CidrRegion {
    network: u32,
    mask: u32,
    region: &'static str,
    city: &'static str,
}

impl CidrRegion {
    const fn new(a: u8, b: u8, c: u8, d: u8, prefix: u8, region: &'static str, city: &'static str) -> Self {
        let net = ((a as u32) << 24) | ((b as u32) << 16) | ((c as u32) << 8) | (d as u32);
        let mask = if prefix == 0 { 0 } else { !0u32 << (32 - prefix) };
        Self { network: net & mask, mask, region, city }
    }

    #[inline]
    fn matches(&self, ip_bits: u32) -> bool {
        (ip_bits & self.mask) == self.network
    }
}

/// Local table of known Roblox AS22697 datacenter prefixes.
///
/// Most specific prefixes appear first; any IPv4 outside these ranges falls
/// back cleanly to `"Roblox Server (<ip>)"` with zero network traffic.
const ROBLOX_CIDRS: &[CidrRegion] = &[
    // US-West: San Jose / Los Angeles / Seattle
    CidrRegion::new(128, 116, 0, 0, 24, "US-West", "San Jose"),
    CidrRegion::new(128, 116, 1, 0, 24, "US-West", "Los Angeles"),
    CidrRegion::new(128, 116, 6, 0, 24, "US-West", "San Jose"),
    CidrRegion::new(128, 116, 15, 0, 24, "US-West", "Los Angeles"),
    CidrRegion::new(128, 116, 24, 0, 24, "US-West", "San Jose"),
    CidrRegion::new(128, 116, 46, 0, 24, "US-West", "Seattle"),
    CidrRegion::new(128, 116, 63, 0, 24, "US-West", "Los Angeles"),
    CidrRegion::new(128, 116, 64, 0, 24, "US-West", "San Jose"),
    CidrRegion::new(128, 116, 75, 0, 24, "US-West", "Seattle"),
    CidrRegion::new(128, 116, 90, 0, 24, "US-West", "Los Angeles"),
    CidrRegion::new(128, 116, 108, 0, 24, "US-West", "San Jose"),
    CidrRegion::new(128, 116, 109, 0, 24, "US-West", "Los Angeles"),
    CidrRegion::new(128, 116, 110, 0, 24, "US-West", "Seattle"),
    CidrRegion::new(128, 116, 119, 0, 24, "US-West", "San Jose"),
    // US-East: Ashburn / New York / Miami
    CidrRegion::new(128, 116, 4, 0, 23, "US-East", "Ashburn"),
    CidrRegion::new(128, 116, 10, 0, 23, "US-East", "Ashburn"),
    CidrRegion::new(128, 116, 31, 0, 24, "US-East", "New York"),
    CidrRegion::new(128, 116, 32, 0, 24, "US-East", "Ashburn"),
    CidrRegion::new(128, 116, 34, 0, 24, "US-East", "New York"),
    CidrRegion::new(128, 116, 45, 0, 24, "US-East", "Miami"),
    CidrRegion::new(128, 116, 53, 0, 24, "US-East", "Ashburn"),
    CidrRegion::new(128, 116, 54, 0, 24, "US-East", "Miami"),
    CidrRegion::new(128, 116, 56, 0, 24, "US-East", "Ashburn"),
    CidrRegion::new(128, 116, 65, 0, 24, "US-East", "Ashburn"),
    CidrRegion::new(128, 116, 74, 0, 24, "US-East", "Miami"),
    CidrRegion::new(128, 116, 84, 0, 24, "US-East", "Ashburn"),
    CidrRegion::new(128, 116, 86, 0, 24, "US-East", "New York"),
    CidrRegion::new(128, 116, 87, 0, 24, "US-East", "Ashburn"),
    CidrRegion::new(128, 116, 95, 0, 24, "US-East", "Ashburn"),
    CidrRegion::new(128, 116, 105, 0, 24, "US-East", "Ashburn"),
    CidrRegion::new(128, 116, 111, 0, 24, "US-East", "Miami"),
    CidrRegion::new(128, 116, 115, 0, 24, "US-East", "Ashburn"),
    CidrRegion::new(128, 116, 116, 0, 24, "US-East", "New York"),
    CidrRegion::new(128, 116, 127, 0, 24, "US-East", "Ashburn"),
    // US-Central: Chicago
    CidrRegion::new(128, 116, 12, 0, 24, "US-Central", "Chicago"),
    CidrRegion::new(128, 116, 57, 0, 24, "US-Central", "Chicago"),
    CidrRegion::new(128, 116, 72, 0, 24, "US-Central", "Chicago"),
    CidrRegion::new(128, 116, 88, 0, 24, "US-Central", "Chicago"),
    CidrRegion::new(128, 116, 106, 0, 24, "US-Central", "Chicago"),
    CidrRegion::new(128, 116, 117, 0, 24, "US-Central", "Chicago"),
    // US-South: Dallas
    CidrRegion::new(128, 116, 13, 0, 24, "US-South", "Dallas"),
    CidrRegion::new(128, 116, 14, 0, 24, "US-South", "Dallas"),
    CidrRegion::new(128, 116, 58, 0, 24, "US-South", "Dallas"),
    CidrRegion::new(128, 116, 73, 0, 24, "US-South", "Dallas"),
    CidrRegion::new(128, 116, 89, 0, 24, "US-South", "Dallas"),
    CidrRegion::new(128, 116, 107, 0, 24, "US-South", "Dallas"),
    CidrRegion::new(128, 116, 118, 0, 24, "US-South", "Dallas"),
    // Europe: London, Frankfurt, Amsterdam, Paris
    CidrRegion::new(128, 116, 21, 0, 24, "EU-West", "Amsterdam"),
    CidrRegion::new(128, 116, 22, 0, 24, "EU-Central", "Frankfurt"),
    CidrRegion::new(128, 116, 23, 0, 24, "EU-West", "London"),
    CidrRegion::new(128, 116, 33, 0, 24, "EU-Central", "Frankfurt"),
    CidrRegion::new(128, 116, 35, 0, 24, "EU-West", "London"),
    CidrRegion::new(128, 116, 44, 0, 24, "EU-Central", "Frankfurt"),
    CidrRegion::new(128, 116, 66, 0, 24, "EU-West", "Paris"),
    CidrRegion::new(128, 116, 67, 0, 24, "EU-Central", "Frankfurt"),
    CidrRegion::new(128, 116, 77, 0, 24, "EU-West", "London"),
    CidrRegion::new(128, 116, 78, 0, 24, "EU-West", "Amsterdam"),
    CidrRegion::new(128, 116, 79, 0, 24, "EU-Central", "Frankfurt"),
    CidrRegion::new(128, 116, 100, 0, 24, "EU-Central", "Frankfurt"),
    CidrRegion::new(128, 116, 101, 0, 24, "EU-West", "London"),
    CidrRegion::new(128, 116, 102, 0, 24, "EU-West", "Amsterdam"),
    CidrRegion::new(128, 116, 103, 0, 24, "EU-West", "Paris"),
    CidrRegion::new(128, 116, 120, 0, 24, "EU-Central", "Frankfurt"),
    CidrRegion::new(128, 116, 121, 0, 24, "EU-West", "London"),
    // Asia-Pacific: Singapore, Tokyo, Sydney, Mumbai
    CidrRegion::new(128, 116, 48, 0, 24, "AP-South", "Singapore"),
    CidrRegion::new(128, 116, 49, 0, 24, "AP-Northeast", "Tokyo"),
    CidrRegion::new(128, 116, 50, 0, 24, "AP-South", "Singapore"),
    CidrRegion::new(128, 116, 51, 0, 24, "AP-South", "Singapore"),
    CidrRegion::new(128, 116, 52, 0, 24, "AP-Southeast", "Sydney"),
    CidrRegion::new(128, 116, 55, 0, 24, "AP-Northeast", "Tokyo"),
    CidrRegion::new(128, 116, 80, 0, 24, "AP-South", "Mumbai"),
    CidrRegion::new(128, 116, 81, 0, 24, "AP-Northeast", "Tokyo"),
    CidrRegion::new(128, 116, 82, 0, 24, "AP-South", "Singapore"),
    CidrRegion::new(128, 116, 83, 0, 24, "AP-Southeast", "Sydney"),
    CidrRegion::new(128, 116, 97, 0, 24, "AP-South", "Singapore"),
    CidrRegion::new(128, 116, 98, 0, 24, "AP-South", "Mumbai"),
    CidrRegion::new(128, 116, 99, 0, 24, "AP-Northeast", "Tokyo"),
    CidrRegion::new(128, 116, 122, 0, 24, "AP-South", "Singapore"),
    CidrRegion::new(128, 116, 123, 0, 24, "AP-Northeast", "Tokyo"),
    CidrRegion::new(128, 116, 124, 0, 24, "AP-Southeast", "Sydney"),
    CidrRegion::new(128, 116, 126, 0, 24, "AP-South", "Mumbai"),
    // South America: Sao Paulo
    CidrRegion::new(128, 116, 71, 0, 24, "SA-East", "Sao Paulo"),
    CidrRegion::new(128, 116, 104, 0, 24, "SA-East", "Sao Paulo"),
    CidrRegion::new(128, 116, 125, 0, 24, "SA-East", "Sao Paulo"),
];

/// Result of a local IPv4 server region lookup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegionMatch {
    pub region: Option<&'static str>,
    pub city: Option<&'static str>,
    pub label: String,
}

/// Map an IPv4 address locally to a Roblox datacenter region, or fall back to
/// `"Roblox Server (<ip>)"` if it lies outside known ranges.
pub fn lookup_region(ip: Ipv4Addr) -> RegionMatch {
    let bits = u32::from(ip);
    for entry in ROBLOX_CIDRS {
        if entry.matches(bits) {
            return RegionMatch {
                region: Some(entry.region),
                city: Some(entry.city),
                label: format!("{} \u{00b7} {}", entry.region, entry.city),
            };
        }
    }
    RegionMatch {
        region: None,
        city: None,
        label: format!("Roblox Server ({ip})"),
    }
}

/// Convenience lookup from an IP string (falls back cleanly on unparseable text).
pub fn lookup_region_str(ip: &str) -> String {
    match ip.trim().parse::<Ipv4Addr>() {
        Ok(v4) => lookup_region(v4).label,
        Err(_) => format!("Roblox Server ({})", ip.trim()),
    }
}

/// Parse a `Connection accepted from <ip>|<port>` or `<ip>:<port>` log line.
///
/// Verified against Roblox 2.745 logs:
/// `[DFLog::NetworkClient] Connection accepted from 128.116.14.33|59539`
/// Rejects private RFC1918 addresses (e.g. `10.x.x.x` internal RCC addresses).
pub fn parse_connection_accepted(line: &str) -> Option<(String, u16)> {
    let rest = line.split_once("Connection accepted from ")?.1.trim();
    let token = rest.split_whitespace().next()?;
    let (ip_str, port_str) = token.split_once('|').or_else(|| token.split_once(':'))?;
    let ip: Ipv4Addr = ip_str.parse().ok()?;
    if ip.is_private() || ip.is_loopback() || ip.is_unspecified() {
        return None;
    }
    let port: u16 = port_str.parse().ok()?;
    Some((ip.to_string(), port))
}

// ------------------------------------------------------------- live session

/// Live game-server session tracked from Roblox's log lines.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ServerSession {
    pub place_id: Option<u64>,
    pub job_id: Option<String>,
    pub server_ip: Option<String>,
    pub server_port: Option<u16>,
    pub region_label: Option<String>,
    pub joined_at: Option<u64>,
}

impl ServerSession {
    pub fn in_game(&self) -> bool {
        self.place_id.is_some()
    }

    /// Shareable web invite URL that launches straight into this server instance.
    pub fn web_invite_link(&self) -> Option<String> {
        let place_id = self.place_id?;
        Some(match &self.job_id {
            Some(job) if !job.is_empty() => {
                format!("https://www.roblox.com/games/start?placeId={place_id}&gameInstanceId={job}")
            }
            _ => format!("https://www.roblox.com/games/start?placeId={place_id}"),
        })
    }

    /// Direct `roblox://` deep link targeting this exact server instance.
    pub fn deep_link(&self) -> Option<String> {
        let place_id = self.place_id?;
        Some(match &self.job_id {
            Some(job) if !job.is_empty() => {
                format!("roblox://experiences/start?placeId={place_id}&gameInstanceId={job}")
            }
            _ => format!("roblox://experiences/start?placeId={place_id}"),
        })
    }

    /// Formatted multi-line diagnostic summary for "Copy Server Info".
    pub fn summary_text(&self) -> String {
        let mut lines = Vec::new();
        if let Some(region) = &self.region_label {
            lines.push(format!("Region: {region}"));
        }
        if let Some(place_id) = self.place_id {
            lines.push(format!("Place ID: {place_id}"));
        }
        if let Some(job_id) = &self.job_id {
            lines.push(format!("Job ID: {job_id}"));
        }
        if let Some(ip) = &self.server_ip {
            match self.server_port {
                Some(port) => lines.push(format!("Server: {ip}:{port}")),
                None => lines.push(format!("Server: {ip}")),
            }
        }
        if let Some(link) = self.web_invite_link() {
            lines.push(format!("Invite: {link}"));
        }
        lines.join("\n")
    }

    /// Convert to `HostWindow`'s headerbar pill & popover banner.
    pub fn to_banner(&self) -> Option<vanta_shell::host_window::ServerBanner> {
        let place_id = self.place_id?;
        let pill_label = self
            .region_label
            .clone()
            .unwrap_or_else(|| format!("Place {place_id}"));
        let detail_title = match (&self.region_label, &self.server_ip) {
            (Some(r), Some(ip)) => format!("{r}\nPlace {place_id} \u{00b7} {ip}"),
            (Some(r), None) => format!("{r}\nPlace {place_id}"),
            (None, Some(ip)) => format!("Place {place_id}\n{ip}"),
            (None, None) => format!("Place {place_id}"),
        };
        Some(vanta_shell::host_window::ServerBanner {
            pill_label,
            detail_title,
            invite_link: self.web_invite_link().unwrap_or_default(),
            server_info: self.summary_text(),
            can_rejoin: true,
        })
    }

    /// Convert to the `vanta-protocol` `ServerInfo` payload for `state.get`.
    pub fn to_protocol(&self) -> vanta_protocol::msg::ServerInfo {
        vanta_protocol::msg::ServerInfo {
            place_id: self.place_id,
            job_id: self.job_id.clone(),
            server_ip: self.server_ip.clone(),
            server_port: self.server_port,
            region_label: self.region_label.clone(),
            joined_at: self.joined_at,
        }
    }

    /// JSON payload for the `vanta/game.server` core event.
    pub fn to_core_event_payload(&self) -> serde_json::Value {
        if !self.in_game() {
            return serde_json::json!({});
        }
        serde_json::to_value(self.to_protocol()).unwrap_or_else(|_| serde_json::json!({}))
    }
}

static SESSION: Mutex<ServerSession> = Mutex::new(ServerSession {
    place_id: None,
    job_id: None,
    server_ip: None,
    server_port: None,
    region_label: None,
    joined_at: None,
});

/// Last `(place_id, server_ip)` pair we already sent a desktop notification for,
/// so both `UDMUX Address` and `Connection accepted` arriving 500 ms apart for
/// the same join only notify once.
static LAST_NOTIFIED: Mutex<Option<(Option<u64>, String)>> = Mutex::new(None);

/// Lock-free dirty flag checked on every Wayland pump tick.
static UI_DIRTY: AtomicBool = AtomicBool::new(false);

fn mark_ui_dirty() {
    UI_DIRTY.store(true, Ordering::Release);
}

/// Snapshot the current tracked server session.
pub fn current_session() -> ServerSession {
    SESSION.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Consume a pending UI update for `HostWindow::set_server_banner`.
///
/// Costs a single relaxed atomic load when nothing has changed.
pub fn take_ui_update() -> Option<Option<vanta_shell::host_window::ServerBanner>> {
    if !UI_DIRTY.swap(false, Ordering::AcqRel) {
        return None;
    }
    Some(current_session().to_banner())
}

// ------------------------------------------------------ notification setting

fn notify_cell() -> &'static AtomicBool {
    static ON: OnceLock<AtomicBool> = OnceLock::new();
    ON.get_or_init(|| {
        AtomicBool::new(notify_enabled_for(
            std::env::var("VANTA_SERVER_REGION_NOTIFY").ok().as_deref(),
        ))
    })
}

/// Pure env parser for `VANTA_SERVER_REGION_NOTIFY` (defaults ON).
pub fn notify_enabled_for(v: Option<&str>) -> bool {
    !matches!(
        v.map(|s| s.trim().to_ascii_lowercase()).as_deref(),
        Some("0") | Some("false") | Some("no") | Some("off")
    )
}

pub fn notify_enabled() -> bool {
    notify_cell().load(Ordering::Relaxed)
}

pub fn set_notify_enabled(on: bool) {
    notify_cell().store(on, Ordering::Release);
}

fn maybe_notify_join(session: &ServerSession) {
    if !notify_enabled() {
        return;
    }
    let Some(ip) = &session.server_ip else { return };
    let Some(region) = &session.region_label else { return };
    let key = (session.place_id, ip.clone());
    {
        let mut last = LAST_NOTIFIED.lock().unwrap_or_else(|e| e.into_inner());
        if last.as_ref() == Some(&key) {
            return;
        }
        *last = Some(key);
    }
    let summary = format!("Connected to {region}");
    let body = match session.place_id {
        Some(place_id) => format!("Place {place_id} \u{00b7} {ip}"),
        None => ip.clone(),
    };
    let _ = std::thread::Builder::new()
        .name("vanta-region-notify".into())
        .spawn(move || {
            let _ = vanta_plugins::notify::send(&summary, &body);
        });
}

// ------------------------------------------------------------ log event hooks

fn publish_session(session: &ServerSession) {
    mark_ui_dirty();
    if session.in_game() {
        crate::control::server_info(Some(session.to_protocol()));
    } else {
        crate::control::server_info(None);
    }
    crate::plugin_host::publish_core(
        vanta_plugins::core_events::GAME_SERVER,
        session.to_core_event_payload(),
    );
}

/// Called from `game_log::poll` on `Event::Joining`.
pub fn on_joining(place_id: u64, job_id: &str) {
    let snapshot = {
        let mut s = SESSION.lock().unwrap_or_else(|e| e.into_inner());
        if s.place_id != Some(place_id) {
            *s = ServerSession::default();
        }
        s.place_id = Some(place_id);
        s.job_id = Some(job_id.to_string());
        s.clone()
    };
    publish_session(&snapshot);
}

/// Called from `game_log::poll` on `Event::Joined`.
pub fn on_joined(place_id: u64, joined_at: Option<u64>) {
    let snapshot = {
        let mut s = SESSION.lock().unwrap_or_else(|e| e.into_inner());
        if s.place_id != Some(place_id) {
            s.job_id = None;
            s.server_ip = None;
            s.server_port = None;
            s.region_label = None;
        }
        s.place_id = Some(place_id);
        s.joined_at = joined_at;
        s.clone()
    };
    publish_session(&snapshot);
    maybe_notify_join(&snapshot);
}

/// Called from `game_log::poll` on `Event::Server`.
pub fn on_server(address: &str, port: u16) {
    let region = lookup_region_str(address);
    let snapshot = {
        let mut s = SESSION.lock().unwrap_or_else(|e| e.into_inner());
        s.server_ip = Some(address.to_string());
        s.server_port = Some(port);
        s.region_label = Some(region.clone());
        s.clone()
    };
    println!("[vanta] server region: {region} ({address}:{port})");
    publish_session(&snapshot);
    maybe_notify_join(&snapshot);
}

/// Called from `game_log::poll` on `Event::Left`.
pub fn on_left() {
    let snapshot = {
        let mut s = SESSION.lock().unwrap_or_else(|e| e.into_inner());
        *s = ServerSession::default();
        s.clone()
    };
    *LAST_NOTIFIED.lock().unwrap_or_else(|e| e.into_inner()) = None;
    publish_session(&snapshot);
}

/// Re-trigger a live join into the current server instance via Roblox's
/// `roblox://experiences/start?placeId=<id>&gameInstanceId=<job>` deep-link flow.
pub fn trigger_rejoin() -> Result<(), String> {
    let session = current_session();
    let Some(place_id) = session.place_id else {
        return Err("not currently in a game server".to_string());
    };
    let place_str = place_id.to_string();
    let launch = crate::deeplink::HybridLaunch {
        place_id: &place_str,
        instance_id: session.job_id.as_deref(),
        join_attempt_id: None,
        join_attempt_origin: Some("VantaQuickRejoin"),
        browser_tracker_id: None,
        access_code: None,
        link_code: None,
        reserved_server_access_code: None,
    };
    crate::deeplink::publish_hybrid_game_launch(&launch)
}

/// Serialise tests that mutate global server session or notify settings.
#[cfg(test)]
pub(crate) static TEST_GUARD: Mutex<()> = Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cidr_lookup_resolves_known_roblox_regions_and_falls_back_cleanly() {
        // Real server IP from Roblox 2.745 capture: 128.116.14.33
        assert_eq!(lookup_region_str("128.116.14.33"), "US-South \u{00b7} Dallas");
        // Real server IP from game_log.rs capture: 128.116.51.33
        assert_eq!(lookup_region_str("128.116.51.33"), "AP-South \u{00b7} Singapore");
        // Additional major datacenters across regions
        assert_eq!(lookup_region_str("128.116.4.12"), "US-East \u{00b7} Ashburn");
        assert_eq!(lookup_region_str("128.116.12.99"), "US-Central \u{00b7} Chicago");
        assert_eq!(lookup_region_str("128.116.0.42"), "US-West \u{00b7} San Jose");
        assert_eq!(lookup_region_str("128.116.23.5"), "EU-West \u{00b7} London");
        assert_eq!(lookup_region_str("128.116.22.5"), "EU-Central \u{00b7} Frankfurt");
        assert_eq!(lookup_region_str("128.116.21.5"), "EU-West \u{00b7} Amsterdam");
        assert_eq!(lookup_region_str("128.116.66.5"), "EU-West \u{00b7} Paris");
        assert_eq!(lookup_region_str("128.116.49.5"), "AP-Northeast \u{00b7} Tokyo");
        assert_eq!(lookup_region_str("128.116.52.5"), "AP-Southeast \u{00b7} Sydney");
        assert_eq!(lookup_region_str("128.116.80.5"), "AP-South \u{00b7} Mumbai");
        assert_eq!(lookup_region_str("128.116.71.5"), "SA-East \u{00b7} Sao Paulo");
        // Unknown IP falls back without any network call
        assert_eq!(lookup_region_str("203.0.113.42"), "Roblox Server (203.0.113.42)");
    }

    #[test]
    fn parses_real_roblox_2745_connection_accepted_lines() {
        // Verbatim from 2.745.0.1142_20261121T134537Z_Player_496c4_last.log
        let pipe_line = "2026-11-21T13:47:22.467Z,105.467804,a5fbc6c0,6,Info [DFLog::NetworkClient] \
                         Connection accepted from 128.116.14.33|59539";
        assert_eq!(
            parse_connection_accepted(pipe_line),
            Some(("128.116.14.33".to_string(), 59539))
        );

        // Colon-separated variant
        let colon_line = "[DFLog::NetworkClient] Connection accepted from 128.116.4.10:53000";
        assert_eq!(
            parse_connection_accepted(colon_line),
            Some(("128.116.4.10".to_string(), 53000))
        );

        // Private RCC IP is ignored
        let private_line = "[DFLog::NetworkClient] Connection accepted from 10.39.1.143|59539";
        assert_eq!(parse_connection_accepted(private_line), None);
    }

    #[test]
    fn invite_links_and_summary_format_correctly() {
        let s = ServerSession {
            place_id: Some(18879239467),
            job_id: Some("fc38a842-cf20-44d8-af8b-296541a60116".into()),
            server_ip: Some("128.116.14.33".into()),
            server_port: Some(59539),
            region_label: Some("US-South \u{00b7} Dallas".into()),
            joined_at: Some(1795268840),
        };
        assert_eq!(
            s.web_invite_link().as_deref(),
            Some("https://www.roblox.com/games/start?placeId=18879239467&gameInstanceId=fc38a842-cf20-44d8-af8b-296541a60116")
        );
        assert_eq!(
            s.deep_link().as_deref(),
            Some("roblox://experiences/start?placeId=18879239467&gameInstanceId=fc38a842-cf20-44d8-af8b-296541a60116")
        );
        let summary = s.summary_text();
        assert!(summary.contains("Region: US-South \u{00b7} Dallas"));
        assert!(summary.contains("Place ID: 18879239467"));
        assert!(summary.contains("Job ID: fc38a842-cf20-44d8-af8b-296541a60116"));
        assert!(summary.contains("Server: 128.116.14.33:59539"));

        // Without job_id, still builds a valid place-level link
        let no_job = ServerSession { place_id: Some(1818), ..Default::default() };
        assert_eq!(
            no_job.web_invite_link().as_deref(),
            Some("https://www.roblox.com/games/start?placeId=1818")
        );
        assert_eq!(
            no_job.deep_link().as_deref(),
            Some("roblox://experiences/start?placeId=1818")
        );
    }

    #[test]
    fn full_session_lifecycle_tracks_join_server_and_resets_on_leave() {
        let _g = TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        let prev_notify = notify_enabled();
        // Disable desktop portal notifications during unit test.
        set_notify_enabled(false);
        on_left();
        let _ = take_ui_update();

        on_joining(18879239467, "fc38a842-cf20-44d8-af8b-296541a60116");
        on_joined(18879239467, Some(1795268840));
        on_server("128.116.14.33", 59539);

        let cur = current_session();
        assert!(cur.in_game());
        assert_eq!(cur.place_id, Some(18879239467));
        assert_eq!(cur.job_id.as_deref(), Some("fc38a842-cf20-44d8-af8b-296541a60116"));
        assert_eq!(cur.server_ip.as_deref(), Some("128.116.14.33"));
        assert_eq!(cur.server_port, Some(59539));
        assert_eq!(cur.region_label.as_deref(), Some("US-South \u{00b7} Dallas"));

        let banner = take_ui_update().expect("dirty").expect("banner present");
        assert_eq!(banner.pill_label, "US-South \u{00b7} Dallas");
        assert!(banner.can_rejoin);
        assert!(take_ui_update().is_none(), "consumed once");

        on_left();
        assert!(!current_session().in_game());
        assert_eq!(current_session(), ServerSession::default());
        assert_eq!(take_ui_update(), Some(None), "leave hides banner");

        set_notify_enabled(prev_notify);
    }

    #[test]
    fn notify_env_defaults_on_and_respects_disable_words() {
        assert!(notify_enabled_for(None));
        assert!(notify_enabled_for(Some("1")));
        assert!(notify_enabled_for(Some("true")));
        assert!(!notify_enabled_for(Some("0")));
        assert!(!notify_enabled_for(Some("false")));
        assert!(!notify_enabled_for(Some("off")));
    }
}
