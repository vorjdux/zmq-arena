// zmq-arena target wrapper for JeroMQ, the pure Java ZMTP implementation.
//
// Not a binding: JeroMQ reimplements the protocol on the JVM, with no libzmq
// and no JNI, so it is measured as an implementation.
//
// The wrapper times its own steady-state window, which matters more here than
// for any other target so far. A JVM spends hundreds of milliseconds starting
// and runs interpreted until the JIT promotes the hot paths; letting the
// harness time the process would charge all of that to the benchmark and report
// it as the language being slow at messaging. The matrix's warmup messages are
// what get those paths compiled before the clock starts.
import java.util.ArrayList;
import java.util.Arrays;
import java.util.Collections;
import java.util.List;
import org.zeromq.SocketType;
import org.zeromq.ZContext;
import org.zeromq.ZMQ;

public final class Target {

    static String arg(String[] a, String name, String dflt) {
        int i = Arrays.asList(a).indexOf("--" + name);
        return (i >= 0 && i + 1 < a.length) ? a[i + 1] : dflt;
    }

    static boolean flag(String[] a, String name) {
        return Arrays.asList(a).contains("--" + name);
    }

    static String knob(String[] a, String key) {
        for (int i = 0; i < a.length - 1; i++) {
            if (!a[i].equals("--knob")) continue;
            String[] kv = a[i + 1].split("=", 2);
            if (kv.length == 2 && kv[0].equals(key)) return kv[1];
        }
        return System.getenv("ARENA_KNOB_" + key.toUpperCase());
    }

    static void describe() {
        // JeroMQ reports the libzmq protocol version it implements; its own
        // version is the artifact version baked in at build time.
        String ver = System.getProperty("arena.jeromq.version", "unknown");
        System.out.println("{\"engine\":\"jeromq\",\"lib_version\":\"" + ver + "\","
            + "\"binding_version\":null,\"lib_language\":\"Java\",\"impl\":\"native\","
            + "\"ffi_to\":null,\"language\":\"Java\",\"concurrency\":\"sync\","
            + "\"threading\":\"native\",\"io\":\"epoll\"}");
    }

    // The arena forwards one knob superset to every target; apply what this
    // engine understands and ignore the rest.
    static void tune(ZMQ.Socket s, String[] a) {
        String v;
        if ((v = knob(a, "sndhwm")) != null) s.setSndHWM(Integer.parseInt(v));
        if ((v = knob(a, "rcvhwm")) != null) s.setRcvHWM(Integer.parseInt(v));
    }

    static void printLatency(List<Long> rtts) {
        if (rtts.isEmpty()) {
            System.out.println("LATENCY 0 0 0 0 0 0 0");
            return;
        }
        Collections.sort(rtts);
        int n = rtts.size();
        System.out.println("LATENCY " + n + " " + rtts.get(0) + " "
            + rtts.get(Math.min((int) (n * 0.50), n - 1)) + " "
            + rtts.get(Math.min((int) (n * 0.90), n - 1)) + " "
            + rtts.get(Math.min((int) (n * 0.99), n - 1)) + " "
            + rtts.get(Math.min((int) (n * 0.999), n - 1)) + " "
            + rtts.get(n - 1));
    }

    static void timedDrain(ZMQ.Socket s, double seconds) {
        s.recv(0); // first message: the link is live, clock starts after it
        long count = 1;
        long t0 = System.nanoTime();
        long deadline = t0 + (long) (seconds * 1e9);
        while (System.nanoTime() < deadline) {
            s.recv(0);
            count++;
        }
        double secs = Math.max((System.nanoTime() - t0) / 1e9, 1e-9);
        System.out.printf("THROUGHPUT %d %.6f%n", count, secs);
    }

    public static void main(String[] args) {
        if (args.length > 0 && args[0].equals("describe")) {
            describe();
            return;
        }
        String role = arg(args, "role", "sub");
        String kind = arg(args, "kind", "throughput");
        String endpoint = arg(args, "endpoint", null);
        int payloadBytes = Integer.parseInt(arg(args, "payload-bytes", "0"));
        long messages = Long.parseLong(arg(args, "messages", "0"));
        long warmup = Long.parseLong(arg(args, "warmup", "0"));
        double duration = Double.parseDouble(arg(args, "duration-secs", "0"));
        boolean bind = flag(args, "bind");
        byte[] payload = new byte[payloadBytes];
        Arrays.fill(payload, (byte) 'x');

        System.err.println("jeromq-target: role=" + role + " kind=" + kind
            + " ep=" + endpoint + " payload=" + payloadBytes + "B msgs=" + messages);

        // One IO thread unless the matrix says otherwise: the arena gives every
        // engine the same lane budget rather than letting each size its own.
        String io = knob(args, "io_threads");
        try (ZContext ctx = new ZContext(io == null ? 1 : Integer.parseInt(io))) {
            switch (kind) {
                case "throughput": {
                    if (role.equals("sub")) {
                        ZMQ.Socket pull = ctx.createSocket(SocketType.PULL);
                        tune(pull, args);
                        pull.bind(endpoint);
                        for (long i = 0; i < warmup; i++) pull.recv(0);
                        long t0 = System.nanoTime();
                        for (long i = 0; i < messages; i++) pull.recv(0);
                        double secs = Math.max((System.nanoTime() - t0) / 1e9, 1e-9);
                        System.out.printf("THROUGHPUT %d %.6f%n", messages, secs);
                    } else {
                        ZMQ.Socket push = ctx.createSocket(SocketType.PUSH);
                        tune(push, args);
                        push.connect(endpoint);
                        for (long i = 0; i < messages + warmup; i++) push.send(payload, 0);
                        // Drain queued frames rather than dropping them, which
                        // the no-data-dropping rule forbids.
                        push.setLinger(5000);
                    }
                    break;
                }
                case "latency": {
                    if (role.equals("sub")) {
                        ZMQ.Socket rep = ctx.createSocket(SocketType.REP);
                        rep.bind(endpoint);
                        while (!Thread.currentThread().isInterrupted()) {
                            byte[] m = rep.recv(0);
                            if (m == null) break;
                            rep.send(m, 0);
                        }
                    } else {
                        ZMQ.Socket req = ctx.createSocket(SocketType.REQ);
                        req.connect(endpoint);
                        for (long i = 0; i < warmup; i++) {
                            req.send(payload, 0);
                            req.recv(0);
                        }
                        List<Long> rtts = new ArrayList<>((int) messages);
                        for (long i = 0; i < messages; i++) {
                            long t = System.nanoTime();
                            req.send(payload, 0);
                            req.recv(0);
                            rtts.add(System.nanoTime() - t);
                        }
                        printLatency(rtts);
                    }
                    break;
                }
                case "pubsub": {
                    if (role.equals("pub")) {
                        ZMQ.Socket pub = ctx.createSocket(SocketType.PUB);
                        tune(pub, args);
                        pub.bind(endpoint);
                        // PUB drops what it sends before a subscriber has
                        // finished subscribing, so settle before flooding.
                        Thread.sleep(500);
                        while (true) pub.send(payload, 0);
                    } else {
                        ZMQ.Socket sub = ctx.createSocket(SocketType.SUB);
                        tune(sub, args);
                        sub.connect(endpoint);
                        sub.subscribe(new byte[0]);
                        timedDrain(sub, duration);
                    }
                    break;
                }
                case "fanout":
                case "fanin": {
                    if (role.equals("pub")) {
                        ZMQ.Socket push = ctx.createSocket(SocketType.PUSH);
                        tune(push, args);
                        if (bind) push.bind(endpoint); else push.connect(endpoint);
                        while (true) push.send(payload, 0);
                    } else {
                        ZMQ.Socket pull = ctx.createSocket(SocketType.PULL);
                        tune(pull, args);
                        if (bind) pull.bind(endpoint); else pull.connect(endpoint);
                        timedDrain(pull, duration);
                    }
                    break;
                }
                default:
                    System.err.println("jeromq-target: unsupported kind " + kind);
                    System.exit(1);
            }
        } catch (InterruptedException e) {
            Thread.currentThread().interrupt();
        }
        System.out.flush();
    }
}
