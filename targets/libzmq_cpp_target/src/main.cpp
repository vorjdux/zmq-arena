// zmq-arena target wrapper: libzmq (epoll + native C++ thread).
//
// Implements the unified target CLI (see ../../README.md) over the stable
// libzmq C API. The producer ("pub" role) and consumer ("sub" role) run as
// distinct processes spawned by the orchestrator. Latency is measured by the
// harness at the boundary; this wrapper is responsible only for honoring the
// pattern, payload size, message count, and the knobs it understands.
//
// Knob handling:
//   sndhwm / rcvhwm  -> ZMQ_SNDHWM / ZMQ_RCVHWM (applied below)
//   io_threads       -> zmq_ctx_set(ZMQ_IO_THREADS)
//   tcp_nodelay      -> accepted but a no-op: ZeroMQ disables Nagle by default.
//   unknown keys     -> ignored without error (contract requirement).

#include <zmq.h>

#include <cstdint>
#include <cstdlib>
#include <cstring>
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
    // pubsub uses PUB/SUB; throughput/fanout/fanin use PUSH/PULL. latency
    // (REQ/REP) is not yet driven by the orchestrator's runnable path.
    if (a.kind == "pubsub") return producer ? ZMQ_PUB : ZMQ_SUB;
    return producer ? ZMQ_PUSH : ZMQ_PULL;
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

    // Consumer binds, producer connects. Binding the receiver keeps the stable
    // endpoint on the measured side; either side may bind, this is a convention.
    if (producer) {
        if (zmq_connect(sock, a.endpoint.c_str()) != 0) die("zmq_connect");
    } else {
        if (zmq_bind(sock, a.endpoint.c_str()) != 0) die("zmq_bind");
        if (a.kind == "pubsub") {
            if (zmq_setsockopt(sock, ZMQ_SUBSCRIBE, "", 0) != 0) die("subscribe");
        }
    }

    const uint64_t total = a.warmup + a.messages;
    std::vector<char> buf(a.payload_bytes ? a.payload_bytes : 1, 0);

    if (producer) {
        for (uint64_t n = 0; n < total; ++n) {
            int rc = zmq_send(sock, buf.data(), a.payload_bytes, 0);
            if (rc < 0) die("zmq_send");
        }
    } else {
        std::vector<char> rx(buf.size());
        for (uint64_t n = 0; n < total; ++n) {
            int rc = zmq_recv(sock, rx.data(), rx.size(), 0);
            if (rc < 0) die("zmq_recv");
        }
    }

    zmq_close(sock);
    zmq_ctx_term(ctx);
    return 0;
}
