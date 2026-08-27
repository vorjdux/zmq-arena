# zmq-arena target wrapper for omq.cr, the pure-Crystal ZMTP implementation.
#
# Not a binding: omq.cr speaks the wire protocol itself, with no libzmq, no FFI
# and no C extension. It is measured as an implementation rather than as a
# language access path, which is why `impl` is native below.
#
# The wrapper times its own steady-state window and prints one result line, the
# same contract every other target follows: letting the harness time the process
# would charge Crystal's startup to the measurement.
require "omq"

def arg(name : String, default : String? = nil) : String?
  i = ARGV.index("--#{name}")
  return default unless i
  ARGV[i + 1]? || default
end

def flag?(name : String) : Bool
  ARGV.includes?("--#{name}")
end

# The arena forwards one knob superset to every target; apply what this engine
# understands and ignore the rest, per the knob convention.
def knob(key : String) : String?
  ARGV.each_with_index do |a, i|
    next unless a == "--knob"
    if kv = ARGV[i + 1]?
      k, _, v = kv.partition('=')
      return v if k == key
    end
  end
  ENV["ARENA_KNOB_#{key.upcase}"]?
end

def send_hwm : Int32?
  (v = knob("sndhwm")) ? v.to_i : nil
end

def recv_hwm : Int32?
  (v = knob("rcvhwm")) ? v.to_i : nil
end

VARIANT = arg("variant", "default").not_nil!

if ARGV[0]? == "describe"
  # The engine is omq.cr itself: it implements ZMTP in Crystal rather than
  # binding anything. Grouping it with the omq family is the dashboard's job,
  # through the registry's `family` field, and does not belong in a target's
  # report of what it actually is.
  #
  # Crystal's runtime schedules fibers on one thread unless built with
  # preview_mt, and its event loop is epoll on Linux, so this reports single /
  # async / epoll the same way the Ruby sibling does.
  puts %({"engine":"omq.cr","lib_version":"#{OMQ::VERSION}","binding_version":null,) +
       %("lib_language":"Crystal","impl":"native","ffi_to":null,"language":"Crystal",) +
       %("concurrency":"async","threading":"single","io":"epoll"})
  exit 0
end

ROLE     = arg("role").not_nil!
KIND     = arg("kind", "throughput").not_nil!
ENDPOINT = arg("endpoint").not_nil!
PAYLOAD  = "x" * (arg("payload-bytes", "0").not_nil!.to_i)
MESSAGES = arg("messages", "0").not_nil!.to_i64
WARMUP   = arg("warmup", "0").not_nil!.to_i64
DURATION = arg("duration-secs", "0").not_nil!.to_f
BIND     = flag?("bind")

STDERR.puts "omq-cr-target: role=#{ROLE} kind=#{KIND} ep=#{ENDPOINT} " \
            "payload=#{PAYLOAD.bytesize}B msgs=#{MESSAGES} variant=#{VARIANT}"

# PUB drops what it publishes before a subscriber has finished subscribing, so
# the publisher waits before the loop starts. Same knob and default as the Ruby
# sibling, for the same reason: the settle has to cover the arrival of every
# subscriber, not just the first.
SETTLE = (ENV["ARENA_PUB_SETTLE"]? || "2.0").to_f

def percentiles(rtts : Array(Int64)) : String
  if rtts.empty?
    return "LATENCY 0 0 0 0 0 0 0"
  end
  rtts.sort!
  q = ->(p : Float64) { rtts[Math.min((rtts.size * p).to_i, rtts.size - 1)] }
  "LATENCY #{rtts.size} #{rtts.first} #{q.call(0.50)} #{q.call(0.90)} " \
  "#{q.call(0.99)} #{q.call(0.999)} #{rtts.last}"
end

# Wait a bounded time for the first message, matching the compiled targets: they
# give up after ten seconds and report a zero window rather than blocking. A
# starved cell must be a zero here and a zero everywhere else, not a missing
# point in one place and a number in another.
def first_or_bail(sock, seconds = 10) : Bool
  done = Channel(Bool).new
  spawn do
    begin
      sock.receive
      done.send(true)
    rescue
      done.send(false)
    end
  end
  select
  when ok = done.receive
    return true if ok
  when timeout(seconds.seconds)
  end
  puts "THROUGHPUT 0 0.000001"
  false
end

def timed_drain(sock, seconds : Float64)
  return unless first_or_bail(sock)
  count = 1_i64
  t0 = Time.instant
  deadline = t0 + seconds.seconds
  while Time.instant < deadline
    sock.receive
    count += 1
  end
  secs = (Time.instant - t0).total_seconds
  puts "THROUGHPUT #{count} #{"%.6f" % Math.max(secs, 1e-9)}"
end

case KIND
when "throughput"
  if ROLE == "sub"
    pull = OMQ::PULL.bind(ENDPOINT, recv_hwm: recv_hwm)
    WARMUP.times { pull.receive } # drained before the clock starts
    t0 = Time.instant
    MESSAGES.times { pull.receive }
    secs = (Time.instant - t0).total_seconds
    puts "THROUGHPUT #{MESSAGES} #{"%.6f" % Math.max(secs, 1e-9)}"
  else
    # linger: nil means close waits for queued frames to go out. omq.cr defaults
    # linger to zero, which skips the drain entirely -- with that default the
    # sender reports having sent every message while the receiver blocks forever
    # waiting for the tail that was thrown away. Every other target in the arena
    # drains, so this one has to say so explicitly.
    push = OMQ::PUSH.connect(ENDPOINT, linger: nil, send_hwm: send_hwm)
    (MESSAGES + WARMUP).times { push.send(PAYLOAD) }
    push.close
  end
when "latency"
  if ROLE == "sub"
    rep = OMQ::REP.bind(ENDPOINT, recv_hwm: recv_hwm, send_hwm: send_hwm)
    loop do
      msg = rep.receive
      rep.send(msg.first)
    end
  else
    req = OMQ::REQ.connect(ENDPOINT, recv_hwm: recv_hwm, send_hwm: send_hwm)
    WARMUP.times { req.send(PAYLOAD); req.receive }
    rtts = [] of Int64
    MESSAGES.times do
      t = Time.instant
      req.send(PAYLOAD)
      req.receive
      rtts << (Time.instant - t).total_nanoseconds.to_i64
    end
    puts percentiles(rtts)
  end
when "pubsub"
  if ROLE == "pub"
    pub = OMQ::PUB.bind(ENDPOINT, send_hwm: send_hwm)
    sleep SETTLE.seconds
    # Hand the scheduler back regularly. Crystal fibers are cooperative, and a
    # publish loop that never blocks never yields, so the socket's own IO fibers
    # never run and nothing reaches the wire -- the subscriber then measures a
    # silent window. Same failure the Ruby sibling has for the same reason.
    n = 0_u64
    loop do
      pub.send(PAYLOAD)
      n += 1
      Fiber.yield if (n & 0xFF) == 0
    end
  else
    sub = OMQ::SUB.connect(ENDPOINT, subscribe: "", recv_hwm: recv_hwm)
    timed_drain(sub, DURATION)
  end
when "fanout", "fanin"
  if ROLE == "pub"
    push = BIND ? OMQ::PUSH.bind(ENDPOINT, linger: nil, send_hwm: send_hwm) : OMQ::PUSH.connect(ENDPOINT, linger: nil, send_hwm: send_hwm)
    n = 0_u64
    loop do
      push.send(PAYLOAD)
      n += 1
      Fiber.yield if (n & 0xFF) == 0
    end
  else
    pull = BIND ? OMQ::PULL.bind(ENDPOINT, recv_hwm: recv_hwm) : OMQ::PULL.connect(ENDPOINT, recv_hwm: recv_hwm)
    timed_drain(pull, DURATION)
  end
else
  STDERR.puts "omq-cr-target: unsupported kind #{KIND}"
  exit 1
end
