#include <jnivm.h>
#include <jnivm/bytebuffer.h>

#include <atomic>
#include <cassert>
#include <chrono>
#include <cstdio>
#include <cstring>
#include <memory>
#include <thread>
#include <time.h>
#include <vector>

extern "C" void* vanta_jni_create_vm();
extern "C" void* vanta_jni_thread_env();
extern "C" int vanta_jni_call_onload(void*, char*, size_t);

// The invoke interface needs no Android classes or engine library.
extern "C" void vanta_register_android_classes(void*) {}
extern "C" void vanta_unimplemented_record(unsigned, const char*) {}
namespace vanta {
void register_game_activity_classes(jnivm::ENV*) {}
void register_init_params_classes(jnivm::ENV*) {}
}

namespace {
jint onload(JavaVM* vm, void* expected) {
    assert(expected == nullptr);
    void* env = nullptr;
    assert(vm->GetEnv(&env, JNI_VERSION_1_6) == JNI_OK && env);
    JavaVM* from_env = nullptr;
    assert(static_cast<JNIEnv*>(env)->GetJavaVM(&from_env) == JNI_OK);
    assert(from_env == vm);
    return JNI_VERSION_1_6;
}

void check(JavaVM* vm) {
    char err[128]{};
    assert(vanta_jni_call_onload(reinterpret_cast<void*>(onload), err, sizeof err) == JNI_VERSION_1_6);
    const auto* main_env = vanta_jni_thread_env();
    std::vector<std::thread> workers;
    for (unsigned i = 0; i < 8; ++i) {
        workers.emplace_back([=] {
            void* first = nullptr;
            assert(vm->GetEnv(&first, JNI_VERSION_1_6) == JNI_OK && first);
            assert(first != main_env);
            for (unsigned n = 0; n < 32; ++n) {
                auto* env = static_cast<JNIEnv*>(vanta_jni_thread_env());
                JNIEnv* attached = nullptr;
                assert(vm->AttachCurrentThread(&attached, nullptr) == JNI_OK && attached == env);
                assert(vm->AttachCurrentThreadAsDaemon(&attached, nullptr) == JNI_OK && attached == env);
                JavaVM* from_env = nullptr;
                assert(env->GetJavaVM(&from_env) == JNI_OK && from_env == vm);
                // Keep the old env alive so a recycled allocator address cannot
                // disguise a stale cache entry.
                auto old = jnivm::ENV::FromJNIEnv(env)->shared_from_this();
                assert(from_env->DetachCurrentThread() == JNI_OK);
                void* next = nullptr;
                if (n % 2 == 0) {
                    assert(vm->GetEnv(&next, JNI_VERSION_1_6) == JNI_OK);
                } else {
                    next = vanta_jni_thread_env();
                }
                assert(next && next != env);
            }
            assert(vm->DetachCurrentThread() == JNI_OK);
        });
    }
    for (auto& worker : workers) {
        worker.join();
    }
    assert(vanta_jni_thread_env() == main_env);

    // Regression: callers may invoke the JNIInvokeInterface entries with a
    // copied/wrapped JavaVM whose `reserved0` is null, or with a null `vm`
    // pointer. The shim must still delegate to libjnivm's real VM rather than
    // throwing "Failed to get reference to jnivm::VM".
    std::thread wrapped_worker([=] {
        JNIInvokeInterface copied_iface = *vm->functions;
        copied_iface.reserved0 = nullptr;
        JavaVM wrapped_vm{};
        wrapped_vm.functions = &copied_iface;

        void* env1 = nullptr;
        assert(wrapped_vm.GetEnv(&env1, JNI_VERSION_1_6) == JNI_OK && env1);
        assert(wrapped_vm.DetachCurrentThread() == JNI_OK);

        void* env2 = nullptr;
        assert(vm->functions->GetEnv(nullptr, &env2, JNI_VERSION_1_6) == JNI_OK && env2);
        JNIEnv* attached = nullptr;
        assert(vm->functions->AttachCurrentThread(&wrapped_vm, &attached, nullptr) == JNI_OK &&
               attached == env2);
        assert(vm->functions->DetachCurrentThread(nullptr) == JNI_OK);
    });
    wrapped_worker.join();

    // Verify fast JNI rocket layer: FindClass, GetMethodID, GetStaticMethodID,
    // Call*Method/V/A, CallStatic*Method, Strings, Arrays, DirectByteBuffer,
    // Global/Local refs, and ZERO localframe leak across 10,000 virtual calls.
    struct BenchObj : jnivm::Object {
        int base = 7;
        jint add(jint a, jint b) { return base + a + b; }
        static jint mul(jint a, jint b) { return a * b; }
    };
    auto* jenv = static_cast<JNIEnv*>(vanta_jni_thread_env());
    auto* nenv = jnivm::ENV::FromJNIEnv(jenv);
    auto bench_cls = nenv->GetClass<BenchObj>("com/vanta/test/BenchObj");
    bench_cls->HookInstance(nenv, "add", &BenchObj::add);
    bench_cls->Hook(nenv, "mul", &BenchObj::mul);

    jclass c1 = jenv->FindClass("com/vanta/test/BenchObj");
    assert(c1 != nullptr);
    jclass c2 = jenv->FindClass("com/vanta/test/BenchObj");
    assert(jenv->IsSameObject(c1, c2) == JNI_TRUE);
    jenv->DeleteLocalRef(c2);

    jclass gcls = static_cast<jclass>(jenv->NewGlobalRef(c1));
    assert(gcls != nullptr);
    assert(jenv->IsSameObject(c1, gcls) == JNI_TRUE);

    jmethodID mid_add = jenv->GetMethodID(gcls, "add", "(II)I");
    jmethodID mid_add2 = jenv->GetMethodID(gcls, "add", "(II)I");
    assert(mid_add != nullptr && mid_add == mid_add2);
    jmethodID mid_mul = jenv->GetStaticMethodID(gcls, "mul", "(II)I");
    assert(mid_mul != nullptr);
    assert(jenv->CallStaticIntMethod(gcls, mid_mul, 6, 7) == 42);

    auto inst = std::make_shared<BenchObj>();
    inst->clazz = bench_cls;
    jobject jobj = reinterpret_cast<jobject>(inst.get());
    jobject gobj = jenv->NewGlobalRef(jobj);
    assert(jenv->CallIntMethod(jobj, mid_add, 10, 20) == 37);
    assert(jenv->CallIntMethod(gobj, mid_add, 10, 20) == 37);

#ifdef EnableJNIVMGC
    const size_t before_locals = nenv->localframe.front().size();
#endif
    for (int i = 0; i < 10000; ++i) {
        assert(jenv->CallIntMethod(gobj, mid_add, i, 1) == 7 + i + 1);
    }
#ifdef EnableJNIVMGC
    // Stock libjnivm leaks 10,000 Class shared_ptrs into localframe.front() here;
    // our rocket layer must leak ZERO.
    assert(nenv->localframe.front().size() == before_locals);
#endif

    // Strings
    jstring js = jenv->NewStringUTF("hello-vanta");
    assert(jenv->GetStringUTFLength(js) == 11);
    jboolean is_copy = JNI_TRUE;
    const char* chars = jenv->GetStringUTFChars(js, &is_copy);
    assert(is_copy == JNI_FALSE);
    assert(std::strcmp(chars, "hello-vanta") == 0);
    jenv->ReleaseStringUTFChars(js, chars);
    jenv->DeleteLocalRef(js);

    // Primitive arrays (single-alloc, zero-init, Get/SetRegion, GetElements, Critical)
    jbyteArray ba = jenv->NewByteArray(64);
    assert(jenv->GetArrayLength(ba) == 64);
    jbyte* bptr = jenv->GetByteArrayElements(ba, &is_copy);
    assert(bptr != nullptr && is_copy == JNI_FALSE && bptr[0] == 0 && bptr[63] == 0);
    const jbyte sample[4] = {11, 22, 33, 44};
    jenv->SetByteArrayRegion(ba, 4, 4, sample);
    jbyte roundtrip[4]{};
    jenv->GetByteArrayRegion(ba, 4, 4, roundtrip);
    assert(std::memcmp(sample, roundtrip, 4) == 0);
    void* crit = jenv->GetPrimitiveArrayCritical(ba, &is_copy);
    assert(crit == bptr && is_copy == JNI_FALSE);
    jenv->ReleasePrimitiveArrayCritical(ba, crit, 0);
    jenv->ReleaseByteArrayElements(ba, bptr, 0);
    jenv->DeleteLocalRef(ba);

    // DirectByteBuffer
    alignas(16) unsigned char raw_buf[32]{};
    auto bb = std::make_shared<jnivm::ByteBuffer>(raw_buf, 32);
    jobject jbb = reinterpret_cast<jobject>(bb.get());
    assert(jenv->GetDirectBufferAddress(jbb) == raw_buf);
    assert(jenv->GetDirectBufferCapacity(jbb) == 32);

    jenv->DeleteGlobalRef(gobj);
    jenv->DeleteGlobalRef(gcls);
    jenv->DeleteLocalRef(c1);
}

double cpu_ns() {
    timespec ts{};
    assert(clock_gettime(CLOCK_PROCESS_CPUTIME_ID, &ts) == 0);
    return ts.tv_sec * 1e9 + ts.tv_nsec;
}

void benchmark(JavaVM* vm, unsigned threads) {
    constexpr unsigned iterations = 500000;
    std::vector<std::thread> workers;
    std::atomic<unsigned> ready{0};
    std::atomic<bool> start{false};
    for (unsigned i = 0; i < threads; ++i) {
        workers.emplace_back([&] {
            const auto* expected = vanta_jni_thread_env();
            assert(expected);
            ++ready;
            while (!start.load()) {
                std::this_thread::yield();
            }
            for (unsigned n = 0; n < iterations; ++n) {
                void* env = nullptr;
                assert(vm->GetEnv(&env, JNI_VERSION_1_6) == JNI_OK && env == expected);
                auto* native_env = static_cast<JNIEnv*>(vanta_jni_thread_env());
                assert(native_env == expected);
                JavaVM* from_env = nullptr;
                assert(native_env->GetJavaVM(&from_env) == JNI_OK && from_env);
            }
            assert(vm->DetachCurrentThread() == JNI_OK);
        });
    }
    while (ready.load() != threads) {
        std::this_thread::yield();
    }
    const auto t0 = std::chrono::steady_clock::now();
    const auto cpu0 = cpu_ns();
    start.store(true);
    for (auto& worker : workers) {
        worker.join();
    }
    const double cpu = cpu_ns() - cpu0;
    const double elapsed = std::chrono::duration<double, std::nano>(
        std::chrono::steady_clock::now() - t0).count();
    const double calls = 3.0 * threads * iterations;
    std::printf("%u threads: %.2f CPU ns/call, %.2f million calls/s\n",
                threads, cpu / calls, calls * 1000 / elapsed);
}

void benchmark_hot_jni() {
    struct HotObj : jnivm::Object {
        jfloat getAxisValue(jint axis, jint pointer) { return static_cast<jfloat>(axis + pointer) * 0.5f; }
    };
    auto* jenv = static_cast<JNIEnv*>(vanta_jni_thread_env());
    auto* nenv = jnivm::ENV::FromJNIEnv(jenv);
    auto cls = nenv->GetClass<HotObj>("com/vanta/test/HotObj");
    cls->HookInstance(nenv, "getAxisValue", &HotObj::getAxisValue);
    jclass jcls = jenv->FindClass("com/vanta/test/HotObj");
    jmethodID mid = jenv->GetMethodID(jcls, "getAxisValue", "(II)F");
    auto inst = std::make_shared<HotObj>();
    inst->clazz = cls;
    jobject gobj = jenv->NewGlobalRef(reinterpret_cast<jobject>(inst.get()));

    constexpr int N = 200000;
    volatile float sink = 0.0f;
    const auto t0 = std::chrono::steady_clock::now();
    for (int i = 0; i < N; ++i) {
        sink += jenv->CallFloatMethod(gobj, mid, i & 7, 0);
    }
    const double call_ns = std::chrono::duration<double, std::nano>(std::chrono::steady_clock::now() - t0).count() / N;

    const auto t1 = std::chrono::steady_clock::now();
    for (int i = 0; i < N; ++i) {
        jbyteArray a = jenv->NewByteArray(128);
        jenv->DeleteLocalRef(a);
    }
    const double arr_ns = std::chrono::duration<double, std::nano>(std::chrono::steady_clock::now() - t1).count() / N;

    jstring js = jenv->NewStringUTF("MotionEvent");
    const auto t2 = std::chrono::steady_clock::now();
    for (int i = 0; i < N; ++i) {
        const char* p = jenv->GetStringUTFChars(js, nullptr);
        sink += static_cast<float>(p[0]);
        jenv->ReleaseStringUTFChars(js, p);
    }
    const double str_ns = std::chrono::duration<double, std::nano>(std::chrono::steady_clock::now() - t2).count() / N;

    std::printf("hot JNI (%s): CallFloatMethod=%.1f ns, NewByteArray+DeleteLocalRef=%.1f ns, GetStringUTFChars=%.1f ns (sink=%.1f)\n",
                getenv("VANTA_NO_JNI_FAST") ? "stock" : "fast", call_ns, arr_ns, str_ns, sink);
    jenv->DeleteLocalRef(js);
    jenv->DeleteGlobalRef(gobj);
    jenv->DeleteLocalRef(jcls);
}
} // namespace

int main(int argc, char** argv) {
    auto* vm = static_cast<JavaVM*>(vanta_jni_create_vm());
    assert(vm);
    if (argc == 2 && std::strcmp(argv[1], "--bench") == 0) {
        for (unsigned threads : {1, 4, 8}) {
            benchmark(vm, threads);
        }
        benchmark_hot_jni();
    } else {
        check(vm);
    }
}
