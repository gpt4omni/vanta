// `getaddrinfo` across the bionic/glibc boundary, with a thread-safe positive,
// negative, port-agnostic, singleflight DNS cache.
//
// This is the same class of defect as `struct stat` and `pthread_mutex_t`, and
// it belongs on that list: **bionic and glibc disagree about `struct addrinfo`,
// and they disagree about the `AI_*` constants.**
//
// Layout, from the two headers in this tree:
//
//     bionic                          glibc
//     int       ai_flags;             int       ai_flags;
//     int       ai_family;            int       ai_family;
//     int       ai_socktype;          int       ai_socktype;
//     int       ai_protocol;          int       ai_protocol;
//     socklen_t ai_addrlen;           socklen_t ai_addrlen;
//     char*     ai_canonname;   <-->  sockaddr* ai_addr;
//     sockaddr* ai_addr;        <-->  char*     ai_canonname;
//     addrinfo* ai_next;              addrinfo* ai_next;
//
// The two pointers are swapped. Roblox is compiled against the left column, so
// reading `ai_addr` off a glibc result hands it the canonical *name* — a
// NUL-terminated hostname — which it then reads as a `sockaddr`. `sa_family`
// comes out as the first two ASCII bytes of the hostname, matches no address
// family, and every address is discarded.
//
// The constants are worse, because they break the call before it runs:
//
//     AI_NUMERICSERV   bionic 0x0008    glibc 0x0400
//     AI_ALL           bionic 0x0100    glibc 0x0010
//     AI_V4MAPPED_CFG  bionic 0x0200    glibc —
//     AI_ADDRCONFIG    bionic 0x0400    glibc 0x0020
//     AI_V4MAPPED      bionic 0x0800    glibc 0x0008
//
// bionic's `AI_DEFAULT` is `AI_V4MAPPED_CFG|AI_ADDRCONFIG` = 0x600. glibc
// validates `ai_flags` against its own mask and answers `EAI_BADFLAGS` for the
// unknown 0x200 bit, so the lookup fails outright — which is exactly what the
// engine reported, on every request, for every host:
//
//     [DFLog::HttpTraceError] CURLOPT_ERRORBUFFER: Could not resolve host: apis.roblox.com
//
// `struct hostent` is identical in both and needs no translation.
//
// Why we cache here (positive + negative + singleflight):
// Roblox's bundled libcurl only caches positive resolutions. When telemetry
// hosts (`ecsv2.roblox.com`, `ephemeralcounters.api.roblox.com`) are blocked by
// Pi-hole/AdGuard/hosts/DNS, live logs showed 2,681 failed `getaddrinfo` calls
// in 81 seconds (~33/s, ~20-58 ms each = >100 thread-seconds burned starving
// asset download workers). Negative-caching NXDOMAIN/EAI_FAIL for 30 s (and
// transient EAI_AGAIN for 3 s), positive-caching for 60 s (port-agnostic for
// numeric ports), and coalescing concurrent identical lookups via singleflight
// turns thousands of 50 ms NSS stalls into sub-microsecond cache hits.

#include <atomic>
#include <cctype>
#include <condition_variable>
#include <cstddef>
#include <cstdint>
#include <cstdlib>
#include <cstring>
#include <ctime>
#include <memory>
#include <mutex>
#include <shared_mutex>
#include <string>
#include <unordered_map>
#include <vector>

#include <arpa/inet.h>
#include <netdb.h>
#include <netinet/in.h>
#include <sys/socket.h>

namespace {

/// `struct addrinfo` exactly as Roblox was compiled to see it.
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

// bionic's AI_* values. Named here rather than included, because this file is
// compiled against glibc's headers and the whole point is that the two differ.
constexpr int B_AI_PASSIVE = 0x0001;
constexpr int B_AI_CANONNAME = 0x0002;
constexpr int B_AI_NUMERICHOST = 0x0004;
constexpr int B_AI_NUMERICSERV = 0x0008;
constexpr int B_AI_ALL = 0x0100;
constexpr int B_AI_V4MAPPED_CFG = 0x0200;
constexpr int B_AI_ADDRCONFIG = 0x0400;
constexpr int B_AI_V4MAPPED = 0x0800;

constexpr uint64_t NS_PER_SEC = 1000000000ULL;
constexpr uint64_t POSITIVE_TTL_NS = 60ULL * NS_PER_SEC;
constexpr uint64_t NEGATIVE_TTL_NS = 30ULL * NS_PER_SEC;
constexpr uint64_t TRANSIENT_TTL_NS = 3ULL * NS_PER_SEC;
constexpr size_t MAX_CACHE_ENTRIES = 256;

int flags_to_host(int b) {
    int g = 0;
    if (b & B_AI_PASSIVE) g |= AI_PASSIVE;
    if (b & B_AI_CANONNAME) g |= AI_CANONNAME;
    if (b & B_AI_NUMERICHOST) g |= AI_NUMERICHOST;
    if (b & B_AI_NUMERICSERV) g |= AI_NUMERICSERV;
    if (b & B_AI_ALL) g |= AI_ALL;
    if (b & B_AI_ADDRCONFIG) g |= AI_ADDRCONFIG;
    // Both of bionic's V4-mapped bits mean the same thing to glibc; the `_CFG`
    // spelling is "if the kernel supports it", which on Linux it does.
    if (b & (B_AI_V4MAPPED | B_AI_V4MAPPED_CFG)) g |= AI_V4MAPPED;
    return g;
}

/// glibc's `EAI_*` are negative and bionic's are positive; nothing about the
/// numbering survives the trip, so a caller checking for `EAI_NONAME` needs the
/// value it was compiled against.
int eai_to_bionic(int g) {
    switch (g) {
        case EAI_BADFLAGS: return 3;
        case EAI_NONAME: return 8;
        case EAI_AGAIN: return 2;
        case EAI_FAIL: return 4;
        case EAI_FAMILY: return 5;
        case EAI_SOCKTYPE: return 10;
        case EAI_SERVICE: return 9;
        case EAI_MEMORY: return 6;
        case EAI_SYSTEM: return 11;
        case EAI_OVERFLOW: return 14;
        case EAI_NODATA: return 7;
        case EAI_ADDRFAMILY: return 1;
        default: return g == 0 ? 0 : 4;
    }
}

void free_bionic_list(BionicAddrinfo* p) {
    while (p) {
        BionicAddrinfo* next = p->ai_next;
        std::free(p->ai_addr);
        std::free(p->ai_canonname);
        std::free(p);
        p = next;
    }
}

uint64_t monotonic_ns() {
    struct timespec ts {};
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return static_cast<uint64_t>(ts.tv_sec) * NS_PER_SEC + static_cast<uint64_t>(ts.tv_nsec);
}

bool dns_cache_disabled() {
    static const bool disabled = []() {
        const char* v = std::getenv("VANTA_NO_DNS_CACHE");
        return v && v[0] != '\0' && std::strcmp(v, "0") != 0;
    }();
    return disabled;
}

bool parse_numeric_port(const char* service, uint16_t* out_port) {
    if (!service || service[0] == '\0') {
        *out_port = 0;
        return true;
    }
    uint32_t acc = 0;
    for (const unsigned char* p = reinterpret_cast<const unsigned char*>(service); *p; ++p) {
        if (*p < '0' || *p > '9') return false;
        acc = acc * 10u + static_cast<uint32_t>(*p - '0');
        if (acc > 65535u) return false;
    }
    *out_port = static_cast<uint16_t>(acc);
    return true;
}

struct CacheKey {
    std::string node;
    std::string service; // empty when port-agnostic
    int family = 0;
    int socktype = 0;
    int protocol = 0;
    int host_flags = 0;

    bool operator==(const CacheKey& o) const {
        return family == o.family && socktype == o.socktype && protocol == o.protocol &&
               host_flags == o.host_flags && node == o.node && service == o.service;
    }
};

struct CacheKeyHash {
    size_t operator()(const CacheKey& k) const {
        size_t h = std::hash<std::string>{}(k.node);
        auto mix = [&](size_t v) { h ^= v + 0x9e3779b97f4a7c15ULL + (h << 6) + (h >> 2); };
        mix(std::hash<std::string>{}(k.service));
        mix(static_cast<size_t>(k.family));
        mix(static_cast<size_t>(k.socktype));
        mix(static_cast<size_t>(k.protocol));
        mix(static_cast<size_t>(k.host_flags));
        return h;
    }
};

struct CachedNode {
    int ai_family = 0;
    int ai_socktype = 0;
    int ai_protocol = 0;
    socklen_t ai_addrlen = 0;
    struct sockaddr_storage addr {};
    std::string canonname;
    bool has_canonname = false;
};

struct CacheEntry {
    int bionic_rc = 0;
    uint64_t expires_ns = 0;
    uint64_t inserted_ns = 0;
    std::vector<CachedNode> nodes;
};

struct InFlight {
    std::mutex mu;
    std::condition_variable cv;
    bool done = false;
};

struct DnsCache {
    std::shared_mutex mu;
    std::unordered_map<CacheKey, CacheEntry, CacheKeyHash> entries;

    std::mutex inflight_mu;
    std::unordered_map<CacheKey, std::shared_ptr<InFlight>, CacheKeyHash> inflight;

    std::atomic<uint64_t> hits_pos{0};
    std::atomic<uint64_t> hits_neg{0};
    std::atomic<uint64_t> misses{0};
    std::atomic<uint64_t> coalesced{0};
};

DnsCache& dns_cache() {
    static DnsCache cache;
    return cache;
}

BionicAddrinfo* clone_from_cached(const std::vector<CachedNode>& nodes,
                                  bool stamp_port, uint16_t port_host_order,
                                  int* out_err) {
    BionicAddrinfo* head = nullptr;
    BionicAddrinfo** tail = &head;
    const uint16_t port_be = htons(port_host_order);
    for (const auto& n : nodes) {
        auto* b = static_cast<BionicAddrinfo*>(std::calloc(1, sizeof(BionicAddrinfo)));
        if (!b) {
            free_bionic_list(head);
            *out_err = 6; // bionic EAI_MEMORY
            return nullptr;
        }
        b->ai_flags = 0;
        b->ai_family = n.ai_family;
        b->ai_socktype = n.ai_socktype;
        b->ai_protocol = n.ai_protocol;
        b->ai_addrlen = n.ai_addrlen;
        if (n.ai_addrlen > 0) {
            b->ai_addr = static_cast<struct sockaddr*>(std::malloc(n.ai_addrlen));
            if (!b->ai_addr) {
                std::free(b);
                free_bionic_list(head);
                *out_err = 6;
                return nullptr;
            }
            std::memcpy(b->ai_addr, &n.addr, n.ai_addrlen);
            if (stamp_port) {
                if (n.ai_family == AF_INET && n.ai_addrlen >= sizeof(struct sockaddr_in)) {
                    reinterpret_cast<struct sockaddr_in*>(b->ai_addr)->sin_port = port_be;
                } else if (n.ai_family == AF_INET6 && n.ai_addrlen >= sizeof(struct sockaddr_in6)) {
                    reinterpret_cast<struct sockaddr_in6*>(b->ai_addr)->sin6_port = port_be;
                }
            }
        }
        if (n.has_canonname) {
            b->ai_canonname = strdup(n.canonname.c_str());
        }
        *tail = b;
        tail = &b->ai_next;
    }
    *out_err = 0;
    return head;
}

BionicAddrinfo* convert_host_list(struct addrinfo* out,
                                  std::vector<CachedNode>* capture,
                                  int* out_err) {
    BionicAddrinfo* head = nullptr;
    BionicAddrinfo** tail = &head;
    for (struct addrinfo* g = out; g; g = g->ai_next) {
        auto* b = static_cast<BionicAddrinfo*>(std::calloc(1, sizeof(BionicAddrinfo)));
        if (!b) {
            free_bionic_list(head);
            *out_err = 6; // bionic EAI_MEMORY
            return nullptr;
        }
        b->ai_flags = 0;
        b->ai_family = g->ai_family;
        b->ai_socktype = g->ai_socktype;
        b->ai_protocol = g->ai_protocol;
        b->ai_addrlen = g->ai_addrlen;
        if (g->ai_addr && g->ai_addrlen > 0) {
            b->ai_addr = static_cast<struct sockaddr*>(std::malloc(g->ai_addrlen));
            if (b->ai_addr) {
                std::memcpy(b->ai_addr, g->ai_addr, g->ai_addrlen);
            } else {
                b->ai_addrlen = 0;
            }
        }
        if (g->ai_canonname) {
            b->ai_canonname = strdup(g->ai_canonname);
        }
        if (capture && g->ai_addrlen <= sizeof(struct sockaddr_storage)) {
            CachedNode cn;
            cn.ai_family = g->ai_family;
            cn.ai_socktype = g->ai_socktype;
            cn.ai_protocol = g->ai_protocol;
            cn.ai_addrlen = g->ai_addrlen;
            if (g->ai_addr && g->ai_addrlen > 0) {
                std::memcpy(&cn.addr, g->ai_addr, g->ai_addrlen);
            }
            if (g->ai_canonname) {
                cn.has_canonname = true;
                cn.canonname = g->ai_canonname;
            }
            capture->push_back(std::move(cn));
        }
        *tail = b;
        tail = &b->ai_next;
    }
    *out_err = 0;
    return head;
}

uint64_t ttl_for_glibc_rc(int glibc_rc) {
    if (glibc_rc == 0) return POSITIVE_TTL_NS;
    switch (glibc_rc) {
        case EAI_NONAME:
        case EAI_NODATA:
        case EAI_FAIL:
            return NEGATIVE_TTL_NS;
        case EAI_AGAIN:
            return TRANSIENT_TTL_NS;
        default:
            return 0; // do not cache EAI_BADFLAGS, EAI_MEMORY, EAI_SYSTEM, etc.
    }
}

bool try_cache_lookup(const CacheKey& key, bool port_agnostic, uint16_t numeric_port,
                      BionicAddrinfo** res, int* out_rc) {
    auto& cache = dns_cache();
    const uint64_t now = monotonic_ns();
    std::shared_lock<std::shared_mutex> lk(cache.mu);
    auto it = cache.entries.find(key);
    if (it == cache.entries.end() || now >= it->second.expires_ns) {
        return false;
    }
    if (it->second.bionic_rc != 0) {
        cache.hits_neg.fetch_add(1, std::memory_order_relaxed);
        if (res) *res = nullptr;
        *out_rc = it->second.bionic_rc;
        return true;
    }
    int err = 0;
    BionicAddrinfo* head = clone_from_cached(it->second.nodes, port_agnostic, numeric_port, &err);
    if (err != 0) {
        if (res) *res = nullptr;
        *out_rc = err;
        return true;
    }
    cache.hits_pos.fetch_add(1, std::memory_order_relaxed);
    if (res) *res = head; else free_bionic_list(head);
    *out_rc = 0;
    return true;
}

void store_cache_entry(const CacheKey& key, CacheEntry&& entry) {
    auto& cache = dns_cache();
    const uint64_t now = monotonic_ns();
    std::unique_lock<std::shared_mutex> lk(cache.mu);
    if (cache.entries.size() >= MAX_CACHE_ENTRIES && cache.entries.find(key) == cache.entries.end()) {
        // First sweep expired entries; if still full, evict the oldest inserted entry.
        for (auto it = cache.entries.begin(); it != cache.entries.end(); ) {
            if (now >= it->second.expires_ns) {
                it = cache.entries.erase(it);
            } else {
                ++it;
            }
        }
        if (cache.entries.size() >= MAX_CACHE_ENTRIES) {
            auto oldest = cache.entries.begin();
            for (auto it = cache.entries.begin(); it != cache.entries.end(); ++it) {
                if (it->second.inserted_ns < oldest->second.inserted_ns) {
                    oldest = it;
                }
            }
            if (oldest != cache.entries.end()) {
                cache.entries.erase(oldest);
            }
        }
    }
    cache.entries[key] = std::move(entry);
}

} // namespace

extern "C" {

/// bionic-layout `getaddrinfo`, forwarding to the host resolver with a
/// thread-safe positive/negative/singleflight DNS cache.
int vanta_getaddrinfo(const char* node, const char* service,
                        const BionicAddrinfo* hints, BionicAddrinfo** res) {
    struct addrinfo host_hints;
    struct addrinfo* host_hints_p = nullptr;
    int bionic_flags = 0;
    if (hints) {
        std::memset(&host_hints, 0, sizeof host_hints);
        bionic_flags = hints->ai_flags;
        host_hints.ai_flags = flags_to_host(bionic_flags);
        // AF_*, SOCK_* and IPPROTO_* are kernel constants and agree.
        host_hints.ai_family = hints->ai_family;
        host_hints.ai_socktype = hints->ai_socktype;
        host_hints.ai_protocol = hints->ai_protocol;
        host_hints_p = &host_hints;
    }

    const bool cacheable = !dns_cache_disabled() &&
                           node != nullptr && node[0] != '\0' &&
                           (bionic_flags & (B_AI_NUMERICHOST | B_AI_PASSIVE)) == 0;

    if (!cacheable) {
        struct addrinfo* out = nullptr;
        int rc = ::getaddrinfo(node, service, host_hints_p, &out);
        if (rc != 0) {
            if (res) *res = nullptr;
            return eai_to_bionic(rc);
        }
        int err = 0;
        BionicAddrinfo* head = convert_host_list(out, nullptr, &err);
        ::freeaddrinfo(out);
        if (err != 0) {
            if (res) *res = nullptr;
            return err;
        }
        if (res) *res = head; else free_bionic_list(head);
        return 0;
    }

    uint16_t numeric_port = 0;
    const bool port_agnostic = parse_numeric_port(service, &numeric_port);

    CacheKey key;
    key.node = node;
    if (!port_agnostic && service) {
        key.service = service;
    }
    if (host_hints_p) {
        key.family = host_hints.ai_family;
        key.socktype = host_hints.ai_socktype;
        key.protocol = host_hints.ai_protocol;
        key.host_flags = host_hints.ai_flags & ~AI_NUMERICSERV;
    }

    int cached_rc = 0;
    if (try_cache_lookup(key, port_agnostic, numeric_port, res, &cached_rc)) {
        return cached_rc;
    }

    // Singleflight: if another thread is already resolving this exact key,
    // wait for it to populate the cache instead of issuing duplicate NSS RPCs.
    auto& cache = dns_cache();
    std::shared_ptr<InFlight> leader;
    bool is_leader = false;
    {
        std::lock_guard<std::mutex> lk(cache.inflight_mu);
        auto it = cache.inflight.find(key);
        if (it != cache.inflight.end()) {
            leader = it->second;
        } else {
            leader = std::make_shared<InFlight>();
            cache.inflight.emplace(key, leader);
            is_leader = true;
        }
    }

    if (!is_leader) {
        cache.coalesced.fetch_add(1, std::memory_order_relaxed);
        std::unique_lock<std::mutex> lk(leader->mu);
        leader->cv.wait(lk, [&] { return leader->done; });
        if (try_cache_lookup(key, port_agnostic, numeric_port, res, &cached_rc)) {
            return cached_rc;
        }
        // Uncacheable error from leader; fall through to direct lookup.
    }

    cache.misses.fetch_add(1, std::memory_order_relaxed);
    struct addrinfo* out = nullptr;
    int rc = ::getaddrinfo(node, service, host_hints_p, &out);

    const uint64_t ttl = ttl_for_glibc_rc(rc);
    int bionic_rc = (rc == 0) ? 0 : eai_to_bionic(rc);
    BionicAddrinfo* head = nullptr;

    if (rc == 0) {
        std::vector<CachedNode> captured;
        int err = 0;
        head = convert_host_list(out, ttl > 0 ? &captured : nullptr, &err);
        ::freeaddrinfo(out);
        if (err != 0) {
            bionic_rc = err;
        } else if (ttl > 0) {
            const uint64_t now = monotonic_ns();
            CacheEntry entry;
            entry.bionic_rc = 0;
            entry.inserted_ns = now;
            entry.expires_ns = now + ttl;
            entry.nodes = std::move(captured);
            store_cache_entry(key, std::move(entry));
        }
    } else if (ttl > 0) {
        const uint64_t now = monotonic_ns();
        CacheEntry entry;
        entry.bionic_rc = bionic_rc;
        entry.inserted_ns = now;
        entry.expires_ns = now + ttl;
        store_cache_entry(key, std::move(entry));
    }

    if (is_leader) {
        {
            std::lock_guard<std::mutex> lk(cache.inflight_mu);
            cache.inflight.erase(key);
        }
        {
            std::lock_guard<std::mutex> lk(leader->mu);
            leader->done = true;
        }
        leader->cv.notify_all();
    }

    if (bionic_rc != 0) {
        if (res) *res = nullptr;
        return bionic_rc;
    }
    if (res) *res = head; else free_bionic_list(head);
    return 0;
}

void vanta_freeaddrinfo(BionicAddrinfo* p) {
    free_bionic_list(p);
}

void vanta_netdb_cache_clear() {
    auto& cache = dns_cache();
    std::unique_lock<std::shared_mutex> lk(cache.mu);
    cache.entries.clear();
    cache.hits_pos.store(0, std::memory_order_relaxed);
    cache.hits_neg.store(0, std::memory_order_relaxed);
    cache.misses.store(0, std::memory_order_relaxed);
    cache.coalesced.store(0, std::memory_order_relaxed);
}

void vanta_netdb_cache_stats(uint64_t* hits_pos, uint64_t* hits_neg,
                             uint64_t* misses, uint64_t* coalesced) {
    auto& cache = dns_cache();
    if (hits_pos) *hits_pos = cache.hits_pos.load(std::memory_order_relaxed);
    if (hits_neg) *hits_neg = cache.hits_neg.load(std::memory_order_relaxed);
    if (misses) *misses = cache.misses.load(std::memory_order_relaxed);
    if (coalesced) *coalesced = cache.coalesced.load(std::memory_order_relaxed);
}

} // extern "C"

extern "C" struct VantaNetdbSymbol {
    const char* name;
    void* addr;
};

extern "C" const VantaNetdbSymbol* vanta_netdb_symbols(size_t* count) {
    static const VantaNetdbSymbol table[] = {
        {"getaddrinfo", (void*)&vanta_getaddrinfo},
        {"freeaddrinfo", (void*)&vanta_freeaddrinfo},
    };
    *count = sizeof(table) / sizeof(table[0]);
    return table;
}
