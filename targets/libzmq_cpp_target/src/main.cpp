// zmq-arena target wrapper: libzmq (epoll + native C++ thread).
//
// Implements the unified target CLI (see ../../README.md) over the stable
// libzmq C API. The producer ("pub" role) and consumer ("sub" role) run as
// distinct processes spawned by the orchestrator. For throughput and pub/sub
// the orchestrator times the run externally. For the latency kind the REQ
// client times each round-trip itself and prints the quantiles to stdout, which
// the orchestrator parses.
//
// Knob handling:
//   sndhwm / rcvhwm  -> ZMQ_SNDHWM / ZMQ_RCVHWM (applied below)
//   io_threads       -> zmq_ctx_set(ZMQ_IO_THREADS)
//   tcp_nodelay      -> accepted but a no-op: ZeroMQ disables Nagle by default.
//   unknown keys     -> ignored without error (contract requirement).

#include <zmq.h>

#include <algorithm>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <ctime>
#include <iostream>
#include <map>
#include <string>
#include <vector>

namespace {

struct Args {
    std::string role;       // "pub" | "sub"
    std::string kind = "throughput";  // throughput | latency | pubsub | fanout | fanin
    std::string transport;  // "tcp" | "ipc"
    std::string endpoint;
    std::string variant;    // accepted; libzmq has a single variant
    uint32_t payload_bytes = 0;
    uint32_t peers = 0;     // accepted; used by pubsub/fanout/fanin
    uint64_t messages = 0;
    uint64_t warmup = 0;
    bool bind = false;      // set on the binding side of a multi-peer kind
    double duration_secs = 0.0;  // measurement window for duration-based kinds
    std::map<std::string, std::string> knobs;
};

[[noreturn]] void die(const std::string& msg) {
    std::cerr << "libzmq_target: " << msg;
    if (errno != 0) std::cerr << " (" << zmq_strerror(zmq_errno()) << ")";
    std::cerr << std::endl;
    std::exit(1);
}

std::string take_value(int& i, int argc, char** argv, const char* flag) {
    if (i + 1 >= argc) die(std::string("missing value for ") + flag);
    return argv[++i];
}

Args parse(int argc, char** argv) {
    Args a;
    for (int i = 1; i < argc; ++i) {
        std::string f = argv[i];
        if (f == "--role") a.role = take_value(i, argc, argv, "--role");
        else if (f == "--kind") a.kind = take_value(i, argc, argv, "--kind");
        else if (f == "--variant") a.variant = take_value(i, argc, argv, "--variant");
        else if (f == "--peers")
            a.peers = static_cast<uint32_t>(std::stoul(take_value(i, argc, argv, "--peers")));
        else if (f == "--bind") a.bind = true;
        else if (f == "--duration-secs")
            a.duration_secs = std::stod(take_value(i, argc, argv, "--duration-secs"));
        else if (f == "--transport") a.transport = take_value(i, argc, argv, "--transport");
        else if (f == "--endpoint") a.endpoint = take_value(i, argc, argv, "--endpoint");
        else if (f == "--payload-bytes")
            a.payload_bytes = static_cast<uint32_t>(std::stoul(take_value(i, argc, argv, "--payload-bytes")));
        else if (f == "--messages")
            a.messages = std::stoull(take_value(i, argc, argv, "--messages"));
        else if (f == "--warmup")
            a.warmup = std::stoull(take_value(i, argc, argv, "--warmup"));
        else if (f == "--knob") {
            std::string kv = take_value(i, argc, argv, "--knob");
            auto eq = kv.find('=');
            if (eq == std::string::npos) die("knob must be key=value: " + kv);
            a.knobs[kv.substr(0, eq)] = kv.substr(eq + 1);
        } else {
            die("unknown flag: " + f);
        }
    }
    if (a.role.empty() || a.endpoint.empty()) die("--role and --endpoint are required");
    return a;
}

int knob_int(const Args& a, const char* key, int fallback) {
    auto it = a.knobs.find(key);
    return it == a.knobs.end() ? fallback : std::stoi(it->second);
}

void apply_hwm(void* sock, int opt, const Args& a, const char* key) {
    auto it = a.knobs.find(key);
    if (it == a.knobs.end()) return;
    int v = std::stoi(it->second);
    if (zmq_setsockopt(sock, opt, &v, sizeof(v)) != 0) die(std::string("setsockopt ") + key);
}

int socket_type(const Args& a) {
    const bool producer = (a.role == "pub");
    if (a.kind == "latency") return producer ? ZMQ_REQ : ZMQ_REP;
    if (a.kind == "pubsub") return producer ? ZMQ_PUB : ZMQ_SUB;
    // throughput, fanout, fanin
    return producer ? ZMQ_PUSH : ZMQ_PULL;
}

int64_t now_ns() {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return static_cast<int64_t>(ts.tv_sec) * 1000000000LL + ts.tv_nsec;
}

// REQ/REP latency. The REP server (consumer) echoes forever and is killed by
// the orchestrator. The REQ client (producer) times each round-trip and prints
// one line the orchestrator parses:
//   LATENCY <count> <min> <p50> <p90> <p99> <p999> <max>   (all nanoseconds)
void run_latency(void* sock, const Args& a, bool producer) {
    std::vector<char> buf(a.payload_bytes ? a.payload_bytes : 1, 0);
    std::vector<char> rx(buf.size());

    if (!producer) {
        for (;;) {
            if (zmq_recv(sock, rx.data(), rx.size(), 0) < 0) die("zmq_recv (rep)");
            if (zmq_send(sock, rx.data(), a.payload_bytes, 0) < 0) die("zmq_send (rep)");
        }
    }

    for (uint64_t n = 0; n < a.warmup; ++n) {
        if (zmq_send(sock, buf.data(), a.payload_bytes, 0) < 0) die("zmq_send (warmup)");
        if (zmq_recv(sock, rx.data(), rx.size(), 0) < 0) die("zmq_recv (warmup)");
    }

    std::vector<int64_t> lat;
    lat.reserve(a.messages);
    for (uint64_t n = 0; n < a.messages; ++n) {
        int64_t t0 = now_ns();
        if (zmq_send(sock, buf.data(), a.payload_bytes, 0) < 0) die("zmq_send (req)");
        if (zmq_recv(sock, rx.data(), rx.size(), 0) < 0) die("zmq_recv (req)");
        lat.push_back(now_ns() - t0);
    }

    if (lat.empty()) {
        std::cout << "LATENCY 0 0 0 0 0 0 0\n";
        return;
    }
    std::sort(lat.begin(), lat.end());
    auto q = [&](double p) {
        size_t idx = static_cast<size_t>(p * static_cast<double>(lat.size() - 1));
        return lat[idx];
    };
    std::cout << "LATENCY " << lat.size() << " " << lat.front() << " "
              << q(0.50) << " " << q(0.90) << " " << q(0.99) << " "
              << q(0.999) << " " << lat.back() << "\n";
    std::cout.flush();
}

// PUB/SUB duration-based throughput. The PUB sends forever (killed by the
// orchestrator). The SUB counts received messages for duration_secs, starting
// the clock on the first message, and prints:
//   THROUGHPUT <count> <elapsed_secs>
void run_pubsub_loop(void* sock, const Args& a, bool producer) {
    if (producer) {
        std::vector<char> buf(a.payload_bytes ? a.payload_bytes : 1, 0);
        for (;;) {
            zmq_send(sock, buf.data(), a.payload_bytes, 0);  // ignore transient errors
        }
    }

    // A receive timeout keeps the SUB from blocking past the window.
    int timeo_ms = 200;
    zmq_setsockopt(sock, ZMQ_RCVTIMEO, &timeo_ms, sizeof(timeo_ms));
    std::vector<char> rx(a.payload_bytes ? a.payload_bytes : 1);

    // Wait (bounded) for the first message, then start the clock.
    int64_t wait_start = now_ns();
    for (;;) {
        if (zmq_recv(sock, rx.data(), rx.size(), 0) >= 0) break;
        if (now_ns() - wait_start > 10LL * 1000000000LL) {
            std::printf("THROUGHPUT 0 0.000001\n");
            std::fflush(stdout);
            return;
        }
    }
    uint64_t count = 1;
    int64_t t0 = now_ns();
    int64_t deadline = t0 + static_cast<int64_t>(a.duration_secs * 1e9);
    while (now_ns() < deadline) {
        if (zmq_recv(sock, rx.data(), rx.size(), 0) >= 0) ++count;
        // a timeout (rc < 0) falls through to the deadline check
    }
    double elapsed = static_cast<double>(now_ns() - t0) / 1e9;
    std::printf("THROUGHPUT %llu %.6f\n", static_cast<unsigned long long>(count), elapsed);
    std::fflush(stdout);
}

}  // namespace

int main(int argc, char** argv) {
    Args a = parse(argc, argv);

    void* ctx = zmq_ctx_new();
    if (!ctx) die("zmq_ctx_new");
    if (zmq_ctx_set(ctx, ZMQ_IO_THREADS, knob_int(a, "io_threads", 1)) != 0)
        die("zmq_ctx_set IO_THREADS");

    void* sock = zmq_socket(ctx, socket_type(a));
    if (!sock) die("zmq_socket");

    apply_hwm(sock, ZMQ_SNDHWM, a, "sndhwm");
    apply_hwm(sock, ZMQ_RCVHWM, a, "rcvhwm");

    const bool producer = (a.role == "pub");

    // Bind side: pubsub is told explicitly by the orchestrator (--bind on the
    // PUB); throughput/latency keep the convention that the consumer binds.
    const bool do_bind = (a.kind == "pubsub") ? a.bind : !producer;
    if (do_bind) {
        if (zmq_bind(sock, a.endpoint.c_str()) != 0) die("zmq_bind");
    } else {
        if (zmq_connect(sock, a.endpoint.c_str()) != 0) die("zmq_connect");
    }
    if (a.kind == "pubsub" && !producer) {
        if (zmq_setsockopt(sock, ZMQ_SUBSCRIBE, "", 0) != 0) die("subscribe");
    }

    if (a.kind == "latency") {
        run_latency(sock, a, producer);
    } else if (a.kind == "pubsub") {
        run_pubsub_loop(sock, a, producer);
    } else {
        // Streaming throughput (PUSH/PULL): the producer sends the whole block,
        // the consumer receives it; the orchestrator times it.
        const uint64_t total = a.warmup + a.messages;
        std::vector<char> buf(a.payload_bytes ? a.payload_bytes : 1, 0);
        if (producer) {
            for (uint64_t n = 0; n < total; ++n)
                if (zmq_send(sock, buf.data(), a.payload_bytes, 0) < 0) die("zmq_send");
        } else {
            std::vector<char> rx(buf.size());
            for (uint64_t n = 0; n < total; ++n)
                if (zmq_recv(sock, rx.data(), rx.size(), 0) < 0) die("zmq_recv");
        }
    }

    zmq_close(sock);
    zmq_ctx_term(ctx);
    return 0;
}
