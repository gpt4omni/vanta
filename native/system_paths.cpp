// Android's `/system` tree, served from a directory Vanta owns.
//
// Roblox asks the platform for `/system/fonts/NotoSansCJK-Regular.ttc`. On
// Android that always exists. On a Linux host there is no `/system` at all, the
// lookup fails, and the engine turns the failure into an *empty* path and throws
// during app startup:
//
//     RBXCRASH: UnhandledException (St13runtime_error Path does not exist: "")
//
// Which is a genuinely hard failure to read from the outside — the exception
// names no path, because by then there isn't one. It was found by tracing the
// path-taking libc calls and noticing that the same thread stats the font, gets
// -1, and immediately stats "" three times.
//
// Serving `/system` is not a workaround for a Roblox bug. It is part of what an
// Android runtime owes the code it hosts, exactly like `AAssetManager` or
// `ALooper`. Vanta owns the symbol table, so the redirect belongs at the libc
// boundary rather than anywhere near the engine.
//
// Written in C++ because `open` is variadic, and forwarding a C variadic to the
// real `open` is the one thing C does better here — the same reason liblog.cpp
// is C++.

#include <atomic>
#include <cstdarg>
#include <cstddef>
#include <cstdio>
#include <cstring>
#include <cstdlib>

#include <cerrno>
#include <dirent.h>
#include <sys/syscall.h>
#include <fcntl.h>
#include <limits.h>
#include <sys/stat.h>
#include <sys/statvfs.h>
#include <cstdint>
#include <unistd.h>

/// Weakly linked into `vanta-runtime`'s `asset::vanta_overlay_resolve` so
/// standalone C++ unit tests and `vanta-linker-sys` binaries that do not link
/// `vanta-runtime` still link cleanly with zero undefined symbols.
extern "C" __attribute__((weak)) int vanta_overlay_resolve(
    const char* path, int for_write, char* out, size_t out_len);

namespace {

/// The host directory standing in for `/system`. Set once from Rust before the
/// engine runs; empty means "no redirect", which leaves every call untouched.
char g_root[PATH_MAX];
size_t g_root_len = 0;

/// The extracted Roblox asset root (`<cache>/assets`), set from Rust via
/// `vanta_set_asset_root`. Used as a zero-overhead prefix gate before crossing
/// into `vanta_overlay_resolve` so non-asset paths pay no FFI cost.
char g_asset_root[PATH_MAX];
std::atomic<size_t> g_asset_root_len{0};

/// `VANTA_TRACE_PATHS=1`. Every function here is fixed-arity except `open`,
/// which is forwarded properly, so this is safe to leave on — unlike
/// `VANTA_TRACE=1`, which wraps variadics with fixed-arity declarations and
/// makes the engine abort.
///
/// These wrappers are the only place the path calls are intercepted. An earlier
/// version had a second set in `trace.rs`; because both landed in the same
/// symbol map the tracing copy silently won and the redirect never ran, which
/// looked exactly like the redirect not working.
bool g_trace = false;

void trace(const char* call, const char* path, const char* result) {
    if (!g_trace) {
        return;
    }
    // The thread id is not decoration: Roblox spreads this work over twenty-odd
    // threads, and a single interleaved log invites reading two unrelated calls
    // as cause and effect.
    std::fprintf(stderr, "[paths] tid=%ld %s(\"%s\") = %s\n",
                 (long)::syscall(SYS_gettid), call, path ? path : "(null)", result);
}

void trace_i(const char* call, const char* path, long r) {
    if (!g_trace) {
        return;
    }
    char b[32];
    std::snprintf(b, sizeof b, "%ld", r);
    trace(call, path, b);
}

static inline bool open_is_write(int flags) {
    const int acc = flags & O_ACCMODE;
    return acc != O_RDONLY || (flags & (O_CREAT | O_TRUNC | O_APPEND | O_TMPFILE)) != 0;
}

static inline bool fopen_is_write(const char* mode) {
    if (!mode) {
        return false;
    }
    return std::strpbrk(mode, "wa+") != nullptr;
}

/// Rewrite `/system/<rest>` to `<root>/<rest>`, or consult `vanta_overlay_resolve`
/// for read-only accesses under the registered extracted Roblox asset root.
///
/// Returns null when the path is neither under `/system` nor overlaid.
const char* remap(const char* path, int for_write, char* buf, size_t n) {
    if (!path) {
        return nullptr;
    }
    if (g_root_len != 0 && std::strncmp(path, "/system/", 8) == 0) {
        // `path + 7` keeps the separator, so the result is `<root>/fonts/...`.
        const char* tail = path + 7;
        size_t tail_len = std::strlen(tail);
        if (g_root_len + tail_len + 1 > n) {
            return nullptr;
        }
        std::memcpy(buf, g_root, g_root_len);
        std::memcpy(buf + g_root_len, tail, tail_len + 1);
        return buf;
    }
    if (!for_write && vanta_overlay_resolve != nullptr) {
        const size_t alen = g_asset_root_len.load(std::memory_order_acquire);
        if (alen != 0 && std::strncmp(path, g_asset_root, alen) == 0 && path[alen] == '/') {
            if (vanta_overlay_resolve(path, 0, buf, n) == 1) {
                return buf;
            }
        }
    }
    return nullptr;
}

static inline bool needs_slow_path(const char* path) {
    if (__builtin_expect(g_trace, 0)) {
        return true;
    }
    if (__builtin_expect(!path, 0)) {
        return false;
    }
    if (__builtin_expect(g_root_len != 0 && path[0] == '/' && path[1] == 's' &&
                             std::strncmp(path, "/system/", 8) == 0,
                         0)) {
        return true;
    }
    if (vanta_overlay_resolve != nullptr) {
        const size_t alen = g_asset_root_len.load(std::memory_order_acquire);
        if (__builtin_expect(alen != 0 && path[0] == g_asset_root[0] &&
                                 std::strncmp(path, g_asset_root, alen) == 0 &&
                                 path[alen] == '/',
                             0)) {
            return true;
        }
    }
    return false;
}

#define REMAP(path, for_write)                                  \
    char _buf[PATH_MAX];                                        \
    const char* _p = remap(path, for_write, _buf, sizeof _buf); \
    const char* real = _p ? _p : (path)

[[gnu::noinline]] int s_stat_slow(const char* path, struct stat* out) {
    REMAP(path, 0);
    int r = ::stat(real, out);
    trace_i("stat", real, r);
    return r;
}
int s_stat(const char* path, struct stat* out) {
    if (__builtin_expect(!needs_slow_path(path), 1)) {
        return ::stat(path, out);
    }
    return s_stat_slow(path, out);
}

[[gnu::noinline]] int s_lstat_slow(const char* path, struct stat* out) {
    REMAP(path, 0);
    int r = ::lstat(real, out);
    trace_i("lstat", real, r);
    return r;
}
int s_lstat(const char* path, struct stat* out) {
    if (__builtin_expect(!needs_slow_path(path), 1)) {
        return ::lstat(path, out);
    }
    return s_lstat_slow(path, out);
}

[[gnu::noinline]] int s_access_slow(const char* path, int mode) {
    REMAP(path, (mode & W_OK) != 0 ? 1 : 0);
    int r = ::access(real, mode);
    trace_i("access", real, r);
    return r;
}
int s_access(const char* path, int mode) {
    if (__builtin_expect(!needs_slow_path(path), 1)) {
        return ::access(path, mode);
    }
    return s_access_slow(path, mode);
}

[[gnu::noinline]] DIR* s_opendir_slow(const char* path) {
    REMAP(path, 0);
    DIR* d = ::opendir(real);
    trace(d ? "opendir" : "opendir!", real, d ? "ok" : "null");
    return d;
}
DIR* s_opendir(const char* path) {
    if (__builtin_expect(!needs_slow_path(path), 1)) {
        return ::opendir(path);
    }
    return s_opendir_slow(path);
}

char* s_realpath(const char* path, char* resolved) {
    REMAP(path, 0);
    if (!resolved) {
        // glibc's `realpath(path, NULL)` is a GNU extension: it `malloc`s the
        // result buffer itself and hands ownership to the caller, who is
        // expected to `free` it. That allocation comes from the *host's*
        // allocator — Roblox statically links its own (mimalloc, going by
        // `DFLog::Mimalloc`), and every one of its `malloc`/`free`/`new`/
        // `delete` symbols is resolved internally, never through Vanta's
        // symbol table. When the engine later releases a buffer this call
        // handed it, that release runs entirely inside Roblox's own
        // allocator, which indexes a table keyed by the pointer's own
        // address to find the arena that owns it. A host-`malloc`'d pointer
        // was never registered in that table, so the lookup's first level
        // comes back null and the very next dereference — unconditional,
        // with no null check — faults.
        //
        // This is exactly what feeds Roblox's cURL-based HTTP stack the
        // CA-bundle path (`./exe/cacert.pem`, resolved once per connection
        // going by `VANTA_TRACE_PATHS=1`): confirmed live under lldb with a
        // breakpoint on this function — `resolved` is null, the host
        // `realpath` call mallocs, and the pointer it returns is the exact
        // address that later faults on the `HttpClient` thread with
        // `rax=0x0, rcx=0xe000` — a segment-map miss for foreign memory.
        //
        // There is no buffer Vanta can hand back here that is safe for the
        // engine to free through its own allocator, because that allocator's
        // bookkeeping is not reachable from here (its `malloc`/`free` are not
        // exported). The only safe move is to never produce the host
        // allocation in the first place. Resolving into a stack buffer only
        // for the trace log and reporting failure (as `realpath` is
        // documented to do when resolution cannot be completed) is a real,
        // if slightly degraded, POSIX outcome: the caller falls back to the
        // path it already had, which is what every caller of this GNU form is
        // required to handle.
        //
        // `vasprintf` and `getcwd(NULL, n)` have the same shape and get the same
        // treatment in foreign_alloc.cpp.
        if (!g_trace) {
            errno = ENOTSUP;
            return nullptr;
        }
        char tmp[PATH_MAX];
        char* r = ::realpath(real, tmp);
        trace("realpath", real, r ? r : "null (unsupported: no caller buffer)");
        errno = r ? ENOTSUP : errno;
        return nullptr;
    }
    char* r = ::realpath(real, resolved);
    trace("realpath", real, r ? r : "null");
    return r;
}

[[gnu::noinline]] ssize_t s_readlink_slow(const char* path, char* buf, size_t n) {
    REMAP(path, 0);
    ssize_t r = ::readlink(real, buf, n);
    trace_i("readlink", real, (long)r);
    return r;
}
ssize_t s_readlink(const char* path, char* buf, size_t n) {
    if (__builtin_expect(!needs_slow_path(path), 1)) {
        return ::readlink(path, buf, n);
    }
    return s_readlink_slow(path, buf, n);
}

[[gnu::noinline]] FILE* s_fopen_slow(const char* path, const char* mode) {
    REMAP(path, fopen_is_write(mode) ? 1 : 0);
    FILE* f = ::fopen(real, mode);
    trace("fopen", real, f ? "ok" : "null");
    return f;
}
FILE* s_fopen(const char* path, const char* mode) {
    if (__builtin_expect(!needs_slow_path(path), 1)) {
        return ::fopen(path, mode);
    }
    return s_fopen_slow(path, mode);
}

[[gnu::noinline]] int s_open_slow(const char* path, int flags, unsigned mode) {
    REMAP(path, open_is_write(flags) ? 1 : 0);
    int r = ::open(real, flags, mode);
    trace_i("open", real, r);
    return r;
}
/// `open` is variadic: the mode argument exists only for `O_CREAT`/`O_TMPFILE`.
/// Reading it unconditionally would walk the register save area for an argument
/// the caller never pushed, which is the mistake that makes `VANTA_TRACE=1`
/// abort the engine.
int s_open(const char* path, int flags, ...) {
    unsigned mode = 0;
    if (flags & (O_CREAT | O_TMPFILE)) {
        va_list ap;
        va_start(ap, flags);
        mode = va_arg(ap, unsigned);
        va_end(ap);
    }
    if (__builtin_expect(!needs_slow_path(path), 1)) {
        return ::open(path, flags, mode);
    }
    return s_open_slow(path, flags, mode);
}

/// `statvfs`, which the engine imports and Vanta had never intercepted.
///
/// Two separate defects, and the second is why this is not just a redirect.
///
/// **It was not path-translated.** Every other path-taking call here is; this
/// one went straight to the host with whatever the engine built, and because it
/// was not in the table it did not appear in `VANTA_TRACE_PATHS=1` output
/// either. A trace that cannot see a call is not evidence the call did not
/// happen, and a conclusion in flag-init.md §23.2 -- "storage is never
/// attempted, 19,296 path calls and none of them `rbx-storage`" -- was drawn
/// with this blind spot in it.
///
/// **The struct layouts differ.** bionic's `struct statvfs` runs
/// `f_fsid, f_flag, f_namemax`; glibc's inserts an `int __f_unused` after
/// `f_fsid`, which on LP64 pushes `f_flag` and `f_namemax` eight bytes along.
/// The size fields the engine cares about for free space -- `f_bsize` through
/// `f_favail` -- happen to align, so this is not obviously fatal, but the engine
/// reading `f_flag` gets glibc's padding and reading `f_namemax` gets glibc's
/// `f_flag`. `ST_RDONLY` lives in `f_flag`. This is the same family as the
/// `sigset_t`, `struct sigaction` and `mallinfo` divergences already recorded,
/// and it is fixed the same way: fill the bionic shape by hand rather than hope
/// the two agree.
struct bionic_statvfs {
    unsigned long f_bsize;
    unsigned long f_frsize;
    unsigned long f_blocks;
    unsigned long f_bfree;
    unsigned long f_bavail;
    unsigned long f_files;
    unsigned long f_ffree;
    unsigned long f_favail;
    unsigned long f_fsid;
    unsigned long f_flag;
    unsigned long f_namemax;
    uint32_t __f_reserved[6];
};

int s_statvfs(const char* path, bionic_statvfs* out) {
    REMAP(path, 0);
    struct ::statvfs host {};
    const int r = ::statvfs(real, &host);
    trace_i("statvfs", real, r);
    if (r == 0 && out) {
        *out = bionic_statvfs{};
        out->f_bsize = host.f_bsize;
        out->f_frsize = host.f_frsize;
        out->f_blocks = host.f_blocks;
        out->f_bfree = host.f_bfree;
        out->f_bavail = host.f_bavail;
        out->f_files = host.f_files;
        out->f_ffree = host.f_ffree;
        out->f_favail = host.f_favail;
        out->f_fsid = host.f_fsid;
        out->f_flag = host.f_flag;
        out->f_namemax = host.f_namemax;
    }
    return r;
}

#undef REMAP

} // namespace

extern "C" struct VantaSystemSymbol {
    const char* name;
    void* addr;
};

/// Point the redirect at a host directory. Passing null or "" disables it.
extern "C" void vanta_set_system_root(const char* root) {
    if (!root || !*root) {
        g_root_len = 0;
        g_root[0] = '\0';
        return;
    }
    std::snprintf(g_root, sizeof g_root, "%s", root);
    g_root_len = std::strlen(g_root);
    // A trailing slash would produce `<root>//fonts`, which works but reads
    // badly in a trace, and would break a later exact-prefix comparison.
    while (g_root_len > 1 && g_root[g_root_len - 1] == '/') {
        g_root[--g_root_len] = '\0';
    }
}

/// Register the extracted Roblox asset root (`<cache>/assets`) for overlay
/// prefix gating. Passing null or "" disables filesystem overlay checks.
extern "C" void vanta_set_asset_root(const char* root) {
    g_asset_root_len.store(0, std::memory_order_release);
    if (!root || !*root) {
        g_asset_root[0] = '\0';
        return;
    }
    std::snprintf(g_asset_root, sizeof g_asset_root, "%s", root);
    size_t len = std::strlen(g_asset_root);
    while (len > 1 && g_asset_root[len - 1] == '/') {
        g_asset_root[--len] = '\0';
    }
    g_asset_root_len.store(len, std::memory_order_release);
}

/// Turn on the path log. Separate from the root so tracing works even when the
/// redirect is disabled.
extern "C" void vanta_set_path_trace(int on) {
    g_trace = on != 0;
}

extern "C" const VantaSystemSymbol* vanta_system_symbols(size_t* count) {
    static const VantaSystemSymbol table[] = {
        {"stat", (void*)&s_stat},
        {"lstat", (void*)&s_lstat},
        {"access", (void*)&s_access},
        {"opendir", (void*)&s_opendir},
        {"realpath", (void*)&s_realpath},
        {"readlink", (void*)&s_readlink},
        {"fopen", (void*)&s_fopen},
        {"statvfs", (void*)&s_statvfs},
        {"open", (void*)&s_open},
    };
    *count = sizeof(table) / sizeof(table[0]);
    return table;
}
