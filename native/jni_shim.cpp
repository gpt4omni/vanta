// extern "C" surface over libjnivm, so Rust can stand up a JavaVM.
//
// As with shim.cpp: translation only, no policy.

#include <jnivm.h>
#include <jnivm/weak.h>
#include <jnivm/bytebuffer.h>
#include <jnivm/internal/skipJNIType.h>

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

#include <algorithm>
#include <array>
#include <atomic>
#include <cstdarg>
#include <cstddef>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <exception>
#include <execinfo.h>
#include <unistd.h>
#include <stdexcept>
#include <memory>
#include <string>
#include <unordered_map>
#include <vector>

namespace {
JNIInvokeInterface g_traced_iface;
JavaVM g_traced_vm;
JNIInvokeInterface g_real_iface;
JavaVM* g_real_vm = nullptr;
bool g_trace_invoke = false;

// ---------------------------------------------------------------------------
// Fast JNI Rocket Layer (zero third_party/libjnivm edits, wired via AddHook).
//
// Eliminates libjnivm's per-call overhead on the hot path:
//  1. 2-instruction vptr unwrap (replaces 3-9 dynamic_casts + shared_ptr churn)
//  2. O(1) reverse-scan swap-and-pop DeleteLocalRef & DeleteGlobalRef
//  3. Leak-free single-alloc New*Array (fixes libjnivm's `new T[n]{0}` leak)
//  4. Zero-heap stack jvalue[16] Call*Method/V/A + CallStatic*Method/V/A
//     without leaking a Class local ref on every single virtual call
//  5. Lock-free TLS direct-mapped caches for FindClass, Get*MethodID,
//     Get*FieldID, and virtual overload dispatch.
// Disable via VANTA_NO_JNI_FAST=1 for A/B comparison.
// ---------------------------------------------------------------------------

JNINativeInterface g_orig_jni{};
std::atomic<uint64_t> g_vm_gen{0};

const void* g_vptr_object = nullptr;
const void* g_vptr_global = nullptr;
const void* g_vptr_weak = nullptr;
ptrdiff_t g_global_obj_delta = 0;
ptrdiff_t g_weak_obj_delta = 0;
const void* g_vptr_string = nullptr;
const void* g_vptr_class = nullptr;
const void* g_vptr_bytebuf = nullptr;
const void* g_vptr_arr_obj = nullptr;
ptrdiff_t g_arr_obj_delta = 0;
const void* g_vptr_arr_bool = nullptr;
const void* g_vptr_arr_byte = nullptr;
const void* g_vptr_arr_char = nullptr;
const void* g_vptr_arr_short = nullptr;
const void* g_vptr_arr_int = nullptr;
const void* g_vptr_arr_long = nullptr;
const void* g_vptr_arr_float = nullptr;
const void* g_vptr_arr_double = nullptr;

std::shared_ptr<jnivm::Class> g_cls_object;
std::shared_ptr<jnivm::Class> g_cls_global;
std::shared_ptr<jnivm::Class> g_cls_weak;
std::shared_ptr<jnivm::Class> g_cls_throwable;
std::shared_ptr<jnivm::Class> g_cls_arr_bool;
std::shared_ptr<jnivm::Class> g_cls_arr_byte;
std::shared_ptr<jnivm::Class> g_cls_arr_char;
std::shared_ptr<jnivm::Class> g_cls_arr_short;
std::shared_ptr<jnivm::Class> g_cls_arr_int;
std::shared_ptr<jnivm::Class> g_cls_arr_long;
std::shared_ptr<jnivm::Class> g_cls_arr_float;
std::shared_ptr<jnivm::Class> g_cls_arr_double;

// O(1) index map for `nvm->globals` (guarded by `nvm->mtx`).
std::unordered_map<jnivm::Object*, size_t> g_global_index;

/// Inline equivalent of `jnivm::ENV::FromJNIEnv(env)` (verified in
/// `third_party/libjnivm/src/jnivm/env.cpp:38-41`), avoiding an out-of-line
/// call and exception-setup prologue on every JNI entrypoint.
static inline jnivm::ENV* fast_env(JNIEnv* env) noexcept {
    return static_cast<jnivm::ENV*>(env->functions->reserved0);
}

template <class T>
static const void* vptr_of(const T* p) noexcept {
    return *reinterpret_cast<const void* const*>(p);
}

static void init_fast_vptrs() {
    jnivm::Object o;
    jnivm::Global g;
    jnivm::Weak w;
    jnivm::String s;
    jnivm::Class c;
    jnivm::ByteBuffer bb(nullptr, 0);
    jnivm::Array<jnivm::Object> ao;
    jnivm::Array<jboolean> az;
    jnivm::Array<jbyte> ab;
    jnivm::Array<jchar> ac;
    jnivm::Array<jshort> as;
    jnivm::Array<jint> ai;
    jnivm::Array<jlong> aj;
    jnivm::Array<jfloat> af;
    jnivm::Array<jdouble> ad;

    jnivm::Object* g_obj = &g;
    jnivm::Object* w_obj = &w;
    g_vptr_object = vptr_of(&o);
    g_vptr_global = vptr_of(g_obj);
    g_vptr_weak = vptr_of(w_obj);
    g_global_obj_delta = reinterpret_cast<const char*>(&g) - reinterpret_cast<const char*>(g_obj);
    g_weak_obj_delta = reinterpret_cast<const char*>(&w) - reinterpret_cast<const char*>(w_obj);
    g_vptr_string = vptr_of(static_cast<jnivm::Object*>(&s));
    g_vptr_class = vptr_of(static_cast<jnivm::Object*>(&c));
    g_vptr_bytebuf = vptr_of(static_cast<jnivm::Object*>(&bb));
    jnivm::Object* ao_obj = static_cast<jnivm::Object*>(&ao);
    g_vptr_arr_obj = vptr_of(ao_obj);
    g_arr_obj_delta = reinterpret_cast<const char*>(&ao) - reinterpret_cast<const char*>(ao_obj);
    g_vptr_arr_bool = vptr_of(static_cast<jnivm::Object*>(&az));
    g_vptr_arr_byte = vptr_of(static_cast<jnivm::Object*>(&ab));
    g_vptr_arr_char = vptr_of(static_cast<jnivm::Object*>(&ac));
    g_vptr_arr_short = vptr_of(static_cast<jnivm::Object*>(&as));
    g_vptr_arr_int = vptr_of(static_cast<jnivm::Object*>(&ai));
    g_vptr_arr_long = vptr_of(static_cast<jnivm::Object*>(&aj));
    g_vptr_arr_float = vptr_of(static_cast<jnivm::Object*>(&af));
    g_vptr_arr_double = vptr_of(static_cast<jnivm::Object*>(&ad));
}

static void init_fast_classes(jnivm::ENV* env) {
    g_cls_object = env->GetClass("java/lang/Object");
    g_cls_global = env->GetClass("internal/lang/Global");
    g_cls_weak = env->GetClass("java/lang/ref/WeakReference");
    g_cls_throwable = env->GetClass("java/lang/Throwable");
    g_cls_arr_bool = env->GetClass("[Z");
    g_cls_arr_byte = env->GetClass("[B");
    g_cls_arr_char = env->GetClass("[C");
    g_cls_arr_short = env->GetClass("[S");
    g_cls_arr_int = env->GetClass("[I");
    g_cls_arr_long = env->GetClass("[J");
    g_cls_arr_float = env->GetClass("[F");
    g_cls_arr_double = env->GetClass("[D");
}

struct FastObj {
    jnivm::Object* ptr = nullptr;
    /// Populated ONLY when unwrapping a `jnivm::Weak` ref so the strong
    /// reference stays alive for the entire JNI call (preventing UAF).
    /// Null for local and global refs (zero atomic refcount overhead).
    std::shared_ptr<jnivm::Object> pin;
    explicit operator bool() const noexcept { return ptr != nullptr; }
};

struct FastClass {
    jnivm::Class* ptr = nullptr;
    std::shared_ptr<jnivm::Object> pin;
    explicit operator bool() const noexcept { return ptr != nullptr; }
};

static inline FastObj fast_unwrap(jobject o) noexcept {
    if (!o) {
        return {};
    }
    auto* obj = reinterpret_cast<jnivm::Object*>(o);
    const void* vp = vptr_of(obj);
    if (__builtin_expect(vp != g_vptr_global && vp != g_vptr_weak, 1)) {
        return {obj, {}};
    }
    if (vp == g_vptr_global) {
        auto* g = reinterpret_cast<jnivm::Global*>(reinterpret_cast<char*>(obj) + g_global_obj_delta);
        return {g->wrapped.get(), {}};
    }
    auto* w = reinterpret_cast<jnivm::Weak*>(reinterpret_cast<char*>(obj) + g_weak_obj_delta);
    auto sp = w->wrapped.lock();
    jnivm::Object* raw = sp.get();
    return {raw, std::move(sp)};
}

static inline FastClass fast_unwrap_class(jclass c) noexcept {
    FastObj obj = fast_unwrap(c);
    if (!obj.ptr) {
        return {};
    }
    if (__builtin_expect(vptr_of(obj.ptr) == g_vptr_class, 1)) {
        return {static_cast<jnivm::Class*>(obj.ptr), std::move(obj.pin)};
    }
    auto* cp = dynamic_cast<jnivm::Class*>(obj.ptr);
    if (!cp) {
        return {};
    }
    return {cp, std::move(obj.pin)};
}

static inline void push_local(jnivm::ENV* nenv, const std::shared_ptr<jnivm::Object>& obj) {
#ifdef EnableJNIVMGC
    if (obj) {
        nenv->localframe.front().emplace_back(obj);
    }
#else
    (void)nenv;
    (void)obj;
#endif
}

static inline void push_local(jnivm::ENV* nenv, std::shared_ptr<jnivm::Object>&& obj) {
#ifdef EnableJNIVMGC
    if (obj) {
        nenv->localframe.front().emplace_back(std::move(obj));
    }
#else
    (void)nenv;
    (void)obj;
#endif
}

// Fast FNV-1a 64-bit hash for small JNI names/signatures.
static inline uint64_t fnv1a(const char* s, size_t& out_len) noexcept {
    uint64_t h = 14695981039346656037ULL;
    size_t n = 0;
    while (unsigned char c = static_cast<unsigned char>(s[n])) {
        h ^= c;
        h *= 1099511628211ULL;
        ++n;
    }
    out_len = n;
    return h;
}

// ---- Local / Global Reference & Exception / Frame fast paths --------------

static void fast_delete_local_ref(JNIEnv* env, jobject obj) {
#ifdef EnableJNIVMGC
    if (!obj) {
        return;
    }
    // Reject Global/Weak refs: DeleteLocalRef must never unwrap and delete a
    // local ref belonging to the same underlying object.
    auto* raw = reinterpret_cast<jnivm::Object*>(obj);
    const void* vp = vptr_of(raw);
    if (__builtin_expect(vp == g_vptr_global || vp == g_vptr_weak, 0)) {
        return;
    }
    auto* nenv = fast_env(env);
    if (nenv->localframe.empty()) {
        return;
    }
    // ONLY inspect the innermost local frame (`front()`), never parent frames.
    auto& frame = nenv->localframe.front();
    for (size_t i = frame.size(); i-- > 0;) {
        if (frame[i].get() == raw) {
            if (i + 1 < frame.size()) {
                frame[i] = std::move(frame.back());
            }
            frame.pop_back();
            return;
        }
    }
#else
    (void)env;
    (void)obj;
#endif
}

static inline void insert_global_locked(jnivm::VM* nvm, std::shared_ptr<jnivm::Object>&& obj) {
#ifdef EnableJNIVMGC
    jnivm::Object* key = obj.get();
    const size_t idx = nvm->globals.size();
    nvm->globals.emplace_back(std::move(obj));
    g_global_index[key] = idx;
#else
    (void)nvm;
    (void)obj;
#endif
}

static inline void remove_global_locked(jnivm::VM* nvm, jnivm::Object* raw) {
#ifdef EnableJNIVMGC
    auto& g = nvm->globals;
    auto it = g_global_index.find(raw);
    if (__builtin_expect(it != g_global_index.end() && it->second < g.size() && g[it->second].get() == raw, 1)) {
        const size_t idx = it->second;
        g_global_index.erase(it);
        const size_t last = g.size() - 1;
        if (idx != last) {
            g[idx] = std::move(g[last]);
            g_global_index[g[idx].get()] = idx;
        }
        g.pop_back();
        return;
    }
    if (it != g_global_index.end()) {
        g_global_index.erase(it);
    }
    // Defensive fallback if an entry was inserted/removed outside the fast path.
    for (size_t i = g.size(); i-- > 0;) {
        if (g[i].get() == raw) {
            const size_t last = g.size() - 1;
            if (i != last) {
                g[i] = std::move(g[last]);
                g_global_index[g[i].get()] = i;
            }
            g.pop_back();
            return;
        }
    }
#else
    (void)nvm;
    (void)raw;
#endif
}

static jobject fast_new_global_ref(JNIEnv* env, jobject obj) {
    if (!obj) {
        return nullptr;
    }
    FastObj unwrapped = fast_unwrap(obj);
    if (!unwrapped.ptr) {
        return nullptr;
    }
    auto strong = unwrapped.pin ? std::move(unwrapped.pin) : unwrapped.ptr->weak_from_this().lock();
    if (!strong) {
        return g_orig_jni.NewGlobalRef(env, obj);
    }
    auto global = std::make_shared<jnivm::Global>();
    global->wrapped = std::move(strong);
    global->clazz = g_cls_global;
    jnivm::Object* global_obj = static_cast<jnivm::Object*>(global.get());
#ifdef EnableJNIVMGC
    auto* nvm = fast_env(env)->GetVM();
    {
        std::lock_guard<std::mutex> guard(nvm->mtx);
        insert_global_locked(nvm, std::move(global));
    }
#endif
    return reinterpret_cast<jobject>(global_obj);
}

static void fast_delete_global_ref(JNIEnv* env, jobject obj) {
#ifdef EnableJNIVMGC
    if (!obj) {
        return;
    }
    auto* raw = reinterpret_cast<jnivm::Object*>(obj);
    auto* nvm = fast_env(env)->GetVM();
    std::lock_guard<std::mutex> guard(nvm->mtx);
    remove_global_locked(nvm, raw);
#else
    (void)env;
    (void)obj;
#endif
}

static jweak fast_new_weak_global_ref(JNIEnv* env, jobject obj) {
    if (!obj) {
        return nullptr;
    }
    FastObj unwrapped = fast_unwrap(obj);
    if (!unwrapped.ptr) {
        return nullptr;
    }
    auto strong = unwrapped.pin ? std::move(unwrapped.pin) : unwrapped.ptr->weak_from_this().lock();
    if (!strong) {
        return g_orig_jni.NewWeakGlobalRef(env, obj);
    }
    auto weak = std::make_shared<jnivm::Weak>();
    weak->wrapped = std::move(strong);
    weak->clazz = g_cls_weak;
    jnivm::Object* weak_obj = static_cast<jnivm::Object*>(weak.get());
#ifdef EnableJNIVMGC
    auto* nvm = fast_env(env)->GetVM();
    {
        std::lock_guard<std::mutex> guard(nvm->mtx);
        insert_global_locked(nvm, std::move(weak));
    }
#endif
    return reinterpret_cast<jweak>(weak_obj);
}

static void fast_delete_weak_global_ref(JNIEnv* env, jweak obj) {
#ifdef EnableJNIVMGC
    if (!obj) {
        return;
    }
    auto* raw = reinterpret_cast<jnivm::Object*>(obj);
    auto* nvm = fast_env(env)->GetVM();
    std::lock_guard<std::mutex> guard(nvm->mtx);
    remove_global_locked(nvm, raw);
#else
    (void)env;
    (void)obj;
#endif
}

static jobject fast_new_local_ref(JNIEnv* env, jobject obj) {
    if (!obj) {
        return nullptr;
    }
    FastObj unwrapped = fast_unwrap(obj);
    if (!unwrapped.ptr) {
        return nullptr;
    }
    auto strong = unwrapped.pin ? std::move(unwrapped.pin) : unwrapped.ptr->weak_from_this().lock();
    if (!strong) {
        return g_orig_jni.NewLocalRef(env, obj);
    }
    push_local(fast_env(env), strong);
    return reinterpret_cast<jobject>(strong.get());
}

static jboolean fast_is_same_object(JNIEnv*, jobject l, jobject r) {
    if (l == r) {
        return JNI_TRUE;
    }
    FastObj lo = fast_unwrap(l);
    FastObj ro = fast_unwrap(r);
    return lo.ptr == ro.ptr ? JNI_TRUE : JNI_FALSE;
}

static jclass fast_get_object_class(JNIEnv* env, jobject obj) {
    FastObj raw = fast_unwrap(obj);
    if (!raw.ptr) {
        return nullptr;
    }
    auto cl = raw.ptr->clazz.lock();
    if (!cl) {
        return g_orig_jni.GetObjectClass(env, obj);
    }
    push_local(fast_env(env), cl);
    return reinterpret_cast<jclass>(cl.get());
}

static jboolean fast_exception_check(JNIEnv* env) {
    return fast_env(env)->current_exception ? JNI_TRUE : JNI_FALSE;
}

static jthrowable fast_exception_occurred(JNIEnv* env) {
    auto* nenv = fast_env(env);
    auto ex = nenv->current_exception;
    if (!ex) {
        return nullptr;
    }
    if (ex->clazz.expired()) {
        ex->clazz = g_cls_throwable;
    }
    push_local(nenv, ex);
    return reinterpret_cast<jthrowable>(ex.get());
}

static void fast_exception_clear(JNIEnv* env) {
    fast_env(env)->current_exception.reset();
}

static jint fast_push_local_frame(JNIEnv* env, jint cap) {
#ifdef EnableJNIVMGC
    auto* nenv = fast_env(env);
    if (nenv->freeframes.empty()) {
        nenv->localframe.emplace_front();
    } else {
        nenv->localframe.splice_after(
            nenv->localframe.before_begin(), nenv->freeframes, nenv->freeframes.before_begin());
    }
    if (cap > 0) {
        auto& front = nenv->localframe.front();
        if (front.capacity() < static_cast<size_t>(cap)) {
            front.reserve(static_cast<size_t>(cap));
        }
    }
#else
    (void)env;
    (void)cap;
#endif
    return JNI_OK;
}

static jobject fast_pop_local_frame(JNIEnv* env, jobject result) {
#ifdef EnableJNIVMGC
    auto* nenv = fast_env(env);
    std::shared_ptr<jnivm::Object> res;
    if (__builtin_expect(result != nullptr, 0)) {
        FastObj raw = fast_unwrap(result);
        if (raw.ptr) {
            res = raw.pin ? std::move(raw.pin) : raw.ptr->weak_from_this().lock();
        }
    }
    if (!nenv->localframe.empty()) {
        nenv->localframe.front().clear();
        nenv->freeframes.splice_after(
            nenv->freeframes.before_begin(), nenv->localframe, nenv->localframe.before_begin());
    }
    if (__builtin_expect(nenv->localframe.empty(), 0)) {
        nenv->localframe.emplace_front();
    }
    if (__builtin_expect(res != nullptr, 0)) {
        push_local(nenv, res);
        return reinterpret_cast<jobject>(res.get());
    }
    return nullptr;
#else
    (void)env;
    return result;
#endif
}

static jint fast_ensure_local_capacity(JNIEnv* env, jint cap) {
#ifdef EnableJNIVMGC
    if (cap > 0) {
        auto* nenv = fast_env(env);
        if (!nenv->localframe.empty()) {
            auto& front = nenv->localframe.front();
            if (front.capacity() < static_cast<size_t>(cap)) {
                front.reserve(static_cast<size_t>(cap));
            }
        }
    }
#else
    (void)env;
    (void)cap;
#endif
    return JNI_OK;
}

static jobjectRefType fast_get_object_ref_type(JNIEnv*, jobject obj) {
    if (!obj) {
        return JNIInvalidRefType;
    }
    const void* vp = vptr_of(reinterpret_cast<jnivm::Object*>(obj));
    if (__builtin_expect(vp != g_vptr_global && vp != g_vptr_weak, 1)) {
        return JNILocalRefType;
    }
    return vp == g_vptr_global ? JNIGlobalRefType : JNIWeakGlobalRefType;
}

static bool fast_has_base_class(jnivm::ENV* nenv, jnivm::Class* cl, jnivm::Class* target) {
    if (cl == target) {
        return true;
    }
    if (!cl || !target) {
        return false;
    }
    if (cl->baseclasses) {
        for (auto&& base : cl->baseclasses(nenv)) {
            if (base && fast_has_base_class(nenv, base.get(), target)) {
                return true;
            }
        }
    }
    return false;
}

static jboolean fast_is_instance_of(JNIEnv* env, jobject jo, jclass cl) {
    if (!jo || !cl) {
        return JNI_FALSE;
    }
    FastObj raw = fast_unwrap(jo);
    FastClass target = fast_unwrap_class(cl);
    if (!raw.ptr || !target.ptr) {
        return JNI_FALSE;
    }
    auto* nenv = fast_env(env);
    auto obj_cls = raw.ptr->clazz.lock();
    if (!obj_cls) {
        obj_cls = raw.ptr->getClassInternal(nenv);
    }
    return fast_has_base_class(nenv, obj_cls.get(), target.ptr) ? JNI_TRUE : JNI_FALSE;
}

// ---- TLS caches for FindClass / Get*MethodID / Get*FieldID ----------------

struct FindClassSlot {
    uint64_t gen = 0;
    uint64_t hash = 0;
    uint16_t len = 0;
    char name[126]{};
    std::shared_ptr<jnivm::Class> cls;
};

static constexpr size_t kFindClassCacheSize = 64;
thread_local std::array<FindClassSlot, kFindClassCacheSize> t_find_class_cache{};

static jclass fast_find_class(JNIEnv* env, const char* name) {
    if (!name) {
        return g_orig_jni.FindClass(env, name);
    }
    size_t len = 0;
    const uint64_t h = fnv1a(name, len);
    const uint64_t gen = g_vm_gen.load(std::memory_order_relaxed);
    if (len < sizeof(FindClassSlot::name)) {
        auto& slot = t_find_class_cache[h & (kFindClassCacheSize - 1)];
        if (slot.gen == gen && slot.hash == h && slot.len == len &&
            std::memcmp(slot.name, name, len) == 0 && slot.cls) {
            push_local(fast_env(env), slot.cls);
            return reinterpret_cast<jclass>(slot.cls.get());
        }
        jclass res = g_orig_jni.FindClass(env, name);
        if (res) {
            if (auto cp = fast_unwrap_class(res)) {
                auto sp = std::static_pointer_cast<jnivm::Class>(
                    cp.pin ? std::move(cp.pin) : cp.ptr->weak_from_this().lock());
                if (sp) {
                    slot.gen = gen;
                    slot.hash = h;
                    slot.len = static_cast<uint16_t>(len);
                    std::memcpy(slot.name, name, len + 1);
                    slot.cls = std::move(sp);
                }
            }
        }
        return res;
    }
    return g_orig_jni.FindClass(env, name);
}

struct MemberIdSlot {
    uint64_t gen = 0;
    uint64_t hash = 0;
    const jnivm::Class* cls = nullptr;
    void* id = nullptr;
    uint8_t kind = 0; // 0=method, 1=static_method, 2=field, 3=static_field
    uint8_t name_len = 0;
    uint16_t sig_len = 0;
    char name[64]{};
    char sig[128]{};
};

static constexpr size_t kMemberCacheSize = 128;
thread_local std::array<MemberIdSlot, kMemberCacheSize> t_member_cache{};

template <uint8_t Kind, typename IdT, typename OrigFn>
static IdT fast_lookup_member_id(JNIEnv* env, jclass cl, const char* name, const char* sig, OrigFn orig) {
    if (!cl || !name || !sig) {
        return orig(env, cl, name, sig);
    }
    FastClass cls = fast_unwrap_class(cl);
    jnivm::Class* cp = cls.ptr;
    if (!cp) {
        return orig(env, cl, name, sig);
    }
    size_t nlen = 0;
    size_t slen = 0;
    const uint64_t nh = fnv1a(name, nlen);
    const uint64_t sh = fnv1a(sig, slen);
    if (nlen < sizeof(MemberIdSlot::name) && slen < sizeof(MemberIdSlot::sig)) {
        const uint64_t h = (reinterpret_cast<uintptr_t>(cp) * 0x9E3779B97F4A7C15ULL) ^ nh ^ (sh * 31ULL) ^ Kind;
        const uint64_t gen = g_vm_gen.load(std::memory_order_relaxed);
        auto& slot = t_member_cache[h & (kMemberCacheSize - 1)];
        if (slot.gen == gen && slot.hash == h && slot.cls == cp && slot.kind == Kind &&
            slot.name_len == nlen && slot.sig_len == slen &&
            std::memcmp(slot.name, name, nlen) == 0 &&
            std::memcmp(slot.sig, sig, slen) == 0) {
            return reinterpret_cast<IdT>(slot.id);
        }
        IdT res = orig(env, cl, name, sig);
        if (res) {
            slot.gen = gen;
            slot.hash = h;
            slot.cls = cp;
            slot.id = reinterpret_cast<void*>(res);
            slot.kind = Kind;
            slot.name_len = static_cast<uint8_t>(nlen);
            slot.sig_len = static_cast<uint16_t>(slen);
            std::memcpy(slot.name, name, nlen + 1);
            std::memcpy(slot.sig, sig, slen + 1);
        }
        return res;
    }
    return orig(env, cl, name, sig);
}

static jmethodID fast_get_method_id(JNIEnv* env, jclass cl, const char* name, const char* sig) {
    return fast_lookup_member_id<0, jmethodID>(env, cl, name, sig, g_orig_jni.GetMethodID);
}
static jmethodID fast_get_static_method_id(JNIEnv* env, jclass cl, const char* name, const char* sig) {
    return fast_lookup_member_id<1, jmethodID>(env, cl, name, sig, g_orig_jni.GetStaticMethodID);
}
static jfieldID fast_get_field_id(JNIEnv* env, jclass cl, const char* name, const char* sig) {
    return fast_lookup_member_id<2, jfieldID>(env, cl, name, sig, g_orig_jni.GetFieldID);
}
static jfieldID fast_get_static_field_id(JNIEnv* env, jclass cl, const char* name, const char* sig) {
    return fast_lookup_member_id<3, jfieldID>(env, cl, name, sig, g_orig_jni.GetStaticFieldID);
}

// ---- Fast String & DirectByteBuffer accessors -----------------------------

struct FastString {
    jnivm::String* ptr = nullptr;
    std::shared_ptr<jnivm::Object> pin;
};

static inline FastString fast_as_string(jstring s) noexcept {
    FastObj raw = fast_unwrap(s);
    if (!raw.ptr) {
        return {};
    }
    if (__builtin_expect(vptr_of(raw.ptr) == g_vptr_string, 1)) {
        return {static_cast<jnivm::String*>(raw.ptr), std::move(raw.pin)};
    }
    auto* sp = dynamic_cast<jnivm::String*>(raw.ptr);
    if (!sp) {
        return {};
    }
    return {sp, std::move(raw.pin)};
}

static jstring fast_new_string_utf(JNIEnv* env, const char* str) {
    auto s = std::make_shared<jnivm::String>(std::string(str ? str : ""));
    push_local(fast_env(env), s);
    return reinterpret_cast<jstring>(s.get());
}

static jsize fast_get_string_utf_length(JNIEnv*, jstring str) {
    auto s = fast_as_string(str);
    return s.ptr ? static_cast<jsize>(s.ptr->length()) : 0;
}

static const char* fast_get_string_utf_chars(JNIEnv* env, jstring str, jboolean* isCopy) {
    auto s = fast_as_string(str);
    // For Weak refs, fall back to g_orig_jni so the returned pointer cannot
    // outlive our temporary lock.
    if (__builtin_expect(s.pin != nullptr, 0)) {
        return g_orig_jni.GetStringUTFChars(env, str, isCopy);
    }
    if (isCopy) {
        *isCopy = JNI_FALSE;
    }
    return s.ptr ? s.ptr->c_str() : "";
}

static void* fast_get_direct_buffer_address(JNIEnv*, jobject buf) {
    FastObj raw = fast_unwrap(buf);
    if (!raw.ptr) {
        return nullptr;
    }
    auto* bb = (__builtin_expect(vptr_of(raw.ptr) == g_vptr_bytebuf, 1))
        ? static_cast<jnivm::ByteBuffer*>(raw.ptr)
        : dynamic_cast<jnivm::ByteBuffer*>(raw.ptr);
    return bb ? bb->buffer : nullptr;
}

static jlong fast_get_direct_buffer_capacity(JNIEnv*, jobject buf) {
    FastObj raw = fast_unwrap(buf);
    if (!raw.ptr) {
        return -1;
    }
    auto* bb = (__builtin_expect(vptr_of(raw.ptr) == g_vptr_bytebuf, 1))
        ? static_cast<jnivm::ByteBuffer*>(raw.ptr)
        : dynamic_cast<jnivm::ByteBuffer*>(raw.ptr);
    return bb ? bb->capacity : -1;
}

// ---- Leak-free single-alloc New*Array & zero-dynamic_cast Array ops -------

template <class T>
struct ArrayTraits;

#define DEFINE_ARRAY_TRAITS(CType, JArrType, Name, VptrVar, ClsVar) \
    template <> struct ArrayTraits<CType> { \
        using JArr = JArrType; \
        static inline const void* vptr() noexcept { return VptrVar; } \
        static inline const std::shared_ptr<jnivm::Class>& cls() noexcept { return ClsVar; } \
        static inline CType* orig_get_elements(JNIEnv* e, JArr a, jboolean* c) { \
            return g_orig_jni.Get##Name##ArrayElements(e, a, c); \
        } \
        static inline void orig_get_region(JNIEnv* e, JArr a, jsize s, jsize l, CType* b) { \
            g_orig_jni.Get##Name##ArrayRegion(e, a, s, l, b); \
        } \
        static inline void orig_set_region(JNIEnv* e, JArr a, jsize s, jsize l, const CType* b) { \
            g_orig_jni.Set##Name##ArrayRegion(e, a, s, l, b); \
        } \
    }

DEFINE_ARRAY_TRAITS(jboolean, jbooleanArray, Boolean, g_vptr_arr_bool, g_cls_arr_bool);
DEFINE_ARRAY_TRAITS(jbyte, jbyteArray, Byte, g_vptr_arr_byte, g_cls_arr_byte);
DEFINE_ARRAY_TRAITS(jchar, jcharArray, Char, g_vptr_arr_char, g_cls_arr_char);
DEFINE_ARRAY_TRAITS(jshort, jshortArray, Short, g_vptr_arr_short, g_cls_arr_short);
DEFINE_ARRAY_TRAITS(jint, jintArray, Int, g_vptr_arr_int, g_cls_arr_int);
DEFINE_ARRAY_TRAITS(jlong, jlongArray, Long, g_vptr_arr_long, g_cls_arr_long);
DEFINE_ARRAY_TRAITS(jfloat, jfloatArray, Float, g_vptr_arr_float, g_cls_arr_float);
DEFINE_ARRAY_TRAITS(jdouble, jdoubleArray, Double, g_vptr_arr_double, g_cls_arr_double);
#undef DEFINE_ARRAY_TRAITS

template <class T>
struct FastArray {
    jnivm::Array<T>* ptr = nullptr;
    std::shared_ptr<jnivm::Object> pin;
};

template <class T>
static inline FastArray<T> fast_as_array(typename ArrayTraits<T>::JArr a) noexcept {
    FastObj raw = fast_unwrap(a);
    if (!raw.ptr) {
        return {};
    }
    if (__builtin_expect(vptr_of(raw.ptr) == ArrayTraits<T>::vptr(), 1)) {
        return {static_cast<jnivm::Array<T>*>(raw.ptr), std::move(raw.pin)};
    }
    auto* arr = dynamic_cast<jnivm::Array<T>*>(raw.ptr);
    if (!arr) {
        return {};
    }
    return {arr, std::move(raw.pin)};
}

static jsize fast_get_array_length(JNIEnv*, jarray a) {
    FastObj raw = fast_unwrap(a);
    if (!raw.ptr) {
        return 0;
    }
    const void* vp = vptr_of(raw.ptr);
    if (vp == g_vptr_arr_byte || vp == g_vptr_arr_int || vp == g_vptr_arr_float ||
        vp == g_vptr_arr_short || vp == g_vptr_arr_long || vp == g_vptr_arr_bool ||
        vp == g_vptr_arr_char || vp == g_vptr_arr_double) {
        return static_cast<jnivm::Array<void>*>(raw.ptr)->getSize();
    }
    auto* arr = dynamic_cast<jnivm::Array<void>*>(raw.ptr);
    return arr ? arr->getSize() : 0;
}

template <class T>
static typename ArrayTraits<T>::JArr fast_new_array(JNIEnv* env, jsize length) {
    if (length < 0) {
        length = 0;
    }
    auto arr = length > 0
        ? std::make_shared<jnivm::Array<T>>(length)
        : std::make_shared<jnivm::Array<T>>();
    if (length > 0) {
        std::memset(arr->getArray(), 0, sizeof(T) * static_cast<size_t>(length));
    }
    arr->clazz = ArrayTraits<T>::cls();
    push_local(fast_env(env), arr);
    return reinterpret_cast<typename ArrayTraits<T>::JArr>(arr.get());
}

template <class T>
static T* fast_get_array_elements(JNIEnv* env, typename ArrayTraits<T>::JArr a, jboolean* isCopy) {
    auto arr = fast_as_array<T>(a);
    if (__builtin_expect(arr.pin != nullptr, 0)) {
        return ArrayTraits<T>::orig_get_elements(env, a, isCopy);
    }
    if (isCopy) {
        *isCopy = JNI_FALSE;
    }
    return arr.ptr ? arr.ptr->getArray() : nullptr;
}

template <class T>
static void fast_get_array_region(JNIEnv* env, typename ArrayTraits<T>::JArr a, jsize start, jsize len, T* buf) {
    auto arr = fast_as_array<T>(a);
    if (__builtin_expect(!arr.ptr || !buf, 0)) {
        ArrayTraits<T>::orig_get_region(env, a, start, len, buf);
        return;
    }
    const jsize raw_size = arr.ptr->getSize();
    const size_t arr_size = raw_size > 0 ? static_cast<size_t>(raw_size) : 0;
    if (__builtin_expect(
            start < 0 || len < 0 || static_cast<size_t>(start) > arr_size ||
                static_cast<size_t>(len) > arr_size - static_cast<size_t>(start),
            0)) {
        ArrayTraits<T>::orig_get_region(env, a, start, len, buf);
        return;
    }
    if (len > 0) {
        std::memcpy(buf, arr.ptr->getArray() + start, sizeof(T) * static_cast<size_t>(len));
    }
}

template <class T>
static void fast_set_array_region(JNIEnv* env, typename ArrayTraits<T>::JArr a, jsize start, jsize len, const T* buf) {
    auto arr = fast_as_array<T>(a);
    if (__builtin_expect(!arr.ptr || !buf, 0)) {
        ArrayTraits<T>::orig_set_region(env, a, start, len, buf);
        return;
    }
    const jsize raw_size = arr.ptr->getSize();
    const size_t arr_size = raw_size > 0 ? static_cast<size_t>(raw_size) : 0;
    if (__builtin_expect(
            start < 0 || len < 0 || static_cast<size_t>(start) > arr_size ||
                static_cast<size_t>(len) > arr_size - static_cast<size_t>(start),
            0)) {
        ArrayTraits<T>::orig_set_region(env, a, start, len, buf);
        return;
    }
    if (len > 0) {
        std::memcpy(arr.ptr->getArray() + start, buf, sizeof(T) * static_cast<size_t>(len));
    }
}

static void* fast_get_primitive_array_critical(JNIEnv* env, jarray a, jboolean* isCopy) {
    FastObj raw = fast_unwrap(a);
    if (__builtin_expect(raw.pin != nullptr, 0)) {
        return g_orig_jni.GetPrimitiveArrayCritical(env, a, isCopy);
    }
    if (isCopy) {
        *isCopy = JNI_FALSE;
    }
    if (!raw.ptr) {
        return nullptr;
    }
    const void* vp = vptr_of(raw.ptr);
    if (vp == g_vptr_arr_byte || vp == g_vptr_arr_int || vp == g_vptr_arr_float ||
        vp == g_vptr_arr_short || vp == g_vptr_arr_long || vp == g_vptr_arr_bool ||
        vp == g_vptr_arr_char || vp == g_vptr_arr_double) {
        return static_cast<jnivm::Array<void>*>(raw.ptr)->getArray();
    }
    auto* arr = dynamic_cast<jnivm::Array<void>*>(raw.ptr);
    return arr ? arr->getArray() : nullptr;
}

static inline jnivm::Array<jnivm::Object>* fast_as_obj_array(jnivm::Object* raw) noexcept {
    if (__builtin_expect(vptr_of(raw) == g_vptr_arr_obj, 1)) {
        return reinterpret_cast<jnivm::Array<jnivm::Object>*>(
            reinterpret_cast<char*>(raw) + g_arr_obj_delta);
    }
    return dynamic_cast<jnivm::Array<jnivm::Object>*>(raw);
}

static jobject fast_get_object_array_element(JNIEnv* env, jobjectArray a, jsize index) {
    FastObj raw = fast_unwrap(a);
    if (__builtin_expect(!raw.ptr, 0)) {
        return nullptr;
    }
    auto* arr = fast_as_obj_array(raw.ptr);
    if (__builtin_expect(!arr || index < 0 || index >= arr->getSize(), 0)) {
        return g_orig_jni.GetObjectArrayElement(env, a, index);
    }
    std::shared_ptr<jnivm::Object> elem = arr->Get(index, jnivm::impl::array_type_t<jnivm::Object>{});
    if (!elem) {
        return nullptr;
    }
    if (elem->clazz.expired()) {
        elem->clazz = g_cls_object;
    }
    push_local(fast_env(env), elem);
    return reinterpret_cast<jobject>(elem.get());
}

static void fast_set_object_array_element(JNIEnv* env, jobjectArray a, jsize index, jobject val) {
    FastObj raw = fast_unwrap(a);
    if (__builtin_expect(!raw.ptr, 0)) {
        g_orig_jni.SetObjectArrayElement(env, a, index, val);
        return;
    }
    auto* arr = fast_as_obj_array(raw.ptr);
    if (__builtin_expect(!arr || index < 0 || index >= arr->getSize(), 0)) {
        g_orig_jni.SetObjectArrayElement(env, a, index, val);
        return;
    }
    if (!val) {
        arr->Set(index, nullptr);
        return;
    }
    FastObj raw_val = fast_unwrap(val);
    if (!raw_val.ptr) {
        arr->Set(index, nullptr);
        return;
    }
    auto strong = raw_val.pin ? std::move(raw_val.pin) : raw_val.ptr->weak_from_this().lock();
    if (!strong) {
        g_orig_jni.SetObjectArrayElement(env, a, index, val);
        return;
    }
    arr->Set(index, strong);
}

// ---- Fast Get*Field / Set*Field / GetStatic*Field / SetStatic*Field -------

template <class T>
struct FastField {
    static T get_inst(JNIEnv* env, jobject obj, jfieldID id) {
        auto* fid = reinterpret_cast<jnivm::Field*>(id);
        FastObj raw = fast_unwrap(obj);
        if (__builtin_expect(!fid || !fid->getnativehandle || fid->_static || !raw.ptr, 0)) {
            return orig_get_inst(env, obj, id);
        }
        return static_cast<jnivm::impl::MethodHandleBase<T>*>(fid->getnativehandle.get())
            ->InstanceGet(fast_env(env), obj, nullptr, jnivm::impl::MethodHandleBase<T>{});
    }

    static void set_inst(JNIEnv* env, jobject obj, jfieldID id, T value) {
        auto* fid = reinterpret_cast<jnivm::Field*>(id);
        FastObj raw = fast_unwrap(obj);
        if (__builtin_expect(!fid || !fid->setnativehandle || fid->_static || !raw.ptr, 0)) {
            orig_set_inst(env, obj, id, value);
            return;
        }
        jvalue val{};
        std::memcpy(&val, &value, sizeof(T));
        static_cast<jnivm::impl::MethodHandleBase<T>*>(fid->setnativehandle.get())
            ->InstanceSet(fast_env(env), obj, &val, jnivm::impl::MethodHandleBase<T>{});
    }

    static T get_static(JNIEnv* env, jclass cl, jfieldID id) {
        auto* fid = reinterpret_cast<jnivm::Field*>(id);
        FastClass cls = fast_unwrap_class(cl);
        if (__builtin_expect(!fid || !fid->getnativehandle || !fid->_static || !cls.ptr, 0)) {
            return orig_get_static(env, cl, id);
        }
        return static_cast<jnivm::impl::MethodHandleBase<T>*>(fid->getnativehandle.get())
            ->StaticGet(fast_env(env), cls.ptr, nullptr, jnivm::impl::MethodHandleBase<T>{});
    }

    static void set_static(JNIEnv* env, jclass cl, jfieldID id, T value) {
        auto* fid = reinterpret_cast<jnivm::Field*>(id);
        FastClass cls = fast_unwrap_class(cl);
        if (__builtin_expect(!fid || !fid->setnativehandle || !fid->_static || !cls.ptr, 0)) {
            orig_set_static(env, cl, id, value);
            return;
        }
        jvalue val{};
        std::memcpy(&val, &value, sizeof(T));
        static_cast<jnivm::impl::MethodHandleBase<T>*>(fid->setnativehandle.get())
            ->StaticSet(fast_env(env), cls.ptr, &val, jnivm::impl::MethodHandleBase<T>{});
    }

    static T orig_get_inst(JNIEnv* env, jobject obj, jfieldID id);
    static void orig_set_inst(JNIEnv* env, jobject obj, jfieldID id, T val);
    static T orig_get_static(JNIEnv* env, jclass cl, jfieldID id);
    static void orig_set_static(JNIEnv* env, jclass cl, jfieldID id, T val);
};

#define DEFINE_ORIG_FIELD(T, Name) \
    template <> T FastField<T>::orig_get_inst(JNIEnv* e, jobject o, jfieldID f) { return g_orig_jni.Get##Name##Field(e, o, f); } \
    template <> void FastField<T>::orig_set_inst(JNIEnv* e, jobject o, jfieldID f, T v) { g_orig_jni.Set##Name##Field(e, o, f, v); } \
    template <> T FastField<T>::orig_get_static(JNIEnv* e, jclass c, jfieldID f) { return g_orig_jni.GetStatic##Name##Field(e, c, f); } \
    template <> void FastField<T>::orig_set_static(JNIEnv* e, jclass c, jfieldID f, T v) { g_orig_jni.SetStatic##Name##Field(e, c, f, v); }

DEFINE_ORIG_FIELD(jobject, Object)
DEFINE_ORIG_FIELD(jboolean, Boolean)
DEFINE_ORIG_FIELD(jbyte, Byte)
DEFINE_ORIG_FIELD(jchar, Char)
DEFINE_ORIG_FIELD(jshort, Short)
DEFINE_ORIG_FIELD(jint, Int)
DEFINE_ORIG_FIELD(jlong, Long)
DEFINE_ORIG_FIELD(jfloat, Float)
DEFINE_ORIG_FIELD(jdouble, Double)
#undef DEFINE_ORIG_FIELD

// ---- Zero-heap stack va_list -> jvalue[16] & zero-leak Call*Method --------

static constexpr size_t kStackArgsMax = 16;

static jvalue* parse_va_args(va_list list, const char* sig, jvalue (&stack_buf)[kStackArgsMax], std::vector<jvalue>& overflow) {
    if (!sig || sig[0] != '(') {
        return stack_buf;
    }
    const char* p = sig + 1;
    size_t count = 0;
    auto push = [&](const jvalue& v) {
        if (__builtin_expect(count < kStackArgsMax, 1)) {
            stack_buf[count++] = v;
        } else {
            if (overflow.empty()) {
                overflow.assign(stack_buf, stack_buf + kStackArgsMax);
            }
            overflow.push_back(v);
            ++count;
        }
    };
    while (*p && *p != ')') {
        jvalue v{};
        switch (*p) {
        case 'V':
            break;
        case 'Z':
            v.z = static_cast<jboolean>(va_arg(list, int));
            break;
        case 'B':
            v.b = static_cast<jbyte>(va_arg(list, int));
            break;
        case 'S':
            v.s = static_cast<jshort>(va_arg(list, int));
            break;
        case 'C':
            v.c = static_cast<jchar>(va_arg(list, int));
            break;
        case 'I':
            v.i = va_arg(list, jint);
            break;
        case 'J':
            v.j = va_arg(list, jlong);
            break;
        case 'F':
            v.f = static_cast<jfloat>(va_arg(list, jdouble));
            break;
        case 'D':
            v.d = va_arg(list, jdouble);
            break;
        case '[': {
            const char* end = p + std::strlen(p);
            p = jnivm::SkipJNIType(p + 1, end) - 1;
            v.l = va_arg(list, jobject);
            break;
        }
        case 'L': {
            const char* semi = std::strchr(p, ';');
            if (!semi) {
                return overflow.empty() ? stack_buf : overflow.data();
            }
            p = semi;
            v.l = va_arg(list, jobject);
            break;
        }
        default:
            break;
        }
        push(v);
        ++p;
    }
    return overflow.empty() ? stack_buf : overflow.data();
}

struct VDispatchSlot {
    uint64_t gen = 0;
    const void* key = nullptr; // vptr for typed C++ subclasses, Class* for plain jnivm::Object
    const jnivm::Method* mid = nullptr;
    jnivm::Method* resolved = nullptr;
    bool is_vptr = false;
};

static constexpr size_t kVDispatchCacheSize = 64;
thread_local std::array<VDispatchSlot, kVDispatchCacheSize> t_vdispatch_cache{};

static jnivm::Method* resolve_virtual_uncached(jnivm::Class* cl, jnivm::Method* mid) {
    if (!cl) {
        return mid;
    }
    jnivm::Method* found = nullptr;
    {
        std::lock_guard<std::mutex> lock(cl->mtx);
        // Pointer identity hit first (exact declaring class, zero string comparisons).
        for (const auto& m : cl->methods) {
            if (m.get() == mid && m->nativehandle) {
                found = mid;
                break;
            }
        }
        if (!found) {
            for (const auto& m : cl->methods) {
                if (!m->_static && m->nativehandle && m->name == mid->name && m->signature == mid->signature) {
                    found = m.get();
                    break;
                }
            }
        }
    }
    if (!found && mid->nativehandle) {
        found = mid;
    }
    return found;
}

static inline jnivm::Method* resolve_virtual_for_object(jnivm::Object* raw, jnivm::Method* mid) {
    const uint64_t gen = g_vm_gen.load(std::memory_order_relaxed);
    const void* vp = vptr_of(raw);
    if (__builtin_expect(vp != g_vptr_object, 1)) {
        // Typed C++ subclass: key by (vptr, mid) to skip `raw->clazz.lock()`
        // (avoids 2 cross-core atomic RMWs on every virtual JNI call).
        const size_t idx = ((reinterpret_cast<uintptr_t>(vp) >> 4) ^ (reinterpret_cast<uintptr_t>(mid) >> 4)) &
                           (kVDispatchCacheSize - 1);
        auto& slot = t_vdispatch_cache[idx];
        if (__builtin_expect(slot.gen == gen && slot.is_vptr && slot.key == vp && slot.mid == mid, 1)) {
            return slot.resolved;
        }
        auto cl_sp = raw->clazz.lock();
        jnivm::Method* found = resolve_virtual_uncached(cl_sp.get(), mid);
        if (found) {
            slot.gen = gen;
            slot.key = vp;
            slot.mid = mid;
            slot.resolved = found;
            slot.is_vptr = true;
        }
        return found;
    }
    // Plain jnivm::Object (shared across anonymous/untyped Java classes): key by Class*.
    auto cl_sp = raw->clazz.lock();
    jnivm::Class* cl = cl_sp.get();
    if (!cl) {
        return mid;
    }
    const size_t idx = ((reinterpret_cast<uintptr_t>(cl) >> 4) ^ (reinterpret_cast<uintptr_t>(mid) >> 4)) &
                       (kVDispatchCacheSize - 1);
    auto& slot = t_vdispatch_cache[idx];
    if (__builtin_expect(slot.gen == gen && !slot.is_vptr && slot.key == cl && slot.mid == mid, 1)) {
        return slot.resolved;
    }
    jnivm::Method* found = resolve_virtual_uncached(cl, mid);
    if (found) {
        slot.gen = gen;
        slot.key = cl;
        slot.mid = mid;
        slot.resolved = found;
        slot.is_vptr = false;
    }
    return found;
}

template <class T>
struct FastCall {
    static T instance_a(JNIEnv* env, jobject obj, jmethodID id, jvalue* args) {
        auto* mid = reinterpret_cast<jnivm::Method*>(id);
        FastObj raw = fast_unwrap(obj);
        if (__builtin_expect(!mid || !mid->nativehandle || !raw.ptr, 0)) {
            return orig_a(env, obj, id, args);
        }
        jnivm::Method* resolved = resolve_virtual_for_object(raw.ptr, mid);
        if (__builtin_expect(!resolved || !resolved->nativehandle, 0)) {
            return orig_a(env, obj, id, args);
        }
        auto* nenv = fast_env(env);
        try {
            return static_cast<jnivm::impl::MethodHandleBase<T>*>(resolved->nativehandle.get())
                ->InstanceInvoke(nenv, obj, args, jnivm::impl::MethodHandleBase<T>{});
        } catch (...) {
            auto cur = std::make_shared<jnivm::Throwable>();
            cur->except = std::current_exception();
            nenv->current_exception = std::move(cur);
            if constexpr (!std::is_void_v<T>) {
                return T{};
            }
        }
    }

    static T instance_v(JNIEnv* env, jobject obj, jmethodID id, va_list ap) {
        auto* mid = reinterpret_cast<jnivm::Method*>(id);
        if (__builtin_expect(!mid || !mid->nativehandle || !obj, 0)) {
            return orig_v(env, obj, id, ap);
        }
        const char* sig = mid->signature.c_str();
        // Fast-path 0-arg calls without constructing std::vector or parsing va_list.
        if (__builtin_expect(sig[0] == '(' && sig[1] == ')', 1)) {
            return instance_a(env, obj, id, nullptr);
        }
        jvalue stack_buf[kStackArgsMax];
        std::vector<jvalue> overflow;
        jvalue* args = parse_va_args(ap, sig, stack_buf, overflow);
        return instance_a(env, obj, id, args);
    }

    static T instance_dots(JNIEnv* env, jobject obj, jmethodID id, ...) {
        auto* mid = reinterpret_cast<jnivm::Method*>(id);
        if (__builtin_expect(mid && mid->nativehandle && obj, 1)) {
            const char* sig = mid->signature.c_str();
            if (sig[0] == '(' && sig[1] == ')') {
                return instance_a(env, obj, id, nullptr);
            }
        }
        va_list ap;
        va_start(ap, id);
        if constexpr (std::is_void_v<T>) {
            instance_v(env, obj, id, ap);
            va_end(ap);
        } else {
            T r = instance_v(env, obj, id, ap);
            va_end(ap);
            return r;
        }
    }

    static T static_a(JNIEnv* env, jclass cl, jmethodID id, jvalue* args) {
        auto* mid = reinterpret_cast<jnivm::Method*>(id);
        FastClass cls = fast_unwrap_class(cl);
        jnivm::Class* cp = cls.ptr;
        if (__builtin_expect(!mid || !mid->nativehandle || !cp, 0)) {
            return orig_static_a(env, cl, id, args);
        }
        auto* nenv = fast_env(env);
        try {
            return static_cast<jnivm::impl::MethodHandleBase<T>*>(mid->nativehandle.get())
                ->StaticInvoke(nenv, cp, args, jnivm::impl::MethodHandleBase<T>{});
        } catch (...) {
            auto cur = std::make_shared<jnivm::Throwable>();
            cur->except = std::current_exception();
            nenv->current_exception = std::move(cur);
            if constexpr (!std::is_void_v<T>) {
                return T{};
            }
        }
    }

    static T static_v(JNIEnv* env, jclass cl, jmethodID id, va_list ap) {
        auto* mid = reinterpret_cast<jnivm::Method*>(id);
        if (__builtin_expect(!mid || !mid->nativehandle || !cl, 0)) {
            return orig_static_v(env, cl, id, ap);
        }
        const char* sig = mid->signature.c_str();
        if (__builtin_expect(sig[0] == '(' && sig[1] == ')', 1)) {
            return static_a(env, cl, id, nullptr);
        }
        jvalue stack_buf[kStackArgsMax];
        std::vector<jvalue> overflow;
        jvalue* args = parse_va_args(ap, sig, stack_buf, overflow);
        return static_a(env, cl, id, args);
    }

    static T static_dots(JNIEnv* env, jclass cl, jmethodID id, ...) {
        auto* mid = reinterpret_cast<jnivm::Method*>(id);
        if (__builtin_expect(mid && mid->nativehandle && cl, 1)) {
            const char* sig = mid->signature.c_str();
            if (sig[0] == '(' && sig[1] == ')') {
                return static_a(env, cl, id, nullptr);
            }
        }
        va_list ap;
        va_start(ap, id);
        if constexpr (std::is_void_v<T>) {
            static_v(env, cl, id, ap);
            va_end(ap);
        } else {
            T r = static_v(env, cl, id, ap);
            va_end(ap);
            return r;
        }
    }

    static T orig_a(JNIEnv* env, jobject obj, jmethodID id, jvalue* args);
    static T orig_v(JNIEnv* env, jobject obj, jmethodID id, va_list ap);
    static T orig_static_a(JNIEnv* env, jclass cl, jmethodID id, jvalue* args);
    static T orig_static_v(JNIEnv* env, jclass cl, jmethodID id, va_list ap);
};

#define DEFINE_ORIG_CALL(T, Name) \
    template <> T FastCall<T>::orig_a(JNIEnv* e, jobject o, jmethodID m, jvalue* a) { return g_orig_jni.Call##Name##MethodA(e, o, m, a); } \
    template <> T FastCall<T>::orig_v(JNIEnv* e, jobject o, jmethodID m, va_list a) { return g_orig_jni.Call##Name##MethodV(e, o, m, a); } \
    template <> T FastCall<T>::orig_static_a(JNIEnv* e, jclass c, jmethodID m, jvalue* a) { return g_orig_jni.CallStatic##Name##MethodA(e, c, m, a); } \
    template <> T FastCall<T>::orig_static_v(JNIEnv* e, jclass c, jmethodID m, va_list a) { return g_orig_jni.CallStatic##Name##MethodV(e, c, m, a); }

DEFINE_ORIG_CALL(jobject, Object)
DEFINE_ORIG_CALL(jboolean, Boolean)
DEFINE_ORIG_CALL(jbyte, Byte)
DEFINE_ORIG_CALL(jchar, Char)
DEFINE_ORIG_CALL(jshort, Short)
DEFINE_ORIG_CALL(jint, Int)
DEFINE_ORIG_CALL(jlong, Long)
DEFINE_ORIG_CALL(jfloat, Float)
DEFINE_ORIG_CALL(jdouble, Double)
DEFINE_ORIG_CALL(void, Void)
#undef DEFINE_ORIG_CALL

class ProcessVM final : public jnivm::VM {
public:
    ProcessVM() : VM(true) {
        g_orig_jni = GetNativeInterfaceTemplate();
        init_fast_vptrs();
        g_global_index.clear();
        g_vm_gen.fetch_add(1, std::memory_order_relaxed);

        const bool fast_jni =
#ifdef JNI_TRACE
            false;
#else
            getenv("VANTA_NO_JNI_FAST") == nullptr;
#endif

        // libjnivm also takes the VM mutex to read this immutable association.
        // Hand out `&g_traced_vm` so JNIEnv::GetJavaVM stays on the same
        // wrapper table as JNI_OnLoad: DetachCurrentThread invalidates `t_env`
        // and libjnivm's own `g_real_vm` (and its `reserved0` pointer) stays
        // private to this file.
        AddHook([fast_jni](JNINativeInterface& iface) {
            iface.GetJavaVM = [](JNIEnv*, JavaVM** out) -> jint {
                if (out) {
                    *out = &g_traced_vm;
                }
                return JNI_OK;
            };
            if (!fast_jni) {
                return;
            }
            // Ref, frame, exception, & class lookups
            iface.FindClass = fast_find_class;
            iface.GetObjectClass = fast_get_object_class;
            iface.IsInstanceOf = fast_is_instance_of;
            iface.NewGlobalRef = fast_new_global_ref;
            iface.DeleteGlobalRef = fast_delete_global_ref;
            iface.NewWeakGlobalRef = fast_new_weak_global_ref;
            iface.DeleteWeakGlobalRef = fast_delete_weak_global_ref;
            iface.DeleteLocalRef = fast_delete_local_ref;
            iface.NewLocalRef = fast_new_local_ref;
            iface.IsSameObject = fast_is_same_object;
            iface.GetObjectRefType = fast_get_object_ref_type;
            iface.PushLocalFrame = fast_push_local_frame;
            iface.PopLocalFrame = fast_pop_local_frame;
            iface.EnsureLocalCapacity = fast_ensure_local_capacity;
            iface.ExceptionCheck = fast_exception_check;
            iface.ExceptionOccurred = fast_exception_occurred;
            iface.ExceptionClear = fast_exception_clear;
            iface.GetMethodID = fast_get_method_id;
            iface.GetStaticMethodID = fast_get_static_method_id;
            iface.GetFieldID = fast_get_field_id;
            iface.GetStaticFieldID = fast_get_static_field_id;

            // Object construction (libjnivm dispatches NewObject* through the
            // same static MDispatch<jobject, jclass> path, now stack-buffered).
            iface.NewObject = FastCall<jobject>::static_dots;
            iface.NewObjectV = FastCall<jobject>::static_v;
            iface.NewObjectA = FastCall<jobject>::static_a;

            // Strings & DirectByteBuffer
            iface.NewStringUTF = fast_new_string_utf;
            iface.GetStringUTFLength = fast_get_string_utf_length;
            iface.GetStringUTFChars = fast_get_string_utf_chars;
            iface.GetDirectBufferAddress = fast_get_direct_buffer_address;
            iface.GetDirectBufferCapacity = fast_get_direct_buffer_capacity;

            // Arrays (leak-free single alloc + zero dynamic_cast accessors)
            iface.GetArrayLength = fast_get_array_length;
            iface.GetObjectArrayElement = fast_get_object_array_element;
            iface.SetObjectArrayElement = fast_set_object_array_element;
            iface.GetPrimitiveArrayCritical = fast_get_primitive_array_critical;
#define HOOK_PRIM_ARRAY(T, Name) \
            iface.New##Name##Array = fast_new_array<T>; \
            iface.Get##Name##ArrayElements = fast_get_array_elements<T>; \
            iface.Get##Name##ArrayRegion = fast_get_array_region<T>; \
            iface.Set##Name##ArrayRegion = fast_set_array_region<T>
            HOOK_PRIM_ARRAY(jboolean, Boolean);
            HOOK_PRIM_ARRAY(jbyte, Byte);
            HOOK_PRIM_ARRAY(jchar, Char);
            HOOK_PRIM_ARRAY(jshort, Short);
            HOOK_PRIM_ARRAY(jint, Int);
            HOOK_PRIM_ARRAY(jlong, Long);
            HOOK_PRIM_ARRAY(jfloat, Float);
            HOOK_PRIM_ARRAY(jdouble, Double);
#undef HOOK_PRIM_ARRAY

            // Fields (zero dynamic_cast / zero localref leak)
#define HOOK_FIELD(T, Name) \
            iface.Get##Name##Field = FastField<T>::get_inst; \
            iface.Set##Name##Field = FastField<T>::set_inst; \
            iface.GetStatic##Name##Field = FastField<T>::get_static; \
            iface.SetStatic##Name##Field = FastField<T>::set_static
            HOOK_FIELD(jobject, Object);
            HOOK_FIELD(jboolean, Boolean);
            HOOK_FIELD(jbyte, Byte);
            HOOK_FIELD(jchar, Char);
            HOOK_FIELD(jshort, Short);
            HOOK_FIELD(jint, Int);
            HOOK_FIELD(jlong, Long);
            HOOK_FIELD(jfloat, Float);
            HOOK_FIELD(jdouble, Double);
#undef HOOK_FIELD

            // Zero-heap, zero-localref-leak Call*Method/V/A & CallStatic*Method/V/A
#define HOOK_CALL(T, Name) \
            iface.Call##Name##Method = FastCall<T>::instance_dots; \
            iface.Call##Name##MethodV = FastCall<T>::instance_v; \
            iface.Call##Name##MethodA = FastCall<T>::instance_a; \
            iface.CallStatic##Name##Method = FastCall<T>::static_dots; \
            iface.CallStatic##Name##MethodV = FastCall<T>::static_v; \
            iface.CallStatic##Name##MethodA = FastCall<T>::static_a
            HOOK_CALL(jobject, Object);
            HOOK_CALL(jboolean, Boolean);
            HOOK_CALL(jbyte, Byte);
            HOOK_CALL(jchar, Char);
            HOOK_CALL(jshort, Short);
            HOOK_CALL(jint, Int);
            HOOK_CALL(jlong, Long);
            HOOK_CALL(jfloat, Float);
            HOOK_CALL(jdouble, Double);
            HOOK_CALL(void, Void);
#undef HOOK_CALL
        });
        initialize();
        init_fast_classes(GetEnv().get());
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
    return env ? fast_env(env) : nullptr;
}
} // namespace vanta
