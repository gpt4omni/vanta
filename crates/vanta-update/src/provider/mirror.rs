//! Fetching the build from APKPure, a mirror that is not Roblox.
//!
//! [ADR-025](../../../docs/adr/ADR-025-fetching-from-a-third-party-mirror.md)
//! is the decision and it turns on one sentence:
//! [`crate::apk_signature`] verifies that Roblox's own signing certificate
//! signed these exact bytes, so a mirror cannot alter the file it serves
//! without the alteration being caught. That check is not a formality bolted on
//! afterwards -- **it is the entire reason this file is allowed to exist**, and
//! a caller that fetches through here and skips it has not saved a step, it has
//! removed the only thing that made the source acceptable.
//!
//! What the check does not cover is worth naming, because it is what the user
//! is actually deciding about when they choose this source. A mirror can be
//! down. A mirror can decline to serve a version, or serve an older one while
//! claiming it is current -- **nothing here verifies the version list**, only
//! the archives. And a mirror sees who asked and when. Those are the terms, and
//! they are why this is not downloaded from without being asked on a metered
//! connection, and why Sober's copy is offered as an import
//! ([`super::import`]) for anybody who prefers Google Play's route.
//!
//! ## The protocol has no specification, and this is a reader for it
//!
//! The endpoint answers with a length-delimited binary encoding that is
//! protocol-buffer-shaped and has no published schema, no `.proto`, and no
//! promise of stability. There is no way to parse it correctly in the sense
//! that word usually means; what there is, is a set of byte patterns that hold
//! on the responses this has been measured against. That makes the reader below
//! **a maintenance surface**, and the only thing that makes a maintenance
//! surface maintainable is a record of what it looked like when it worked.
//!
//! Measured on 2026-08-26 from this machine, `x-abis: x86_64`:
//!
//! ```text
//! HTTP/2 200, 99 073 bytes
//! 15 version markers, newest first
//! newest              2.734.917, version code 2908
//! its download        https://download.pureapk.com/b/APK/...  (a single APK)
//! second marker       2.734.916, code 2904
//! ```
//!
//! The engine already installed on that machine reads `2.734.0.917`, which is
//! the same build from a source that shares no code with this one. That
//! agreement is the control: a reader that mis-parsed the response would have
//! to mis-parse it into the right answer to pass it.
//!
//! ## The download, measured the same day
//!
//! `cargo run --release -p vanta-update --example fetch_probe -- --download`:
//!
//! ```text
//! candidate-0.apk   229 140 095 bytes, one monolithic APK
//! contents          3578 entries, 1835 under assets/,
//!                   lib/{arm64-v8a,armeabi-v7a,x86_64}/libroblox.so
//! verified          44932ea35a17a267372d71b54d1a0cb3da0dca5113e94406ae2fe18090ba1477
//! ```
//!
//! **That fingerprint is the same one the archive Sober downloaded from Google
//! Play carries**, and the two files are not the same file: 229 MB of every
//! ABI against 97 MB of assets plus a 53 MB x86-64 split. Two distribution
//! routes that share nothing, one signing certificate, and this verifier saying
//! so about both.
//!
//! That is the strongest statement available about whether the mirror is
//! serving Roblox's build, and it is the measurement to repeat when anyone asks
//! whether this source is safe. It is also, precisely, the thing ADR-025 said
//! had to be true.
//!
//! ## Three field tags do all the work
//!
//! Naming them makes the scan below legible rather than magical.
//!
//! | Byte | Protobuf meaning |
//! |---|---|
//! | `0x2a` | field 5, length-delimited -- the version code, ASCII decimal |
//! | `0x32` | field 6 -- the version name, ASCII |
//! | `0x3a` | field 7 -- whatever follows the name; its tag is the ASCII colon the scan anchors on |
//! | `0x4a` | field 9 -- a download URL |
//!
//! Lengths are single-byte varints except in the URLs, which is why every
//! length check here is against a value under 128 and the URL length is two
//! bytes.
//!
//! ## Nothing downstream trusts this about architecture
//!
//! The ABI filter is a hint to the service and not a fact about what arrives.
//! The archive is opened and [`crate::apk::LIBRARY_IN_APK`] (the host's own
//! library path -- `lib/x86_64/libroblox.so` on an x86-64 build,
//! `lib/arm64-v8a/libroblox.so` on aarch64) is looked for directly, which is
//! why the broad-filter retry is safe: widening it buys availability and
//! cannot buy a wrong answer.

use super::{Archives, Available, Progress, Provider};
use crate::url_policy;
use crate::Unreachable;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Where the version list is asked for, and which hosts may serve the bytes.
///
/// **The base URL is configurable and the protocol is not**, and that split is
/// the honest version of "an APKPure-compatible endpoint". The response is an
/// undocumented, length-delimited binary blob with no schema and no `.proto`,
/// read by the byte patterns below. Anything answering here has to produce that
/// exact shape. So this is the same bargain as pointing an OpenAI client at a
/// different base URL -- the wire format is fixed and the host is yours, be it
/// a mirror, a caching proxy in front of APKPure, or a copy inside a company
/// network -- and it is not a plugin interface for arbitrary services.
///
/// Saying that plainly matters more than it looks. A setting called "mirror
/// URL" invites somebody to paste an F-Droid or Aptoide address, get a reader
/// that finds no version markers, and read the failure as Vanta being broken.
/// The refusal names the shape for that reason.
///
/// ## Why this is allowed to widen the allow-list
///
/// [`crate::url_policy`] exists to stop the *mirror* sending Vanta somewhere
/// of its own choosing, and it still does: every redirect is re-checked and an
/// unlisted target is refused. What changes is who writes the list, and that is
/// now the user -- who could replace the binary anyway, and who is the one
/// person entitled to decide which third party they talk to.
///
/// **The property that actually protects them is untouched.** Whatever host
/// serves the archive, it installs only if Roblox's own signing certificate
/// signed those exact bytes. A hostile base URL can waste bandwidth, watch who
/// asked, and serve nothing installable. It cannot serve a modified Roblox.
/// That is why a mirror was acceptable at all, and it does not care which one.
#[derive(Debug, Clone)]
pub struct Mirror {
    pub metadata_url: String,
    pub download_hosts: Vec<String>,
    pub headers: Vec<(String, String)>,
}

/// The endpoint measured on 2026-08-26; see the module header for the reading.
const DEFAULT_METADATA: &str =
    "https://api.pureapk.com/m/v3/cms/app_version?hl=en-US&package_name=com.roblox.client";

/// APKPure's own names, plus the CDN it actually serves bytes from.
///
/// `winudf.com` looks out of place beside the other two and is where the
/// download lands, so a list without it refuses every real transfer. It is a
/// third organisation to trust with the transport, and naming it rather than
/// quietly widening the rule is the honest version of that.
const DEFAULT_DOWNLOAD_HOSTS: [&str; 3] = ["pureapk.com", "apkpure.com", "winudf.com"];

/// The APKPure client version code the service keys its response shape on.
///
/// **This is the constant most likely to age out.** When the reader below stops
/// finding markers in a response that is otherwise a healthy 200, this is the
/// first thing to look at.
const X_CV: &str = "3172501";
/// Android 10, the SDK level claimed. Bounds which builds are offered.
const X_SV: &str = "29";
/// An opaque flag the client sends. It must be present; its meaning is not
/// known here and guessing at one would be worse than saying so.
const X_GP: &str = "1";

/// Point Vanta at a different endpoint of the same shape.
///
/// One variable and one URL is the whole of the common case, deliberately --
/// a caching proxy in front of APKPure needs nothing else.
pub const MIRROR_URL_ENV: &str = "VANTA_MIRROR_URL";

/// Hosts that may serve the archives, comma separated, when the endpoint above
/// hands back URLs on names it does not itself use.
pub const MIRROR_HOSTS_ENV: &str = "VANTA_MIRROR_DOWNLOAD_HOSTS";

impl Default for Mirror {
    fn default() -> Self {
        Mirror {
            metadata_url: DEFAULT_METADATA.to_string(),
            download_hosts: DEFAULT_DOWNLOAD_HOSTS.iter().map(|s| s.to_string()).collect(),
            headers: vec![
                ("x-cv".into(), X_CV.into()),
                ("x-sv".into(), X_SV.into()),
                ("x-gp".into(), X_GP.into()),
            ],
        }
    }
}

impl Mirror {
    /// The configured mirror, or the default one.
    ///
    /// A custom URL with no host list gets **its own host and subdomains, and
    /// nothing else**. That is the conservative reading of what somebody meant
    /// by naming one endpoint, and it is chosen rather than inherited: keeping
    /// APKPure's three CDN names against a URL that has nothing to do with
    /// APKPure would leave three hosts trusted for a reason nobody asked for.
    pub fn configured() -> Result<Self, Unreachable> {
        let Some(url) = std::env::var(MIRROR_URL_ENV).ok().filter(|s| !s.trim().is_empty())
        else {
            return Ok(Mirror::default());
        };
        let url = url.trim().to_string();

        // Checked when it is read, not when it is used, so a typo fails saying
        // which variable was wrong instead of as a transport error four steps
        // later. The host is taken from the URL itself, so this cannot refuse
        // the very endpoint it was just given.
        let host = url_policy::host_of(&url)?;
        url_policy::check(&url, &url_policy::Allowed::exactly(host.clone())).map_err(|e| {
            Unreachable::Malformed {
                url: url.clone(),
                why: format!("{MIRROR_URL_ENV} is not a URL Vanta can fetch from: {e}"),
            }
        })?;

        let hosts = match std::env::var(MIRROR_HOSTS_ENV).ok().filter(|s| !s.trim().is_empty()) {
            Some(list) => list
                .split(',')
                .map(|h| h.trim().to_ascii_lowercase())
                .filter(|h| !h.is_empty())
                .collect(),
            None => vec![host],
        };

        Ok(Mirror { metadata_url: url, download_hosts: hosts, ..Mirror::default() })
    }

    fn metadata_allowed(&self) -> Result<url_policy::Allowed, Unreachable> {
        Ok(url_policy::Allowed::exactly(url_policy::host_of(&self.metadata_url)?))
    }

    fn download_allowed(&self) -> url_policy::Allowed {
        url_policy::Allowed::any_of(self.download_hosts.clone())
    }

    /// The metadata URL with the ABI filter this request wants.
    fn headers_with(&self, abi: &str) -> Vec<(String, String)> {
        let mut h = self.headers.clone();
        h.push(("x-abis".into(), abi.into()));
        h
    }
}

/// The primary `x-abis` filter sent to APKPure for this host architecture.
///
/// On x86-64 APKPure serves a monolithic all-ABI APK for `x-abis: x86_64`.
/// On aarch64 APKPure serves `.xapk` split bundles (`com.roblox.client.apk` +
/// `config.arm64_v8a.apk`) for `x-abis: arm64-v8a`, which makes ARM64-only
/// Roblox releases visible and installable on ARM64 hosts. If an endpoint
/// omits a release under `arm64-v8a` or serves an unusable bundle, aarch64
/// falls back to [`ABI_MONOLITHIC`] (`"x86_64"`) and [`ABI_BROAD`].
#[cfg(target_arch = "x86_64")]
const ABI_EXACT: &str = "x86_64";
#[cfg(target_arch = "aarch64")]
const ABI_EXACT: &str = "arm64-v8a";

/// The filter that selects APKPure's monolithic all-ABI APK (which also
/// carries `lib/arm64-v8a/libroblox.so`), used as an aarch64 fallback when
/// `x-abis: arm64-v8a` returns no usable bundle.
const ABI_MONOLITHIC: &str = "x86_64";

/// Every ABI, for the retry. The filtered index is not reliably complete for
/// older versions -- APKPure sometimes omits a bundle that does contain the
/// matching split -- so a download that cannot find its version in the narrow
/// list asks again without one. Left spelled out rather than built from
/// `ABI_EXACT`: this list is APKPure's own vocabulary of Android ABI names, not
/// Vanta's, and only ever needs `arm64-v8a` and `x86_64` added to as Android
/// itself grows a new ABI.
const ABI_BROAD: &str = "arm64-v8a,armeabi-v7a,armeabi,x86,x86_64";

/// Metadata is small. Anything larger than this is not a version list, and
/// reading it into memory would be this fetcher's own denial of service.
const MAX_METADATA: u64 = 4 * 1024 * 1024;
/// One archive. Roblox's is about 150 MB across two files; a gigabyte is
/// generous and still bounded.
const MAX_ARCHIVE: u64 = 1024 * 1024 * 1024;
/// Bounds on an outer `.xapk` split-bundle wrapper (which typically holds
/// `manifest.json`, `icon.png`, `com.roblox.client.apk`, and a handful of
/// `config.*.apk` splits).
const MAX_XAPK_ENTRIES: usize = 256;
const MAX_XAPK_TOTAL_BYTES: u64 = MAX_ARCHIVE * 2;
const MAX_XAPK_MANIFEST_BYTES: u64 = 256 * 1024;

/// More than this many candidate archives for one version means the response
/// was not understood, and downloading them all to find out would cost
/// gigabytes to answer a question the first one usually answers.
const MAX_CANDIDATES: usize = 4;

const CONNECT: Duration = Duration::from_secs(10);
const METADATA_TRANSFER: Duration = Duration::from_secs(60);
const ARCHIVE_TRANSFER: Duration = Duration::from_secs(900);

#[derive(Debug, Default)]
pub struct ApkPure;

/// Bytes that may appear in a version name.
fn version_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'.' | b'+' | b'_' | b'-')
}

/// Bytes that may appear in a URL, per the shape these URLs actually take.
fn url_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b"-@:%._+~#=?&/()".contains(&b)
}

/// Where a version name sits in the response.
#[derive(Debug, Clone)]
struct Marker {
    start: usize,
    /// The offset of the `0x3a` that terminates the name.
    colon: usize,
    name: String,
}

/// Every version name in the response, in file order.
///
/// A marker is a byte run that begins with a digit, is not preceded by another
/// name byte -- so a name is not found inside a longer token -- contains a dot,
/// and is immediately followed by `0x3a`. The four conditions together are what
/// separates a version from every other run of digits in a 99 KB binary blob.
fn markers(d: &[u8]) -> Vec<Marker> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < d.len() {
        if d[i].is_ascii_digit() && (i == 0 || !version_byte(d[i - 1])) {
            let mut j = i;
            while j < d.len() && version_byte(d[j]) {
                j += 1;
            }
            let run = &d[i..j];
            if run.contains(&b'.') && j < d.len() && d[j] == 0x3a {
                if let Ok(name) = std::str::from_utf8(run) {
                    out.push(Marker { start: i, colon: j, name: name.to_string() });
                }
                i = j;
                continue;
            }
        }
        i += 1;
    }
    out
}

/// The version code belonging to `marker`, recovered by scanning backwards.
///
/// The code sits in the field immediately before the name, and the five
/// conditions below are what make "immediately" checkable rather than assumed:
/// the last of them requires the name field's value to begin exactly where the
/// marker begins, with nothing in between. Without it this would happily attach
/// the code of one record to the name of another.
fn code_before(d: &[u8], marker: &Marker) -> Option<u64> {
    let name_len = marker.colon - marker.start;
    if name_len > u8::MAX as usize {
        return None;
    }
    let floor = marker.start.saturating_sub(128);
    for p in (floor..marker.start).rev() {
        if d[p] != 0x2a {
            continue;
        }
        let len = *d.get(p + 1)? as usize;
        if !(1..=20).contains(&len) {
            continue;
        }
        let digits = d.get(p + 2..p + 2 + len)?;
        if !digits.iter().all(u8::is_ascii_digit) {
            continue;
        }
        if *d.get(p + 2 + len)? != 0x32 {
            continue;
        }
        if *d.get(p + 3 + len)? as usize != name_len {
            continue;
        }
        if p + 4 + len != marker.start {
            continue;
        }
        let mut code: u64 = 0;
        for b in digits {
            code = code.checked_mul(10)?.checked_add((b - b'0') as u64)?;
        }
        return (code != 0).then_some(code);
    }
    None
}

/// The download URLs inside one record.
///
/// `APKJ` and `XAPKJ` are the artefact kind followed by the `0x4a` tag; the URL
/// starts two bytes later because the length is a two-byte varint, these URLs
/// being well over 127 bytes.
fn urls_in(record: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    let mut at = 0usize;
    while at + 4 <= record.len() {
        let Some(found) = record[at..].windows(4).position(|w| w == b"APKJ") else { break };
        let x = at + found;
        let begin = x + 4 + 2;
        let mut j = begin;
        while j < record.len() && url_byte(record[j]) {
            j += 1;
        }
        if let Ok(url) = std::str::from_utf8(&record[begin..j]) {
            if url.starts_with("https://") {
                out.push(url.to_string());
            }
        }
        at = x + 4;
    }
    out
}

/// Ask the service for the version list, with `abi` as the filter.
fn metadata(mirror: &Mirror, abi: &str) -> Result<Vec<u8>, Unreachable> {
    let agent = url_policy::agent(CONNECT, METADATA_TRANSFER);
    let owned = mirror.headers_with(abi);
    let headers: Vec<(&str, &str)> =
        owned.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let (url, mut response) =
        url_policy::walk(&agent, &mirror.metadata_url, &mirror.metadata_allowed()?, &headers)?;

    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        let body = response
            .body_mut()
            .with_config()
            .limit(64 * 1024)
            .read_to_string()
            .unwrap_or_default();
        return Err(Unreachable::Status { url, status, body });
    }

    let mut bytes = Vec::new();
    response
        .body_mut()
        .as_reader()
        .take(MAX_METADATA)
        .read_to_end(&mut bytes)
        .map_err(|e| Unreachable::Transport { url: url.clone(), why: e.to_string() })?;
    if bytes.len() as u64 >= MAX_METADATA {
        return Err(Unreachable::Malformed {
            url,
            why: "the version list exceeds its size limit, so it is not a version list".into(),
        });
    }
    Ok(bytes)
}

/// Query metadata for the host's primary ABI, falling back to the monolithic
/// `x86_64` index on aarch64 when `arm64-v8a` yields no readable versions.
fn metadata_for_host(mirror: &Mirror) -> Result<Vec<u8>, Unreachable> {
    let body = metadata(mirror, ABI_EXACT)?;
    if ABI_EXACT != ABI_MONOLITHIC && versions_in(&body).is_empty() {
        if let Ok(fallback) = metadata(mirror, ABI_MONOLITHIC) {
            if !versions_in(&fallback).is_empty() {
                return Ok(fallback);
            }
        }
    }
    Ok(body)
}

/// Every version the mirror lists for this host architecture, newest first.
///
/// For the Version page, which asks when it is shown and never at startup. The
/// narrow filter only, for the reason `newest` gives: where a version listed
/// only under the broad filter was checked on x86-64, it had no x86-64 engine
/// in it.
pub fn offered() -> Result<Vec<Available>, Unreachable> {
    let mirror = Mirror::configured()?;
    let body = metadata_for_host(&mirror)?;
    let found = versions_in(&body);
    if found.is_empty() {
        return Err(Unreachable::Malformed {
            url: mirror.metadata_url.clone(),
            why: "the version list carries no versions Vanta can read".into(),
        });
    }
    Ok(found)
}

/// The versions in a response, once each, ordered by version code.
///
/// A name with no code beside it is left out rather than listed with a zero:
/// `code_before`'s adjacency check is what says a dotted number is a record's
/// version and not something that happens to precede the tag. So is a name
/// that is not digits and dots, because it becomes a directory name if it is
/// downloaded.
fn versions_in(d: &[u8]) -> Vec<Available> {
    let mut out: Vec<Available> = Vec::new();
    for m in markers(d) {
        if out.iter().any(|a| a.name == m.name) || !crate::store::is_valid_version(&m.name) {
            continue;
        }
        if let Some(code) = code_before(d, &m) {
            out.push(Available { name: m.name.clone(), code });
        }
    }
    out.sort_by(|a, b| b.code.cmp(&a.code));
    out
}

/// The download URLs the response offers for exactly `version`.
fn downloads_for(d: &[u8], version: &str) -> Vec<String> {
    let marks = markers(d);
    let mut out: Vec<String> = Vec::new();
    for (i, m) in marks.iter().enumerate() {
        if m.name != version {
            continue;
        }
        let end = marks.get(i + 1).map(|n| n.start).unwrap_or(d.len());
        // One URL per record: the first is the artefact, the rest are the same
        // file behind other names.
        if let Some(url) = urls_in(&d[m.colon + 1..end]).into_iter().next() {
            if !out.contains(&url) {
                out.push(url);
            }
        }
    }
    out
}

/// Stream one URL to `dest`, enforcing the ceiling on bytes that arrive.
///
/// **The ceiling is applied to what arrives, not to `Content-Length`.** A
/// declared length is a claim by the same party supplying the bytes; checking
/// it is a cheap early refusal and must never be the only check.
fn download(
    mirror: &Mirror,
    url: &str,
    cancel: &super::Cancel,
    dest: &Path,
    progress: &mut dyn FnMut(Progress),
) -> Result<(), Unreachable> {
    let agent = url_policy::agent(CONNECT, ARCHIVE_TRANSFER);
    let (final_url, mut response) =
        url_policy::walk(&agent, url, &mirror.download_allowed(), &[])?;

    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        return Err(Unreachable::Status { url: final_url, status, body: String::new() });
    }

    let total = response
        .headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok());
    if let Some(t) = total {
        if t > MAX_ARCHIVE {
            return Err(Unreachable::Malformed {
                url: final_url,
                why: format!("the archive declares {t} bytes, which exceeds its size limit"),
            });
        }
    }

    let name = dest.file_name().unwrap_or_default().to_string_lossy().to_string();
    let temporary = dest.with_extension("partial");
    let outcome = (|| -> Result<(), Unreachable> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&temporary)
            .map_err(|e| Unreachable::NoSource { why: e.to_string() })?;

        let mut reader = response.body_mut().as_reader();
        let mut buf = vec![0u8; 256 * 1024];
        let mut done: u64 = 0;
        let mut announced: u64 = 0;
        loop {
            let n = reader
                .read(&mut buf)
                .map_err(|e| Unreachable::Transport { url: final_url.clone(), why: e.to_string() })?;
            if n == 0 {
                break;
            }
            done += n as u64;
            // **Between chunks, which is the only place it can be.** A read
            // already in flight is not interruptible, so the granularity here
            // is one 256 KiB read -- imperceptible next to the transfer, and
            // the error unwinds through the same path a failure does, so the
            // partial file is removed rather than left to be mistaken for a
            // resumable download.
            cancel.check()?;
            if done > MAX_ARCHIVE {
                return Err(Unreachable::Malformed {
                    url: final_url.clone(),
                    why: "the archive exceeds its size limit".into(),
                });
            }
            file.write_all(&buf[..n])
                .map_err(|e| Unreachable::NoSource { why: e.to_string() })?;
            // Every megabyte, not every 64. The coarse interval was chosen
            // when the only consumer was a log line; a progress bar that moves
            // four times during a 229 MB download is not a progress bar. The
            // callback is a label update, so the cost of the finer interval is
            // nothing next to the transfer it is reporting.
            if done - announced >= 1024 * 1024 {
                announced = done;
                progress(Progress::Fetching { file: name.clone(), done, total });
            }
        }
        if done == 0 {
            return Err(Unreachable::Malformed {
                url: final_url.clone(),
                why: "the download is empty".into(),
            });
        }
        file.sync_all().map_err(|e| Unreachable::NoSource { why: e.to_string() })?;
        drop(file);

        // A ZIP local header, checked before anything downstream opens it. An
        // HTML error page served with a 200 is caught here rather than three
        // steps later inside a zip reader, where it reads as a corrupt archive.
        let mut head = [0u8; 4];
        let mut f = std::fs::File::open(&temporary)
            .map_err(|e| Unreachable::NoSource { why: e.to_string() })?;
        f.read_exact(&mut head).map_err(|e| Unreachable::NoSource { why: e.to_string() })?;
        if head[0] != b'P'
            || head[1] != b'K'
            || !matches!(head[2], 0x03 | 0x05 | 0x07)
            || !matches!(head[3], 0x04 | 0x06 | 0x08)
        {
            return Err(Unreachable::Malformed {
                url: final_url.clone(),
                why: "what arrived is not a ZIP archive, so it is not an APK".into(),
            });
        }

        std::fs::rename(&temporary, dest)
            .map_err(|e| Unreachable::NoSource { why: e.to_string() })?;
        progress(Progress::Fetching { file: name.clone(), done, total });
        Ok(())
    })();

    if outcome.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    outcome
}

impl Provider for ApkPure {
    fn name(&self) -> &'static str {
        "apkpure"
    }

    fn needs_network(&self) -> bool {
        true
    }

    /// **No retry on the broad filter here, deliberately.** Asking what the
    /// newest version is must be asked about builds Vanta can run; widening
    /// the filter would let an ARM-only release become "the newest version",
    /// and every step after this would then be working towards a build that
    /// cannot start.
    ///
    /// That was written as a hypothetical and it is now a measurement. On
    /// 2026-08-26 the narrow filter answered 2.734.917 while the broad one
    /// answered 2.735.1138, which looked like the filter hiding a newer build.
    /// Reading both of 2.735.1138's XAPK bundles -- by their ZIP central
    /// directories over range requests, 600 kB rather than 276 MB -- shows
    /// `config.armeabi_v7a.apk` in one and `config.arm64_v8a.apk` in the other
    /// and **no `config.x86_64.apk` in either**. The narrow filter was right
    /// and a retry here would have chased a build with nothing in it Vanta
    /// can execute. `docs/analysis/apk-mirrors.md` has the entry lists.
    fn newest(&self, progress: &mut dyn FnMut(Progress)) -> Result<Available, Unreachable> {
        progress(Progress::Asking { provider: self.name() });
        let mirror = Mirror::configured()?;
        let body = metadata_for_host(&mirror)?;
        let marks = markers(&body);
        let first = marks.first().ok_or_else(|| Unreachable::Malformed {
            url: mirror.metadata_url.clone(),
            why: "the version list carries no versions. If this endpoint was set with \
                  VANTA_MIRROR_URL it is answering in a shape this reader does not \
                  understand; otherwise the service has changed."
                .into(),
        })?;
        let code = code_before(&body, first).ok_or_else(|| Unreachable::Malformed {
            url: mirror.metadata_url.clone(),
            why: format!(
                "the version list names {} and carries no version code for it, so its shape \
                 has changed",
                first.name
            ),
        })?;
        Ok(Available { name: first.name.clone(), code })
    }

    fn fetch(
        &self,
        version: &Available,
        cancel: &super::Cancel,
        into: &Path,
        progress: &mut dyn FnMut(Progress),
    ) -> Result<Archives, Unreachable> {
        if into.read_dir().map(|mut d| d.next().is_some()).unwrap_or(false) {
            return Err(Unreachable::NoSource {
                why: format!(
                    "{} already holds files, and a download writes into an empty directory \
                     so a previous attempt's leftovers cannot be mistaken for this one's",
                    into.display()
                ),
            });
        }

        // The narrow filter, then (on aarch64) the monolithic x86_64 index,
        // then the broad filter. See ABI_BROAD: this buys availability for
        // older versions and cannot buy a wrong answer, because what arrives
        // is opened, unpacked if it is an XAPK, checked for the host's engine,
        // and signature-verified downstream before anything is installed.
        let mirror = Mirror::configured()?;
        let mut urls = downloads_for(&metadata(&mirror, ABI_EXACT)?, &version.name);
        if urls.is_empty() && ABI_EXACT != ABI_MONOLITHIC {
            urls = downloads_for(&metadata(&mirror, ABI_MONOLITHIC)?, &version.name);
        }
        if urls.is_empty() {
            urls = downloads_for(&metadata(&mirror, ABI_BROAD)?, &version.name);
        }
        if urls.is_empty() {
            return Err(Unreachable::Malformed {
                url: mirror.metadata_url.clone(),
                why: format!(
                    "the mirror does not offer version {}. It is reachable and it does not \
                     have that build.",
                    version.name
                ),
            });
        }
        if urls.len() > MAX_CANDIDATES {
            return Err(Unreachable::Malformed {
                url: mirror.metadata_url.clone(),
                why: format!(
                    "the mirror offers {} different archives for version {}, which is more \
                     than Vanta will try",
                    urls.len(),
                    version.name
                ),
            });
        }
        let allowed = mirror.download_allowed();
        for url in &urls {
            url_policy::check(url, &allowed)?;
        }

        let mut landed: Vec<PathBuf> = Vec::new();
        let mut tried_urls: Vec<String> = Vec::new();
        let mut next_idx: usize = 0;

        let fetch_batch = |batch: &[String],
                           landed: &mut Vec<PathBuf>,
                           tried: &mut Vec<String>,
                           next_idx: &mut usize,
                           progress: &mut dyn FnMut(Progress)|
         -> Result<(), Unreachable> {
            for url in batch {
                if tried.contains(url) {
                    continue;
                }
                tried.push(url.clone());
                let idx = *next_idx;
                *next_idx += 1;
                let dest = into.join(format!("candidate-{idx}.apk"));
                download(&mirror, url, cancel, &dest, progress)?;
                if let Some(inner) = unpack_xapk(&dest, into, &format!("candidate-{idx}"), cancel)? {
                    // The outer XAPK wrapper is no longer needed once its
                    // relevant inner APKs have been extracted; remove it now so
                    // staging disk usage stays bounded.
                    let _ = std::fs::remove_file(&dest);
                    landed.extend(inner);
                } else {
                    landed.push(dest);
                }
                // **Stop as soon as what has landed is enough.** The measured
                // ordinary case is either one monolithic APK or one matching
                // XAPK carrying both the engine and the assets; downloading
                // the remaining candidates after both halves are on disk would
                // double or quadruple the transfer for nothing.
                let (engine, assets) = held(landed);
                if engine.is_some() && assets.is_some() {
                    break;
                }
            }
            Ok(())
        };

        let result = (|| -> Result<(), Unreachable> {
            fetch_batch(&urls, &mut landed, &mut tried_urls, &mut next_idx, progress)?;
            if classify(&landed).is_none() && ABI_EXACT != ABI_MONOLITHIC {
                // On aarch64, if the arm64-v8a XAPK candidates did not yield a
                // usable engine (for example if the mirror returned an
                // armeabi-v7a-only XAPK), fall back to the monolithic bundle.
                if let Ok(body) = metadata(&mirror, ABI_MONOLITHIC) {
                    let fallback_urls = downloads_for(&body, &version.name);
                    if !fallback_urls.is_empty() && fallback_urls.len() <= MAX_CANDIDATES {
                        for url in &fallback_urls {
                            url_policy::check(url, &allowed)?;
                        }
                        fetch_batch(
                            &fallback_urls,
                            &mut landed,
                            &mut tried_urls,
                            &mut next_idx,
                            progress,
                        )?;
                    }
                }
            }
            Ok(())
        })();
        if let Err(e) = result {
            // No partial candidate set. There is no such thing as most of a
            // build, and leftovers a later run mistakes for a complete download
            // are how assets from one release end up beside an engine from
            // another.
            for p in &landed {
                let _ = std::fs::remove_file(p);
            }
            return Err(e);
        }

        let Some(archives) = classify(&landed) else {
            for p in &landed {
                let _ = std::fs::remove_file(p);
            }
            return Err(Unreachable::Malformed {
                url: mirror.metadata_url.clone(),
                why: format!(
                    "the mirror served {} archive(s) for version {} and none of them carries \
                     {}, so there is no engine in what arrived",
                    landed.len(),
                    version.name,
                    crate::apk::LIBRARY_IN_APK
                ),
            });
        };
        Ok(archives)
    }
}

/// Whether `path` starts with a ZIP local-file / empty-archive / spanned-archive
/// magic signature (`PK\x03\x04`, `PK\x05\x06`, or `PK\x07\x08`).
fn has_zip_magic(path: &Path) -> bool {
    let Ok(mut f) = std::fs::File::open(path) else { return false };
    let mut head = [0u8; 4];
    if f.read_exact(&mut head).is_err() {
        return false;
    }
    head[0] == b'P'
        && head[1] == b'K'
        && matches!(head[2], 0x03 | 0x05 | 0x07)
        && matches!(head[3], 0x04 | 0x06 | 0x08)
}

/// Inspect a single APK ZIP for the host engine and for `assets/`.
fn inspect_apk_contents(path: &Path) -> (bool, bool) {
    let Ok(file) = std::fs::File::open(path) else { return (false, false) };
    let Ok(mut zip) = zip::ZipArchive::new(std::io::BufReader::new(file)) else {
        return (false, false);
    };
    let has_engine = zip.by_name(crate::apk::LIBRARY_IN_APK).is_ok();
    let has_assets = (0..zip.len())
        .any(|i| zip.by_index(i).map(|e| e.name().starts_with("assets/")).unwrap_or(false));
    (has_engine, has_assets)
}

/// Prioritised inner `.apk` entry names inside an XAPK bundle.
///
/// Reads `manifest.json` when present (APKPure's `split_apks` array) and
/// supplements it with any `.apk` entries found in the ZIP central directory.
/// Entries likely to hold the base APK or the host's ABI split come first so
/// we can stop extracting as soon as both halves are found and never waste
/// disk I/O on unrelated DPI/locale or foreign-ABI splits.
fn xapk_apk_entries(
    zip: &mut zip::ZipArchive<std::io::BufReader<std::fs::File>>,
) -> Vec<String> {
    let mut manifest_base: Vec<String> = Vec::new();
    let mut manifest_all: Vec<String> = Vec::new();
    if let Ok(entry) = zip.by_name("manifest.json") {
        let mut raw = Vec::new();
        if entry.take(MAX_XAPK_MANIFEST_BYTES + 1).read_to_end(&mut raw).is_ok()
            && (raw.len() as u64) <= MAX_XAPK_MANIFEST_BYTES
        {
            if let Ok(val) = serde_json::from_slice::<serde_json::Value>(&raw) {
                if let Some(splits) = val.get("split_apks").and_then(|v| v.as_array()) {
                    for s in splits {
                        let Some(file) = s.get("file").and_then(|v| v.as_str()) else {
                            continue;
                        };
                        if !file.to_ascii_lowercase().ends_with(".apk") {
                            continue;
                        }
                        if s.get("id").and_then(|v| v.as_str()) == Some("base") {
                            manifest_base.push(file.to_string());
                        }
                        if !manifest_all.contains(&file.to_string()) {
                            manifest_all.push(file.to_string());
                        }
                    }
                }
            }
        }
    }

    let mut all: Vec<String> = Vec::new();
    let mut all_in_zip: Vec<String> = Vec::new();
    for i in 0..zip.len() {
        if let Ok(entry) = zip.by_index(i) {
            let name = entry.name().to_string();
            if name.to_ascii_lowercase().ends_with(".apk") {
                all_in_zip.push(name);
            }
        }
    }
    // Keep only manifest entries that actually exist in the ZIP, then append
    // any remaining `.apk` entries from the central directory.
    for name in manifest_all {
        if all_in_zip.contains(&name) && !all.contains(&name) {
            all.push(name);
        }
    }
    for name in all_in_zip {
        if !all.contains(&name) {
            all.push(name);
        }
    }

    let host_abi_underscore = crate::apk::HOST_ABI.replace('-', "_");
    let host_abi_hyphen = crate::apk::HOST_ABI;
    let rank = |name: &str| -> u8 {
        let lower = name.to_ascii_lowercase();
        let is_base = manifest_base.iter().any(|b| b == name)
            || lower == "base.apk"
            || lower.ends_with("/base.apk")
            || lower == "com.roblox.client.apk"
            || lower.ends_with("/com.roblox.client.apk");
        let is_host_abi =
            lower.contains(&host_abi_underscore) || lower.contains(host_abi_hyphen);
        if is_host_abi || is_base {
            return 0;
        }
        let is_foreign_abi = lower.contains("armeabi")
            || lower.contains("mips")
            || (cfg!(target_arch = "aarch64")
                && (lower.contains("x86_64") || lower.contains(".x86.")))
            || (cfg!(target_arch = "x86_64") && lower.contains("arm64"));
        if is_foreign_abi { 2 } else { 1 }
    };

    all.sort_by_key(|n| rank(n));
    all
}

/// If `archive` is an `.xapk` split bundle (a ZIP carrying inner `.apk` files
/// rather than holding the engine or `assets/` directly), safely inspect it
/// under ADR-014 bounds and extract the inner APKs that supply the host's
/// engine and `assets/` into `into`.
///
/// Returns `Ok(None)` when `archive` is an ordinary APK (not an XAPK),
/// `Ok(Some(extracted))` when `archive` was an XAPK (the vector holds the
/// kept inner APKs — possibly empty if the bundle had no matching ABI split —
/// each of which must still pass Roblox APK signature verification downstream),
/// or `Err` on a hostile/corrupt XAPK or cancellation.
fn unpack_xapk(
    archive: &Path,
    into: &Path,
    prefix: &str,
    cancel: &super::Cancel,
) -> Result<Option<Vec<PathBuf>>, Unreachable> {
    let (direct_engine, direct_assets) = inspect_apk_contents(archive);
    if direct_engine || direct_assets {
        return Ok(None);
    }

    let has_inner_apk = {
        let Ok(file) = std::fs::File::open(archive) else { return Ok(None) };
        let Ok(mut zip) = zip::ZipArchive::new(std::io::BufReader::new(file)) else {
            return Ok(None);
        };
        (0..zip.len()).any(|i| {
            zip.by_index(i)
                .map(|e| e.name().to_ascii_lowercase().ends_with(".apk"))
                .unwrap_or(false)
        })
    };
    if !has_inner_apk {
        return Ok(None);
    }

    // Apply ADR-014 archive safety refusals (no symlinks, no path traversal,
    // no setuid, bounded entries and total uncompressed bytes) to the outer
    // XAPK container before extracting any inner APK.
    crate::apk::inspect(
        archive,
        crate::apk::Limits {
            max_entries: MAX_XAPK_ENTRIES,
            max_total_bytes: MAX_XAPK_TOTAL_BYTES,
        },
    )
    .map_err(|e| Unreachable::Malformed {
        url: archive.display().to_string(),
        why: format!("refused XAPK bundle: {e}"),
    })?;

    let file = std::fs::File::open(archive)
        .map_err(|e| Unreachable::NoSource { why: e.to_string() })?;
    let mut zip = zip::ZipArchive::new(std::io::BufReader::new(file)).map_err(|e| {
        Unreachable::Malformed {
            url: archive.display().to_string(),
            why: format!("could not open XAPK bundle: {e}"),
        }
    })?;

    let entries = xapk_apk_entries(&mut zip);
    let mut kept: Vec<PathBuf> = Vec::new();
    let mut found_engine = false;
    let mut found_assets = false;
    let mut total_written: u64 = 0;

    let outcome = (|| -> Result<(), Unreachable> {
        let mut buf = vec![0u8; 256 * 1024];
        for (idx, entry_name) in entries.iter().enumerate() {
            if found_engine && found_assets {
                break;
            }
            cancel.check()?;
            let mut entry = zip.by_name(entry_name).map_err(|e| Unreachable::Malformed {
                url: archive.display().to_string(),
                why: format!("could not read XAPK entry {entry_name:?}: {e}"),
            })?;
            // Write to a Vanta-chosen filename inside `into`, never trusting
            // the inner entry's path as a filesystem destination.
            let dest = into.join(format!("{prefix}-inner-{idx}.apk"));
            let write_result = (|| -> Result<(), Unreachable> {
                let mut out = std::fs::OpenOptions::new()
                    .write(true)
                    .create(true)
                    .truncate(true)
                    .open(&dest)
                    .map_err(|e| Unreachable::NoSource { why: e.to_string() })?;
                let mut entry_written: u64 = 0;
                loop {
                    let n = entry.read(&mut buf).map_err(|e| Unreachable::Malformed {
                        url: archive.display().to_string(),
                        why: format!("could not decompress XAPK entry {entry_name:?}: {e}"),
                    })?;
                    if n == 0 {
                        break;
                    }
                    cancel.check()?;
                    entry_written = entry_written.saturating_add(n as u64);
                    total_written = total_written.saturating_add(n as u64);
                    if entry_written > MAX_ARCHIVE || total_written > MAX_XAPK_TOTAL_BYTES {
                        return Err(Unreachable::Malformed {
                            url: archive.display().to_string(),
                            why: format!(
                                "XAPK entry {entry_name:?} exceeds the extracted archive size limit"
                            ),
                        });
                    }
                    out.write_all(&buf[..n])
                        .map_err(|e| Unreachable::NoSource { why: e.to_string() })?;
                }
                out.sync_all().map_err(|e| Unreachable::NoSource { why: e.to_string() })?;
                Ok(())
            })();
            if let Err(e) = write_result {
                let _ = std::fs::remove_file(&dest);
                return Err(e);
            }

            if !has_zip_magic(&dest) {
                let _ = std::fs::remove_file(&dest);
                continue;
            }
            if let Err(e) = crate::apk::inspect(&dest, crate::apk::Limits::default()) {
                let _ = std::fs::remove_file(&dest);
                return Err(Unreachable::Malformed {
                    url: archive.display().to_string(),
                    why: format!("refused inner APK {entry_name:?} inside XAPK: {e}"),
                });
            }

            let (has_engine, has_assets) = inspect_apk_contents(&dest);
            let useful = (has_engine && !found_engine) || (has_assets && !found_assets);
            if useful {
                if has_engine {
                    found_engine = true;
                }
                if has_assets {
                    found_assets = true;
                }
                kept.push(dest);
            } else {
                let _ = std::fs::remove_file(&dest);
            }
        }
        Ok(())
    })();

    if let Err(e) = outcome {
        for p in &kept {
            let _ = std::fs::remove_file(p);
        }
        return Err(e);
    }
    Ok(Some(kept))
}

/// Which of `files`, if any, holds the engine and which holds the assets.
///
/// Split out of [`classify`] so a caller downloading one candidate at a time
/// can ask, after each one lands, whether it already has enough — a monolithic
/// archive sets both from the one file in a single pass here, which is what
/// lets `fetch` stop after the first download in the ordinary case rather than
/// fetching every candidate before anything is inspected.
fn held(files: &[PathBuf]) -> (Option<PathBuf>, Option<PathBuf>) {
    let mut engine: Option<PathBuf> = None;
    let mut assets: Option<PathBuf> = None;
    for f in files {
        let Ok(file) = std::fs::File::open(f) else { continue };
        let Ok(mut zip) = zip::ZipArchive::new(std::io::BufReader::new(file)) else { continue };
        if engine.is_none() && zip.by_name(crate::apk::LIBRARY_IN_APK).is_ok() {
            engine = Some(f.clone());
        }
        if assets.is_none() && (0..zip.len()).any(|i| {
            zip.by_index(i).map(|e| e.name().starts_with("assets/")).unwrap_or(false)
        }) {
            assets = Some(f.clone());
        }
    }
    (engine, assets)
}

/// Which of the downloaded archives is the base and which holds the engine.
///
/// This reads the archives rather than their names, because the names came from
/// the mirror. A monolithic APK holding both is reported as both, which is what
/// [`Archives`] is for.
fn classify(files: &[PathBuf]) -> Option<Archives> {
    let (engine, assets) = held(files);
    let engine = engine?;
    Some(Archives { base: assets.unwrap_or_else(|| engine.clone()), split: engine })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A response captured from the live service. Absent unless somebody has
    /// run the probe, so the tests that need it skip rather than fake: a
    /// synthetic blob written by this file would only prove the reader agrees
    /// with itself.
    fn captured() -> Option<Vec<u8>> {
        let p = std::env::var_os("VANTA_APKPURE_FIXTURE")?;
        std::fs::read(p).ok()
    }

    #[test]
    fn a_run_of_digits_with_no_dot_is_not_a_version() {
        assert!(markers(b"\x00123456\x3a\x00").is_empty());
    }

    #[test]
    fn a_version_inside_a_longer_token_is_not_a_marker() {
        // Preceded by a name byte, so it is part of something else.
        assert!(markers(b"build9.9.9\x3a").is_empty());
        assert_eq!(markers(b"\x009.9.9\x3a")[0].name, "9.9.9");
    }

    #[test]
    fn a_version_not_terminated_by_the_colon_tag_is_not_a_marker() {
        assert!(markers(b"\x009.9.9\x40").is_empty());
    }

    /// **The condition that stops a code being attached to the wrong name.**
    /// Everything else about the backward scan can hold while the two fields
    /// belong to different records; only adjacency rules that out.
    #[test]
    fn a_version_code_separated_from_its_name_is_not_accepted() {
        let mut d = Vec::new();
        d.push(0x2a);
        d.push(4);
        d.extend_from_slice(b"2908");
        d.push(0x32);
        d.push(5);
        d.extend_from_slice(&[0, 0, 0, 0]); // something between, so not adjacent
        d.extend_from_slice(b"1.2.3");
        d.push(0x3a);
        let m = markers(&d);
        assert_eq!(m.len(), 1);
        assert_eq!(code_before(&d, &m[0]), None);
    }

    #[test]
    fn a_well_formed_record_yields_its_code() {
        let mut d = vec![0u8];
        d.push(0x2a);
        d.push(4);
        d.extend_from_slice(b"2908");
        d.push(0x32);
        d.push(5);
        d.extend_from_slice(b"1.2.3");
        d.push(0x3a);
        let m = markers(&d);
        assert_eq!(m[0].name, "1.2.3");
        assert_eq!(code_before(&d, &m[0]), Some(2908));
    }

    fn record(code: &str, name: &str) -> Vec<u8> {
        let mut d = vec![0u8, 0x2a, code.len() as u8];
        d.extend_from_slice(code.as_bytes());
        d.push(0x32);
        d.push(name.len() as u8);
        d.extend_from_slice(name.as_bytes());
        d.push(0x3a);
        d
    }

    /// Listed once each and newest first, whatever order the records came in.
    #[test]
    fn the_version_list_is_deduplicated_and_ordered_by_code() {
        let mut d = record("2908", "2.734.917");
        d.extend(record("3101", "2.738.1397"));
        d.extend(record("2908", "2.734.917"));
        // A dotted number with no code beside it is not a version.
        d.extend_from_slice(b"\x001.2.3\x3a");
        let names: Vec<String> = versions_in(&d).into_iter().map(|a| a.name).collect();
        assert_eq!(names, ["2.738.1397", "2.734.917"]);
    }

    #[test]
    fn a_url_that_is_not_https_is_not_taken_from_a_record() {
        let mut r = b"XAPKJ".to_vec();
        r.extend_from_slice(&[0x00, 0x00]);
        r.extend_from_slice(b"http://evil.example/x.apk");
        assert!(urls_in(&r).is_empty());
    }

    /// The reader against a real response, if one has been captured. This is
    /// the only test here that says anything about the actual service.
    #[test]
    fn the_captured_response_reads_the_way_the_module_header_records() {
        let Some(d) = captured() else {
            eprintln!("skipped: no captured response (set VANTA_APKPURE_FIXTURE)");
            return;
        };
        let marks = markers(&d);
        assert!(marks.len() > 3, "expected several versions, found {}", marks.len());
        let code = code_before(&d, &marks[0]).expect("the newest version must have a code");
        assert!(code > 0);
        let urls = downloads_for(&d, &marks[0].name);
        assert!(!urls.is_empty(), "the newest version must offer a download");
        for u in &urls {
            url_policy::check(u, &Mirror::default().download_allowed())
                .unwrap_or_else(|e| panic!("a real download URL must pass the allow-list: {e}"));
        }
        eprintln!("captured: {} at code {code}, {} download(s)", marks[0].name, urls.len());
    }

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("vanta-update-mirror-{tag}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A zip holding whatever entries are named. Same shape as `apk`'s and
    /// `install`'s own fixture: the entry path is what `held` acts on, and
    /// nothing here is a real Roblox byte.
    fn zip_of(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        for (name, body) in entries {
            w.start_file(*name, zip::write::SimpleFileOptions::default()).unwrap();
            std::io::Write::write_all(&mut w, body).unwrap();
        }
        w.finish().unwrap().into_inner()
    }

    /// **The common case, and the reason `fetch`'s download loop can stop
    /// after one candidate.** A monolithic archive sets both `engine` and
    /// `assets` from the same file in one pass, because the two conditions in
    /// `held`'s loop are independent checks against the same open zip rather
    /// than a match on which one it "is".
    #[test]
    fn held_finds_both_halves_in_one_monolithic_archive() {
        let dir = scratch("monolithic");
        let one = dir.join("one.apk");
        std::fs::write(
            &one,
            zip_of(&[("assets/x.json", b"{}" as &[u8]), (crate::apk::LIBRARY_IN_APK, b"\x7fELF")]),
        )
        .unwrap();
        let (engine, assets) = held(std::slice::from_ref(&one));
        assert_eq!(engine.as_deref(), Some(one.as_path()));
        assert_eq!(assets.as_deref(), Some(one.as_path()));
    }

    /// **The case that must not stop the loop early.** A real split build
    /// needs both files; asking after only the first has landed must say so
    /// plainly rather than reporting the archive that has arrived as enough.
    #[test]
    fn held_needs_both_files_when_the_build_is_genuinely_split() {
        let dir = scratch("split");
        let base = dir.join("base.apk");
        let split = dir.join("split.apk");
        std::fs::write(&base, zip_of(&[("assets/x.json", b"{}")])).unwrap();
        std::fs::write(&split, zip_of(&[(crate::apk::LIBRARY_IN_APK, b"\x7fELF")])).unwrap();

        let (engine, assets) = held(std::slice::from_ref(&base));
        assert!(engine.is_none(), "the base half alone carries no engine");
        assert!(assets.is_some(), "and it does carry the assets it was checked for");

        let (engine, assets) = held(&[base.clone(), split.clone()]);
        assert_eq!(engine.as_deref(), Some(split.as_path()));
        assert_eq!(assets.as_deref(), Some(base.as_path()));
    }

    #[test]
    fn held_finds_neither_in_an_archive_with_no_engine_and_no_assets() {
        let dir = scratch("neither");
        let apk = dir.join("nothing.apk");
        std::fs::write(&apk, zip_of(&[("AndroidManifest.xml", b"x")])).unwrap();
        let (engine, assets) = held(&[apk]);
        assert!(engine.is_none() && assets.is_none());
    }

    /// `classify` still has to agree with `held` once both halves are in:
    /// this is the seam between the two, kept as a test now that `classify`
    /// is a thin wrapper rather than its own copy of the scan.
    #[test]
    fn classify_still_pairs_a_split_build_correctly() {
        let dir = scratch("classify-split");
        let base = dir.join("base.apk");
        let split = dir.join("split.apk");
        std::fs::write(&base, zip_of(&[("assets/x.json", b"{}")])).unwrap();
        std::fs::write(&split, zip_of(&[(crate::apk::LIBRARY_IN_APK, b"\x7fELF")])).unwrap();
        let archives = classify(&[base.clone(), split.clone()]).expect("both halves are present");
        assert_eq!(archives.base, base);
        assert_eq!(archives.split, split);
    }

    #[test]
    fn xapk_with_manifest_extracts_base_and_abi_split_and_drops_unused_splits() {
        let dir = scratch("xapk-manifest");
        let xapk = dir.join("candidate-0.apk");
        let split_name = format!("config.{}.apk", crate::apk::HOST_ABI.replace('-', "_"));
        let manifest = format!(
            r#"{{"package_name":"com.roblox.client","split_apks":[{{"file":"com.roblox.client.apk","id":"base"}},{{"file":"{split_name}","id":"config.abi"}},{{"file":"config.xxhdpi.apk","id":"config.xxhdpi"}},{{"file":"config.armeabi_v7a.apk","id":"config.armeabi_v7a"}}]}}"#
        );
        let base_bytes = zip_of(&[("assets/content/fonts/x.json", b"{}")]);
        let split_bytes = zip_of(&[(crate::apk::LIBRARY_IN_APK, b"\x7fELF")]);
        let dpi_bytes = zip_of(&[("res/drawable/icon.png", b"png")]);
        let foreign_bytes = zip_of(&[("lib/armeabi-v7a/libroblox.so", b"\x7fELF32")]);

        std::fs::write(
            &xapk,
            zip_of(&[
                ("manifest.json", manifest.as_bytes()),
                ("com.roblox.client.apk", &base_bytes),
                (&split_name, &split_bytes),
                ("config.xxhdpi.apk", &dpi_bytes),
                ("config.armeabi_v7a.apk", &foreign_bytes),
            ]),
        )
        .unwrap();

        let staging = dir.join("staging");
        std::fs::create_dir_all(&staging).unwrap();
        let cancel = super::super::Cancel::new();
        let extracted = unpack_xapk(&xapk, &staging, "candidate-0", &cancel)
            .expect("valid xapk")
            .expect("recognised as xapk");
        assert_eq!(extracted.len(), 2, "only base and host ABI split should be kept");

        let archives = classify(&extracted).expect("both halves extracted from xapk");
        assert_ne!(archives.base, archives.split);
        let (base_engine, base_assets) = inspect_apk_contents(&archives.base);
        let (split_engine, split_assets) = inspect_apk_contents(&archives.split);
        assert!(!base_engine && base_assets);
        assert!(split_engine && !split_assets);

        // No irrelevant splits left behind in staging.
        let staging_count = std::fs::read_dir(&staging).unwrap().count();
        assert_eq!(staging_count, 2);

        // Every extracted APK must still pass Roblox signature verification;
        // synthetic unsigned inner APKs are refused.
        for apk in archives.distinct() {
            assert!(
                crate::apk_signature::verify_signed_by(apk, &crate::apk_signature::pinned())
                    .is_err(),
                "unsigned inner APK {} must never pass signature verification",
                apk.display()
            );
        }
    }

    #[test]
    fn xapk_without_manifest_still_extracts_base_and_abi_split() {
        let dir = scratch("xapk-no-manifest");
        let xapk = dir.join("bundle.xapk");
        let base_bytes = zip_of(&[("assets/x.json", b"{}")]);
        let split_bytes = zip_of(&[(crate::apk::LIBRARY_IN_APK, b"\x7fELF")]);
        std::fs::write(
            &xapk,
            zip_of(&[("base.apk", &base_bytes), ("split_config.abi.apk", &split_bytes)]),
        )
        .unwrap();

        let staging = dir.join("staging");
        std::fs::create_dir_all(&staging).unwrap();
        let cancel = super::super::Cancel::new();
        let extracted = unpack_xapk(&xapk, &staging, "cand", &cancel)
            .unwrap()
            .expect("recognised as xapk even without manifest.json");
        let archives = classify(&extracted).expect("both halves extracted");
        assert_ne!(archives.base, archives.split);
    }

    #[test]
    fn xapk_with_only_foreign_abi_yields_no_engine() {
        let dir = scratch("xapk-wrong-abi");
        let xapk = dir.join("foreign.xapk");
        let base_bytes = zip_of(&[("assets/x.json", b"{}")]);
        let foreign_bytes = zip_of(&[("lib/armeabi-v7a/libroblox.so", b"\x7fELF32")]);
        std::fs::write(
            &xapk,
            zip_of(&[
                ("com.roblox.client.apk", &base_bytes),
                ("config.armeabi_v7a.apk", &foreign_bytes),
            ]),
        )
        .unwrap();

        let staging = dir.join("staging");
        std::fs::create_dir_all(&staging).unwrap();
        let cancel = super::super::Cancel::new();
        let extracted = unpack_xapk(&xapk, &staging, "cand", &cancel)
            .unwrap()
            .expect("recognised as xapk");
        assert!(classify(&extracted).is_none(), "no host engine inside foreign-ABI xapk");
    }

    #[test]
    fn xapk_with_hostile_traversal_or_symlink_is_refused() {
        let dir = scratch("xapk-hostile");
        let staging = dir.join("staging");
        std::fs::create_dir_all(&staging).unwrap();
        let cancel = super::super::Cancel::new();

        // 1. Parent directory traversal inside outer XAPK.
        let trav = dir.join("traversal.xapk");
        let inner = zip_of(&[(crate::apk::LIBRARY_IN_APK, b"\x7fELF")]);
        std::fs::write(&trav, zip_of(&[("../../evil.apk", &inner)])).unwrap();
        assert!(matches!(
            unpack_xapk(&trav, &staging, "t", &cancel),
            Err(Unreachable::Malformed { .. })
        ));
        assert_eq!(std::fs::read_dir(&staging).unwrap().count(), 0);

        // 2. Symlink inside outer XAPK.
        let sym = dir.join("symlink.xapk");
        let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        w.start_file("com.roblox.client.apk", zip::write::SimpleFileOptions::default()).unwrap();
        std::io::Write::write_all(&mut w, &inner).unwrap();
        w.add_symlink("link", "/etc/passwd", zip::write::SimpleFileOptions::default()).unwrap();
        std::fs::write(&sym, w.finish().unwrap().into_inner()).unwrap();
        assert!(matches!(
            unpack_xapk(&sym, &staging, "s", &cancel),
            Err(Unreachable::Malformed { .. })
        ));
        assert_eq!(std::fs::read_dir(&staging).unwrap().count(), 0);
    }

    #[test]
    fn unpack_xapk_leaves_monolithic_apk_untouched() {
        let dir = scratch("xapk-monolithic");
        let apk = dir.join("mono.apk");
        std::fs::write(
            &apk,
            zip_of(&[("assets/x.json", b"{}" as &[u8]), (crate::apk::LIBRARY_IN_APK, b"\x7fELF")]),
        )
        .unwrap();
        let cancel = super::super::Cancel::new();
        assert!(unpack_xapk(&apk, &dir, "m", &cancel).unwrap().is_none());
    }

    /// Against the real mirror, so run by hand: `cargo test -p vanta-update
    /// -- --ignored the_live_mirror_lists`. The fixtures above pin the parser to
    /// a shape; this is the only check that the shape is still the one served.
    #[test]
    #[ignore = "network"]
    fn the_live_mirror_lists_versions() {
        let listed = offered().expect("the mirror answered with a version list");
        for a in &listed {
            println!("{} ({})", a.name, a.code);
        }
        assert!(!listed.is_empty());
        assert!(listed.windows(2).all(|w| w[0].code > w[1].code), "newest first, no repeats");
    }
}
