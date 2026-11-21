// Unit tests and microbenchmark for native/netdb_compat.cpp DNS cache.
#include <cassert>
#include <chrono>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <thread>
#include <vector>

#include <arpa/inet.h>
#include <netinet/in.h>
#include <sys/socket.h>

struct BionicAddrinfo {
    int ai_flags;
    int ai_family;
    int ai_socktype;
    int ai_protocol;
    socklen_t ai_addrlen;
    char* ai_canonname;
    struct sockaddr* ai_addr;
    struct BionicAddrinfo* ai_next;
};

extern "C" {
int vanta_getaddrinfo(const char* node, const char* service,
                      const BionicAddrinfo* hints, BionicAddrinfo** res);
void vanta_freeaddrinfo(BionicAddrinfo* p);
void vanta_netdb_cache_clear();
void vanta_netdb_cache_stats(uint64_t* hits_pos, uint64_t* hits_neg,
                             uint64_t* misses, uint64_t* coalesced);
}

static uint16_t extract_port(const BionicAddrinfo* ai) {
    assert(ai && ai->ai_addr);
    if (ai->ai_family == AF_INET) {
        return ntohs(reinterpret_cast<const struct sockaddr_in*>(ai->ai_addr)->sin_port);
    }
    if (ai->ai_family == AF_INET6) {
        return ntohs(reinterpret_cast<const struct sockaddr_in6*>(ai->ai_addr)->sin6_port);
    }
    return 0;
}

static void test_positive_and_port_stamping() {
    vanta_netdb_cache_clear();
    BionicAddrinfo hints {};
    hints.ai_family = AF_INET;
    hints.ai_socktype = SOCK_STREAM;

    BionicAddrinfo* res1 = nullptr;
    int rc1 = vanta_getaddrinfo("localhost", "443", &hints, &res1);
    assert(rc1 == 0 && res1 != nullptr);
    assert(extract_port(res1) == 443);
    vanta_freeaddrinfo(res1);

    // Different numeric port must hit the same port-agnostic cache entry and stamp 8443.
    BionicAddrinfo* res2 = nullptr;
    int rc2 = vanta_getaddrinfo("localhost", "8443", &hints, &res2);
    assert(rc2 == 0 && res2 != nullptr);
    assert(extract_port(res2) == 8443);
    vanta_freeaddrinfo(res2);

    uint64_t hp = 0, hn = 0, m = 0, c = 0;
    vanta_netdb_cache_stats(&hp, &hn, &m, &c);
    assert(m == 1);
    assert(hp == 1);
    assert(hn == 0);
}

static void test_negative_cache_and_speed() {
    vanta_netdb_cache_clear();
    BionicAddrinfo hints {};
    hints.ai_family = AF_UNSPEC;
    hints.ai_socktype = SOCK_STREAM;

    // RFC 6761 `.invalid` TLD always resolves as NXDOMAIN / EAI_NONAME.
    const char* bad = "vanta-test-blocked-telemetry.invalid";
    BionicAddrinfo* res = nullptr;
    int rc0 = vanta_getaddrinfo(bad, "443", &hints, &res);
    assert(rc0 != 0 && res == nullptr);

    auto t0 = std::chrono::steady_clock::now();
    constexpr int N = 2000;
    for (int i = 0; i < N; ++i) {
        BionicAddrinfo* r = nullptr;
        int rc = vanta_getaddrinfo(bad, "443", &hints, &r);
        assert(rc == rc0 && r == nullptr);
    }
    auto elapsed_ns =
        std::chrono::duration_cast<std::chrono::nanoseconds>(std::chrono::steady_clock::now() - t0).count();

    uint64_t hp = 0, hn = 0, m = 0, c = 0;
    vanta_netdb_cache_stats(&hp, &hn, &m, &c);
    assert(m == 1);
    assert(hn == N);
    std::printf("  netdb_compat negative cache hit: %.1f ns/call (%d calls in %.3f ms)\n",
                double(elapsed_ns) / N, N, double(elapsed_ns) / 1e6);
}

static void test_concurrent_singleflight() {
    vanta_netdb_cache_clear();
    BionicAddrinfo hints {};
    hints.ai_family = AF_INET;
    hints.ai_socktype = SOCK_STREAM;

    constexpr int THREADS = 8;
    std::vector<std::thread> ts;
    for (int i = 0; i < THREADS; ++i) {
        ts.emplace_back([&]() {
            for (int j = 0; j < 200; ++j) {
                BionicAddrinfo* r = nullptr;
                int rc = vanta_getaddrinfo("localhost", "80", &hints, &r);
                assert(rc == 0 && r != nullptr);
                assert(extract_port(r) == 80);
                vanta_freeaddrinfo(r);
            }
        });
    }
    for (auto& t : ts) t.join();

    uint64_t hp = 0, hn = 0, m = 0, c = 0;
    vanta_netdb_cache_stats(&hp, &hn, &m, &c);
    assert(m == 1);
    assert(hp == THREADS * 200 - 1);
}

int main() {
    test_positive_and_port_stamping();
    test_negative_cache_and_speed();
    test_concurrent_singleflight();
    std::printf("vanta_netdb_compat_test: all passed\n");
    return 0;
}
