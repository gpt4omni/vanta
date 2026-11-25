// Android's liblog, implemented against the host's stderr.
//
// This is the single most valuable thing in the runtime layer to get working
// early. Roblox narrates its own startup through it, so every later failure
// comes with the client's own account of what it was doing — which is otherwise
// unavailable, since nothing else in the process can say anything.
//
// An early spec (§9a) put `onJoin`, `onLeave` and `onLogLine` in the plugin
// event schema and said they were parsed from exactly this stream. None of the
// three exists. `vanta_plugins::core_events::ALL` is a closed table of five
// names, none of them these, and nothing anywhere parses this stream for them.
//
// Corrected rather than deleted because the claim stood here long enough to be
// believed, and was repeated into two design documents from this comment. The
// parsing that would make something like it true is being written against the
// engine's own log file instead -- `vanta_runtime::bloxstrap_rpc` reads
// `appData/logs/*_Player_*.log`, which is a file with a settled format, and
// not this stderr channel, whose shape is Vanta's own narration.
//
// Written in C++ rather than Rust because three of the six entry points are
// variadic, and forwarding a C variadic to a real `vsnprintf` is the one thing
// C does better here.

#include <atomic>
#include <chrono>
#include <condition_variable>
#include <cstdint>
#include <ctime>
#include <cstdarg>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <cstddef>
#include <memory>
#include <mutex>
#include <thread>

extern "C" {

// android/log.h priorities.
enum {
    ANDROID_LOG_UNKNOWN = 0,
    ANDROID_LOG_DEFAULT,
    ANDROID_LOG_VERBOSE,
    ANDROID_LOG_DEBUG,
    ANDROID_LOG_INFO,
    ANDROID_LOG_WARN,
    ANDROID_LOG_ERROR,
    ANDROID_LOG_FATAL,
    ANDROID_LOG_SILENT,
};

} // extern "C"

namespace {

char priority_letter(int prio) {
    switch (prio) {
        case ANDROID_LOG_VERBOSE: return 'V';
        case ANDROID_LOG_DEBUG:   return 'D';
        case ANDROID_LOG_INFO:    return 'I';
        case ANDROID_LOG_WARN:    return 'W';
        case ANDROID_LOG_ERROR:   return 'E';
        case ANDROID_LOG_FATAL:   return 'F';
        case ANDROID_LOG_SILENT:  return 'S';
        default:                  return '?';
    }
}

/// Minimum priority actually printed.
///
/// **The default was INFO, and that cost a whole investigation.** The comment
/// here used to say Roblox is extremely chatty at VERBOSE and DEBUG and that
/// INFO "keeps the useful half". The chattiness claim is wrong for DEBUG:
/// measured on a landing-page run, INFO prints 872 lines and DEBUG prints 960
/// — eighty-seven more, not a flood. What those eighty-seven contained was
/// `nativeInitializeNativeFlags: Registered Flag Provider ID from Java: 0` and
/// `flagCount = 139`, the two lines that tell you the flag provider registered
/// exactly as the real client's Waydroid capture shows it registering.
///
/// Because they were hidden, their absence was read as Vanta failing to
/// register a flag provider at all, and `docs/analysis/flag-init.md` §41 had to
/// warn in writing against building on that absence before someone did. This
/// project has mistaken an absence for evidence nine times; a logger whose
/// default silently drops a whole severity is a machine for producing the tenth.
///
/// VERBOSE is still off by default, and that one is genuinely chatty. Anything
/// hidden by default should be cheap to reveal and loudly documented, which is
/// what `VANTA_LOG_LEVEL` is for.
int minimum_priority() {
    static const int level = [] {
        const char* v = getenv("VANTA_LOG_LEVEL");
        if (!v) return (int)ANDROID_LOG_DEBUG;
        switch (v[0]) {
            case 'v': case 'V': return (int)ANDROID_LOG_VERBOSE;
            case 'd': case 'D': return (int)ANDROID_LOG_DEBUG;
            case 'i': case 'I': return (int)ANDROID_LOG_INFO;
            case 'w': case 'W': return (int)ANDROID_LOG_WARN;
            case 'e': case 'E': return (int)ANDROID_LOG_ERROR;
            default:            return (int)ANDROID_LOG_INFO;
        }
    }();
    return level;
}

/// Wall-clock, to millisecond precision, in front of every line.
///
/// **Vanta's logs carried no clock at all.** All 265 log files on the
/// development machine, not one timestamped line -- which meant "is startup
/// getting slower?" could not be answered from the corpus, and no startup
/// regression here has ever been caught by reading a log. Sober and mocktail
/// both stamp theirs, which is why an engine-phase comparison was possible
/// between those two and not with us.
///
/// ISO-8601 with a `T` and milliseconds, matching the engine's own FLog lines
/// so a reader can sort the two together. Local time rather than UTC because
/// the audience is somebody comparing a log against when they pressed a
/// button; the engine's own lines carry `Z` and are distinguishable.
///
/// Built with `localtime_r` and `snprintf` rather than `std::format` or
/// `put_time`: this runs on every log line, including inside the engine's
/// startup, and it must not allocate.
static long local_utc_offset_secs() {
    static const long offset = []() -> long {
        std::time_t now = std::time(nullptr);
        struct tm tmv {};
        if (!localtime_r(&now, &tmv)) {
            return 0;
        }
        return tmv.tm_gmtoff;
    }();
    return offset;
}

static void write_two_digits(char* p, int v) {
    p[0] = static_cast<char>('0' + (v / 10));
    p[1] = static_cast<char>('0' + (v % 10));
}

static void stamp(char* out, size_t n) {
    if (n < 13) {
        if (n > 0) out[0] = '\0';
        return;
    }
    auto now = std::chrono::system_clock::now();
    auto since = now.time_since_epoch();
    auto secs = std::chrono::duration_cast<std::chrono::seconds>(since).count();
    int ms = static_cast<int>(
        std::chrono::duration_cast<std::chrono::milliseconds>(since).count() - secs * 1000);
    if (ms < 0) {
        ms += 1000;
        secs -= 1;
    }
    long local_secs = static_cast<long>(secs) + local_utc_offset_secs();
    long tod = local_secs % 86400;
    if (tod < 0) tod += 86400;
    int hour = static_cast<int>(tod / 3600);
    int min = static_cast<int>((tod % 3600) / 60);
    int sec = static_cast<int>(tod % 60);
    write_two_digits(out + 0, hour);
    out[2] = ':';
    write_two_digits(out + 3, min);
    out[5] = ':';
    write_two_digits(out + 6, sec);
    out[8] = '.';
    out[9] = static_cast<char>('0' + (ms / 100));
    out[10] = static_cast<char>('0' + ((ms / 10) % 10));
    out[11] = static_cast<char>('0' + (ms % 10));
    out[12] = '\0';
}

// Asynchronous stderr writer so Roblox render, simulation and worker threads
// never stall on a synchronous unbuffered `write(2)` to a slow pipe/terminal.
// Uses a fixed pre-allocated ring of 512-byte slots for zero heap allocations
// on hot log bursts. Errors and fatals flush synchronously so crash output is
// never lost.
struct AsyncLogWriter {
    static constexpr size_t kMaxQueued = 512;
    static constexpr size_t kSlotBytes = 512;
    struct Slot {
        uint16_t len = 0;
        char data[kSlotBytes];
    };

    std::mutex mtx;
    std::condition_variable cv;
    Slot ring[kMaxQueued]{};
    size_t head = 0;
    size_t tail = 0;
    size_t count = 0;
    size_t dropped = 0;

    AsyncLogWriter() {
        std::thread([this]() { run(); }).detach();
    }

    void run() {
        auto batch = std::make_unique<Slot[]>(kMaxQueued);
        for (;;) {
            size_t batch_n = 0;
            size_t dropped_now = 0;
            {
                std::unique_lock<std::mutex> lk(mtx);
                cv.wait(lk, [&]() { return count > 0 || dropped > 0; });
                batch_n = count;
                for (size_t i = 0; i < batch_n; ++i) {
                    const Slot& src = ring[(head + i) & (kMaxQueued - 1)];
                    batch[i].len = src.len;
                    std::memcpy(batch[i].data, src.data, src.len);
                }
                head = (head + batch_n) & (kMaxQueued - 1);
                count = 0;
                dropped_now = dropped;
                dropped = 0;
            }
            for (size_t i = 0; i < batch_n; ++i) {
                fwrite(batch[i].data, 1, batch[i].len, stderr);
            }
            if (dropped_now > 0) {
                fprintf(stderr, "[liblog] dropped %zu log line(s) while stderr was backed up\n",
                        dropped_now);
            }
        }
    }

    void push(const char* src, size_t n) {
        if (n == 0) {
            return;
        }
        bool truncated = false;
        if (n > kSlotBytes) {
            n = kSlotBytes;
            truncated = true;
        }
        bool wake = false;
        {
            std::lock_guard<std::mutex> lk(mtx);
            if (count >= kMaxQueued) {
                ++dropped;
                return;
            }
            Slot& dst = ring[tail];
            std::memcpy(dst.data, src, n);
            if (truncated) {
                dst.data[kSlotBytes - 1] = '\n';
            }
            dst.len = static_cast<uint16_t>(n);
            tail = (tail + 1) & (kMaxQueued - 1);
            wake = (count == 0);
            ++count;
        }
        if (wake) {
            cv.notify_one();
        }
    }
};

static AsyncLogWriter& async_writer() {
    static AsyncLogWriter* w = new AsyncLogWriter();
    return *w;
}

void emit(int prio, const char* tag, const char* text) {
    if (prio < minimum_priority()) {
        return;
    }
    char ts[16];
    stamp(ts, sizeof(ts));
    const char* safe_tag = tag ? tag : "(no tag)";
    const char* safe_text = text ? text : "(null)";
    if (prio >= ANDROID_LOG_ERROR) {
        fprintf(stderr, "%s %c/%-24s %s\n", ts, priority_letter(prio), safe_tag, safe_text);
        return;
    }
    char stack_buf[AsyncLogWriter::kSlotBytes];
    int n = snprintf(stack_buf, sizeof(stack_buf), "%s %c/%-24s %s\n",
                     ts, priority_letter(prio), safe_tag, safe_text);
    if (n <= 0) {
        return;
    }
    size_t len = static_cast<size_t>(n);
    if (len >= sizeof(stack_buf)) {
        len = sizeof(stack_buf) - 1;
        stack_buf[len - 1] = '\n';
    }
    async_writer().push(stack_buf, len);
}

int __android_log_write(int prio, const char* tag, const char* text) {
    if (prio < minimum_priority()) {
        return 1;
    }
    emit(prio, tag, text);
    return 1;
}

int __android_log_buf_write(int /*bufID*/, int prio, const char* tag, const char* text) {
    if (prio < minimum_priority()) {
        return 1;
    }
    // Android routes to main/radio/system/crash buffers. One stream is fine here;
    // nothing downstream distinguishes them.
    emit(prio, tag, text);
    return 1;
}

int __android_log_vprint(int prio, const char* tag, const char* fmt, va_list ap) {
    // Check the priority gate before formatting: VERBOSE is off by default and
    // chatty, so formatting first wastes 4 KiB stack + full vsnprintf on every
    // discarded log call on engine threads.
    if (prio < minimum_priority()) {
        return 1;
    }
    char buf[4096];
    vsnprintf(buf, sizeof(buf), fmt, ap);
    emit(prio, tag, buf);
    return 1;
}

int __android_log_print(int prio, const char* tag, const char* fmt, ...) {
    if (prio < minimum_priority()) {
        return 1;
    }
    va_list ap;
    va_start(ap, fmt);
    int rc = __android_log_vprint(prio, tag, fmt, ap);
    va_end(ap);
    return rc;
}

/// bionic's fatal-assertion path. It does not return, and it must not: as a stub
/// returning 0 a failed assertion continues with state its author already
/// declared invalid, and the eventual crash points somewhere unrelated.
void __android_log_assert(const char* cond, const char* tag, const char* fmt, ...) {
    char buf[4096];
    if (fmt) {
        va_list ap;
        va_start(ap, fmt);
        vsnprintf(buf, sizeof(buf), fmt, ap);
        va_end(ap);
    } else {
        snprintf(buf, sizeof(buf), "assertion failed: %s", cond ? cond : "(unknown)");
    }
    fprintf(stderr, "\n*** Roblox fatal assertion ***\n");
    fprintf(stderr, "    tag:       %s\n", tag ? tag : "(no tag)");
    fprintf(stderr, "    condition: %s\n", cond ? cond : "(none)");
    fprintf(stderr, "    message:   %s\n", buf);
    abort();
}

/// Where bionic stashes a message for the crash reporter to pick up. There is no
/// crash reporter here, so printing it is strictly more useful.
void android_set_abort_message(const char* msg) {
    fprintf(stderr, "[abort-message] %s\n", msg ? msg : "(null)");
}

} // namespace

/// Table of everything above, for the Rust symbol table to install.
///
/// The functions have internal linkage on purpose. Vanta links the AOSP bionic
/// linker into the same binary and it defines `__android_log_write` and
/// `android_set_abort_message` for its own logging; exporting a second copy is a
/// duplicate-symbol error. Roblox never resolves these by name through the host
/// linker anyway — they reach it through this table and Vanta's virtual
/// `liblog.so`, which is the only path that matters.
extern "C" struct VantaSymbol {
    const char* name;
    void* addr;
};

extern "C" const VantaSymbol* vanta_liblog_symbols(size_t* count) {
    static const VantaSymbol table[] = {
        {"__android_log_write", (void*)&__android_log_write},
        {"__android_log_buf_write", (void*)&__android_log_buf_write},
        {"__android_log_print", (void*)&__android_log_print},
        {"__android_log_vprint", (void*)&__android_log_vprint},
        {"__android_log_assert", (void*)&__android_log_assert},
        {"android_set_abort_message", (void*)&android_set_abort_message},
    };
    *count = sizeof(table) / sizeof(table[0]);
    return table;
}
