#include <jnivm.h>

#include <atomic>
#include <cassert>
#include <chrono>
#include <cstdio>
#include <cstring>
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
} // namespace

int main(int argc, char** argv) {
    auto* vm = static_cast<JavaVM*>(vanta_jni_create_vm());
    assert(vm);
    if (argc == 2 && std::strcmp(argv[1], "--bench") == 0) {
        for (unsigned threads : {1, 4, 8}) {
            benchmark(vm, threads);
        }
    } else {
        check(vm);
    }
}
