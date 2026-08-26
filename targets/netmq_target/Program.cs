// zmq-arena target wrapper for NetMQ, the pure C# ZMTP implementation.
//
// Not a binding: NetMQ implements the wire protocol in managed code, with no
// libzmq underneath, so it is measured as an implementation.
//
// The wrapper times its own steady-state window. That matters more on a JIT
// runtime than anywhere else: the CLR spends its first milliseconds
// interpreting and tiering up, and letting the harness time the process would
// charge startup and JIT compilation to the benchmark. The warmup messages the
// matrix already sends are what get the hot paths compiled before the clock
// starts.
using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.Threading;
using System.Text.Json;
using NetMQ;
using NetMQ.Sockets;

static class Target
{
    static string? Arg(string[] a, string name, string? dflt = null)
    {
        var i = Array.IndexOf(a, "--" + name);
        return i >= 0 && i + 1 < a.Length ? a[i + 1] : dflt;
    }

    static bool Flag(string[] a, string name) => Array.IndexOf(a, "--" + name) >= 0;

    static string? Knob(string[] a, string key)
    {
        for (var i = 0; i < a.Length - 1; i++)
        {
            if (a[i] != "--knob") continue;
            var kv = a[i + 1].Split('=', 2);
            if (kv.Length == 2 && kv[0] == key) return kv[1];
        }
        return Environment.GetEnvironmentVariable("ARENA_KNOB_" + key.ToUpperInvariant());
    }

    static void Describe()
    {
        var ver = typeof(NetMQSocket).Assembly.GetName().Version?.ToString() ?? "unknown";
        Console.WriteLine(JsonSerializer.Serialize(new
        {
            engine = "netmq",
            lib_version = ver,
            binding_version = (string?)null,
            lib_language = "C#",
            impl = "native",
            ffi_to = (string?)null,
            language = "C#",
            concurrency = "sync",
            threading = "native",
            io = "epoll",
        }));
    }

    // The arena forwards one knob superset to every target; apply what this
    // engine understands and ignore the rest.
    static void Tune(NetMQSocket s, string[] a)
    {
        if (int.TryParse(Knob(a, "sndhwm"), out var sh)) s.Options.SendHighWatermark = sh;
        if (int.TryParse(Knob(a, "rcvhwm"), out var rh)) s.Options.ReceiveHighWatermark = rh;
    }

    static void PrintLatency(List<long> rtts)
    {
        if (rtts.Count == 0) { Console.WriteLine("LATENCY 0 0 0 0 0 0 0"); return; }
        rtts.Sort();
        long Q(double p) => rtts[Math.Min((int)(rtts.Count * p), rtts.Count - 1)];
        Console.WriteLine($"LATENCY {rtts.Count} {rtts[0]} {Q(0.50)} {Q(0.90)} {Q(0.99)} {Q(0.999)} {rtts[^1]}");
    }

    static void TimedDrain(NetMQSocket s, double seconds)
    {
        s.ReceiveFrameBytes(); // first message: the link is live, clock starts after it
        long count = 1;
        var sw = Stopwatch.StartNew();
        var deadline = TimeSpan.FromSeconds(seconds);
        while (sw.Elapsed < deadline)
        {
            s.ReceiveFrameBytes();
            count++;
        }
        Console.WriteLine($"THROUGHPUT {count} {Math.Max(sw.Elapsed.TotalSeconds, 1e-9):F6}");
    }

    static int Main(string[] args)
    {
        if (args.Length > 0 && args[0] == "describe") { Describe(); return 0; }

        var role = Arg(args, "role")!;
        var kind = Arg(args, "kind", "throughput")!;
        var endpoint = Arg(args, "endpoint")!;
        var payloadBytes = int.Parse(Arg(args, "payload-bytes", "0")!);
        var messages = long.Parse(Arg(args, "messages", "0")!);
        var warmup = long.Parse(Arg(args, "warmup", "0")!);
        var duration = double.Parse(Arg(args, "duration-secs", "0")!);
        var bind = Flag(args, "bind");
        var payload = new byte[payloadBytes];
        Array.Fill(payload, (byte)'x');

        Console.Error.WriteLine($"netmq-target: role={role} kind={kind} ep={endpoint} " +
                                $"payload={payloadBytes}B msgs={messages}");

        switch (kind)
        {
            case "throughput":
                if (role == "sub")
                {
                    using var pull = new PullSocket();
                    Tune(pull, args);
                    pull.Bind(endpoint);
                    for (long i = 0; i < warmup; i++) pull.ReceiveFrameBytes();
                    var sw = Stopwatch.StartNew();
                    for (long i = 0; i < messages; i++) pull.ReceiveFrameBytes();
                    Console.WriteLine($"THROUGHPUT {messages} {Math.Max(sw.Elapsed.TotalSeconds, 1e-9):F6}");
                }
                else
                {
                    using var push = new PushSocket();
                    Tune(push, args);
                    push.Connect(endpoint);
                    for (long i = 0; i < messages + warmup; i++) push.SendFrame(payload);
                    // Let queued frames drain rather than dropping them, which
                    // the no-data-dropping rule forbids.
                    push.Options.Linger = TimeSpan.FromSeconds(5);
                }
                break;

            case "latency":
                if (role == "sub")
                {
                    using var rep = new ResponseSocket();
                    rep.Bind(endpoint);
                    try
                    {
                        while (true) rep.SendFrame(rep.ReceiveFrameBytes());
                    }
                    catch (TerminatingException) { /* the REQ side went away */ }
                }
                else
                {
                    using var req = new RequestSocket();
                    req.Connect(endpoint);
                    for (long i = 0; i < warmup; i++)
                    {
                        req.SendFrame(payload);
                        req.ReceiveFrameBytes();
                    }
                    var rtts = new List<long>((int)messages);
                    for (long i = 0; i < messages; i++)
                    {
                        var t = Stopwatch.GetTimestamp();
                        req.SendFrame(payload);
                        req.ReceiveFrameBytes();
                        rtts.Add((long)((Stopwatch.GetTimestamp() - t) *
                                        (1_000_000_000.0 / Stopwatch.Frequency)));
                    }
                    PrintLatency(rtts);
                }
                break;

            case "pubsub":
                if (role == "pub")
                {
                    using var pub = new PublisherSocket();
                    Tune(pub, args);
                    pub.Bind(endpoint);
                    // PUB drops what it sends before a subscriber has finished
                    // subscribing, so settle before flooding.
                    Thread.Sleep(500);
                    while (true) pub.SendFrame(payload);
                }
                else
                {
                    using var sub = new SubscriberSocket();
                    Tune(sub, args);
                    sub.Connect(endpoint);
                    sub.SubscribeToAnyTopic();
                    TimedDrain(sub, duration);
                }
                break;

            case "fanout":
            case "fanin":
                if (role == "pub")
                {
                    using var push = new PushSocket();
                    Tune(push, args);
                    if (bind) push.Bind(endpoint); else push.Connect(endpoint);
                    while (true) push.SendFrame(payload);
                }
                else
                {
                    using var pull = new PullSocket();
                    Tune(pull, args);
                    if (bind) pull.Bind(endpoint); else pull.Connect(endpoint);
                    TimedDrain(pull, duration);
                }
                break;

            default:
                Console.Error.WriteLine($"netmq-target: unsupported kind {kind}");
                return 1;
        }
        return 0;
    }
}
