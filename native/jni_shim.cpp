// extern "C" surface over libjnivm, so Rust can stand up a JavaVM.
//
// As with shim.cpp: translation only, no policy.

#include <jnivm.h>

extern "C" void vanta_register_android_classes(void* env);
namespace vanta {
void register_game_activity_classes(jnivm::ENV* env);
void register_init_params_classes(jnivm::ENV* env);
}

/// The process VM, for translation units that need the real jnivm::ENV rather
/// than a JNIEnv. Keeping this on the C++ side means Rust never has to hold a
/// type it cannot name — passing a JNIEnv* and casting it to jnivm::ENV* is a
/// silent type confusion that surfaces as a null function pointer several calls
/// later.
namespace vanta { jnivm::ENV* process_env(); }

#include <cstddef>
#include <cstdio>
#include <cstdlib>
#include <exception>
#include <execinfo.h>
#include <unistd.h>
#include <stdexcept>
#include <memory>

namespace {
JNIInvokeInterface g_traced_iface;
JavaVM g_traced_vm;
JNIInvokeInterface g_real_iface;
JavaVM* g_real_vm = nullptr;
bool g_trace_invoke = false;

class ProcessVM final : public jnivm::VM {
public:
    ProcessVM() : VM(true) {
        // libjnivm also takes the VM mutex to read this immutable association.
        // Hand out `&g_traced_vm` so JNIEnv::GetJavaVM stays on the same
        // wrapper table as JNI_OnLoad: DetachCurrentThread invalidates `t_env`
        // and libjnivm's own `g_real_vm` (and its `reserved0` pointer) stays
        // private to this file.
        AddHook([](JNINativeInterface& iface) {
            iface.GetJavaVM = [](JNIEnv*, JavaVM** out) -> jint {
                if (out) {
                    *out = &g_traced_vm;
                }
                return JNI_OK;
            };
        });
        initialize();
    }
};

// The VM owns every class, object and environment it hands out, so it outlives
// anything derived from it. One per process.
std::unique_ptr<ProcessVM> g_vm;

/// Report an uncaught C++ exception with its stack instead of dying mute.
///
/// Roblox spawns worker threads during `JNI_OnLoad` and they call straight back
/// into JNI. libjnivm reports misuse by throwing, and an exception escaping a
/// thread Vanta did not start cannot be caught anywhere — the default is
/// `std::terminate` and a core dump carrying no information about which thread,
/// which call, or why.
/// Write the observed Java surface out, if a VM exists and a path was given.
void dump_classes_now() {
#ifdef JNI_DEBUG
    const char* path = getenv("VANTA_JNI_DUMP");
    if (path && g_vm) {
        try {
            g_vm->GenerateClassDump(path);
            fprintf(stderr, "    Java surface Roblox reached for -> %s\n", path);
        } catch (...) {
            fprintf(stderr, "    class dump failed\n");
        }
    }
#endif
}

[[noreturn]] void report_terminate() {
    fprintf(stderr, "\n*** uncaught C++ exception on thread %ld ***\n", (long)gettid());
    if (auto e = std::current_exception()) {
        try {
            std::rethrow_exception(e);
        } catch (const std::exception& ex) {
            fprintf(stderr, "    what(): %s\n", ex.what());
        } catch (...) {
            fprintf(stderr, "    non-standard exception\n");
        }
    }
    void* frames[32];
    int n = backtrace(frames, 32);
    fprintf(stderr, "    %d frames:\n", n);
    backtrace_symbols_fd(frames, n, 2);

    // Dump before dying. Everything Roblox asked Java for up to this point is
    // the most valuable thing in the process, and it is about to be lost — the
    // failure happens on a thread nobody can catch, so there is no later.
    dump_classes_now();
    _exit(70);
}
} // namespace

namespace {

// Even an already-attached thread takes libjnivm's VM-wide mutex to look
// itself up. The VM still owns the env; detach on this same thread invalidates
// the cache through the canonical invocation table, also used by GetJavaVM.
thread_local JNIEnv* t_env = nullptr;

// Always delegate with `g_real_vm` rather than the caller's `vm` argument:
// libjnivm recovers its `jnivm::VM*` from `vm->functions->reserved0` and throws
// "Failed to get reference to jnivm::VM" if a caller passes a wrapped/null
// `JavaVM*` or one whose `reserved0` slot is null (observed inside Roblox's
// `JNI_OnLoad` on worker thread startup after commit ceb796f stopped routing
// through `g_real_vm`).
jint attach(JNIEnv** out, void* args) {
    if (t_env && out) {
        *out = t_env;
        return JNI_OK;
    }
    if (!g_real_vm) {
        return JNI_ERR;
    }
    JNIEnv* env = nullptr;
    const jint rc = g_real_iface.AttachCurrentThread(g_real_vm, out ? &env : nullptr, args);
    if (rc == JNI_OK && out) {
        t_env = *out = env;
    }
    return rc;
}

void note(const char* what, JavaVM* vm) {
    if (g_trace_invoke) {
        fprintf(stderr, "[jni] %s(vm=%p)%s\n", what, (void*)vm,
                vm == &g_traced_vm ? " [ours]" : " [NOT ours]");
    }
}

jint traced_destroy(JavaVM* vm) {
    note("DestroyJavaVM", vm);
    return g_real_vm ? g_real_iface.DestroyJavaVM(g_real_vm) : JNI_ERR;
}
jint traced_attach(JavaVM* vm, JNIEnv** env, void* args) {
    note("AttachCurrentThread", vm);
    return attach(env, args);
}
jint traced_attach_daemon(JavaVM* vm, JNIEnv** env, void* args) {
    note("AttachCurrentThreadAsDaemon", vm);
    // libjnivm implements daemon attachment as ordinary attachment.
    return attach(env, args);
}
jint traced_detach(JavaVM* vm) {
    note("DetachCurrentThread", vm);
    const jint rc = g_real_vm ? g_real_iface.DetachCurrentThread(g_real_vm) : JNI_ERR;
    if (rc == JNI_OK) {
        t_env = nullptr;
    }
    return rc;
}
jint traced_get_env(JavaVM* vm, void** env, jint version) {
    note("GetEnv", vm);
    if (!env) {
        return g_real_vm ? g_real_iface.GetEnv(g_real_vm, nullptr, version) : JNI_ERR;
    }
    // libjnivm ignores the requested version and attaches on GetEnv. Keep
    // that behaviour rather than changing the engine's attachment contract.
    JNIEnv* attached = nullptr;
    const jint rc = attach(&attached, nullptr);
    if (rc == JNI_OK) {
        *env = attached;
    }
    return rc;
}

} // namespace

extern "C" {

/// Create the process's JavaVM. Returns the `JavaVM*` Roblox expects in
/// `JNI_OnLoad`, or null if one already exists.
void* vanta_jni_create_vm() {
    if (g_vm) {
        return nullptr;
    }
    std::set_terminate(report_terminate);
    g_vm = std::make_unique<ProcessVM>();
    g_real_vm = g_vm->GetJavaVM();
    g_real_iface = *g_real_vm->functions;

    // Keep libjnivm's own JavaVM private and hand out `&g_traced_vm` from both
    // `JNI_OnLoad` and `JNIEnv::GetJavaVM`, so `g_real_vm->functions->reserved0`
    // can never be clobbered and every invocation still flows through the TLS
    // cache and invalidation hooks above.
    g_traced_iface = g_real_iface;
    g_traced_iface.DestroyJavaVM = traced_destroy;
    g_traced_iface.AttachCurrentThread = traced_attach;
    g_traced_iface.AttachCurrentThreadAsDaemon = traced_attach_daemon;
    g_traced_iface.DetachCurrentThread = traced_detach;
    g_traced_iface.GetEnv = traced_get_env;
    g_traced_vm.functions = &g_traced_iface;
    g_trace_invoke = getenv("VANTA_JNI_TRACE") != nullptr;

    // Vanta's Java side, before Roblox can ask for any of it.
    vanta_register_android_classes(g_vm->GetEnv().get());
    vanta::register_game_activity_classes(g_vm->GetEnv().get());
    vanta::register_init_params_classes(g_vm->GetEnv().get());

    JavaVM* vm = &g_traced_vm;
    // libjnivm recovers its VM from JavaVM::functions->reserved0. If that is not
    // set, every callback Roblox makes throws before it does anything.
    fprintf(stderr, "[jni] JavaVM=%p functions=%p reserved0=%p (expect %p)\n",
            (void*)vm, (void*)(vm ? vm->functions : nullptr),
            vm && vm->functions ? vm->functions->reserved0 : nullptr,
            (void*)g_vm.get());
    return vm;
}

/// Write C++ stubs for every Java class and method the native code has reached
/// for so far. This is the Phase 2 backlog, observed rather than guessed.
int vanta_jni_dump_classes(const char* path) {
#ifdef JNI_DEBUG
    if (!g_vm) {
        return -1;
    }
    g_vm->GenerateClassDump(path);
    return 0;
#else
    (void)path;
    return -2;  // built without JNI_DEBUG
#endif
}

/// The calling thread's JNIEnv, attaching the thread if it has none -- the
/// same route `vanta::process_env` takes, for the same reason. The arm64
/// guest's JNI stubs (crates/vanta-runtime/src/guest_jni.rs) hand every
/// call to libjnivm with this env in place of the guest's own.
void* vanta_jni_thread_env() {
    if (!g_real_vm) {
        return nullptr;
    }
    JNIEnv* env = nullptr;
    if (attach(&env, nullptr) != JNI_OK) {
        return nullptr;
    }
    return env;
}

/// The current thread's `JNIEnv*`.
void* vanta_jni_env() {
    return vanta_jni_thread_env();
}

/// A jmethodID's JNI descriptor. libjnivm's method IDs are its `Method`
/// objects (`(jmethodID)this` throughout method.cpp), and the descriptor is
/// the only thing that says what a variadic Call*Method's arguments are.
const char* vanta_jni_method_signature(void* method) {
    return method ? static_cast<jnivm::Method*>(method)->signature.c_str() : nullptr;
}

/// A jmethodID's name, beside its descriptor, for the guest's call log: the
/// descriptor alone cannot say which Java method a Play press reached.
const char* vanta_jni_method_name(void* method) {
    return method ? static_cast<jnivm::Method*>(method)->name.c_str() : nullptr;
}

/// Call `JNI_OnLoad` with the process JavaVM, containing any C++ exception.
///
/// libjnivm reports misuse by throwing. Those exceptions originate inside
/// Roblox's call stack and would otherwise cross the Rust FFI boundary, where
/// the only outcome is `std::terminate` and a core dump — which says nothing
/// about what went wrong. Catching here turns that into a message.
///
/// Returns the JNI version on success, or one of the negative codes below.
int vanta_jni_call_onload(void* fn, char* err, size_t err_len) {
    using OnLoad = jint (*)(JavaVM*, void*);
    if (!fn || !g_vm) {
        return -1;
    }
    try {
        return reinterpret_cast<OnLoad>(fn)(&g_traced_vm, nullptr);
    } catch (const std::exception& e) {
        snprintf(err, err_len, "%s", e.what());
        return -2;
    } catch (...) {
        snprintf(err, err_len, "non-standard C++ exception");
        return -3;
    }
}

} // extern "C"

namespace vanta {
jnivm::ENV* process_env() {
    // GetEnv() on the C++ VM would insert a null map entry on a worker that
    // has never called JNI. The shared route attaches before looking it up.
    auto* env = static_cast<JNIEnv*>(vanta_jni_thread_env());
    return env ? jnivm::ENV::FromJNIEnv(env) : nullptr;
}
} // namespace vanta
