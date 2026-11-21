//! Stub implementations for every Android symbol Vanta does not provide yet.
//!
//! A stub records that it was called and returns zero. That makes the first
//! launch attempt produce a *prioritised* list of what to implement — which is
//! considerably more useful than a list of everything Roblox references.
//!
//! `VANTA_STUB_ABORT=1` aborts on the first hit instead, for bisecting a
//! specific failure.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Mutex, OnceLock};

include!(concat!(env!("OUT_DIR"), "/generated_stubs.rs"));

/// Lock-free call counts per stub index. Only the very first hit (`prev == 0`)
/// acquires `FIRST_ORDER` to record first-hit ordering and report once.
fn counts() -> &'static [AtomicU32] {
    static COUNTS: OnceLock<Box<[AtomicU32]>> = OnceLock::new();
    COUNTS.get_or_init(|| (0..SYMBOLS.len()).map(|_| AtomicU32::new(0)).collect())
}

static FIRST_ORDER: Mutex<Vec<usize>> = Mutex::new(Vec::new());

fn abort_on_hit() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VANTA_STUB_ABORT").is_some())
}

fn quiet() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VANTA_STUB_QUIET").is_some())
}

/// Called by every generated stub.
pub fn hit(index: usize) -> i64 {
    let prev = counts()[index].fetch_add(1, Ordering::Relaxed);
    if prev == 0 {
        let mut order = FIRST_ORDER.lock().unwrap_or_else(|e| e.into_inner());
        order.push(index);
        drop(order);

        // Once per symbol into the central register, so the end-of-run report
        // can put libc stubs beside the JNI and framework gaps rather than in a
        // table of their own that has to be correlated by hand.
        if !answering_correctly(SYMBOLS[index].0) {
            crate::unimplemented::record(crate::unimplemented::Kind::LibcStub, SYMBOLS[index].0);
        }
        if !quiet() {
            eprintln!("[stub] {}", SYMBOLS[index].0);
        }
    }
    if abort_on_hit() {
        eprintln!("[stub] VANTA_STUB_ABORT set — aborting on {}", SYMBOLS[index].0);
        report();
        std::process::abort();
    }
    if let Some(why) = is_fatal(SYMBOLS[index].0) {
        fatal(SYMBOLS[index].0, why);
    }
    0
}

/// Symbols where returning `0` is not a harmless placeholder but a lie the
/// caller cannot survive, each with the sentence a person hitting it should
/// read.
///
/// Every stub returns `0`, which is right for most of them: a counter nobody
/// reads, a capability query answered "no". For an entry here it is fatal, and
/// in the worst way — the caller is *told it succeeded* and proceeds.
///
/// **The list is empty, and that does not mean the danger is gone.** It held
/// `pthread_once`, `pthread_getspecific`, `pthread_setspecific`,
/// `pthread_key_create` and `pthread_key_delete` until those five were
/// implemented in `bionic::pthread`; they are resolved now, never reach a stub,
/// and an entry for a symbol that cannot be hit is a comment that lies.
///
/// Nothing replaced them, deliberately. `--lib-dir` without `--host-libc` still
/// ends in SIGSEGV, now further into the engine's static initialisers, with
/// `__cxa_atexit` the last stub reported before the core dump — but *which*
/// stub is the one it cannot survive has not been established, and the obvious
/// guess is wrong: `memset` is stubbed in that configuration and the same run
/// carried on through five more first-hit stubs after calling it. Do not add a
/// symbol here on the strength of it looking dangerous. Add it when a run shows
/// the process dying on it.
///
/// The underlying problem is larger than this list. Bare `--lib-dir` stubs 358
/// libc symbols, `memset` and `pthread_mutex_lock` among them; it is a
/// diagnostic configuration that produces a prioritised work queue, not one the
/// engine can run in. What closes it is a real bionic shim, not more entries
/// here.
const FATAL: &[(&str, &str)] = &[];

fn is_fatal(symbol: &str) -> Option<&'static str> {
    FATAL
        .iter()
        .find(|(name, _)| *name == symbol)
        .map(|(_, why)| *why)
}

/// Say what happened and stop, rather than returning a lie and crashing later.
///
/// A named exit beats a SIGSEGV by the whole distance between "this symbol is
/// not implemented, here is the switch that resolves it" and a core dump three
/// frames into someone else's thread-local teardown.
fn fatal(symbol: &str, why: &str) -> ! {
    eprintln!();
    eprintln!("[stub] {symbol} is not implemented, and returning a placeholder would crash.");
    eprintln!("       {why}");
    eprintln!("       Pass --host-libc to resolve libc from the host, which is what the");
    eprintln!("       --game-activity path does and why that path does not hit this.");
    report();
    // `_exit` for the same reason the loader uses it: Roblox's static
    // initialisers registered atexit handlers that expect a live Android
    // process, and running them here would replace this message with the crash
    // it exists to prevent.
    unsafe { libc_exit(1) }
}

extern "C" {
    #[link_name = "_exit"]
    fn libc_exit(status: std::ffi::c_int) -> !;
}

/// Every stub called at least once, most-called first. This is the work queue.
/// Symbols where a zero return is the contract rather than a shortfall.
///
/// zstd's tracing hooks are the whole list today. `zstd_trace.h` says of
/// `ZSTD_trace_compress_begin` and `ZSTD_trace_decompress_begin`: "@returns
/// Non-zero if tracing is enabled". Zero therefore means tracing is off, and
/// zstd skips the matching `_end` call entirely. Implementing them to return
/// non-zero would oblige us to produce `ZSTD_Trace` records that nothing here
/// consumes -- the stub would start lying, which is the failure AGENTS.md's rule
/// about stubs is written to prevent, arriving from the unusual direction.
///
/// They stay in the stub table so the symbol still resolves and the call still
/// gets an answer; what changes is that the end-of-run report stops calling that
/// answer a gap.
fn answering_correctly(symbol: &str) -> bool {
    matches!(
        symbol,
        "ZSTD_trace_compress_begin"
            | "ZSTD_trace_decompress_begin"
            | "ZSTD_trace_compress_end"
            | "ZSTD_trace_decompress_end"
    )
}

pub fn report() {
    let order = FIRST_ORDER.lock().unwrap_or_else(|e| e.into_inner()).clone();
    if order.is_empty() {
        eprintln!("\n=== no stubs were called ===");
        return;
    }

    let c = counts();
    let mut called: Vec<(usize, u32)> = order
        .iter()
        .map(|&i| (i, c[i].load(Ordering::Relaxed)))
        .collect();
    called.sort_by(|a, b| b.1.cmp(&a.1));

    eprintln!(
        "\n=== stubs called: {} distinct of {} ===",
        called.len(),
        SYMBOLS.len()
    );
    for (i, count) in called {
        eprintln!("  {count:>9}  {}", SYMBOLS[i].0);
    }
}
