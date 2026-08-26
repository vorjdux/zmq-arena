#!/usr/bin/env ruby
# frozen_string_literal: true
#
# zmq-arena target wrapper for omq.rb, the pure-Ruby ZMTP implementation.
#
# Not a binding: omq.rb speaks the wire protocol itself, with no libzmq, no FFI
# and no C extension. It is measured as an implementation rather than as a
# language access path, which is why `impl` is native below.
#
# The wrapper times its own steady-state window and prints one result line. A
# Ruby process spends a hundred-odd milliseconds booting and requiring before it
# reaches the benchmark, and letting the harness time the process would charge
# that to the measurement.

require 'json'
require 'async'

VARIANT = (i = ARGV.index('--variant')) ? ARGV[i + 1] : 'default'

if ARGV[0] == 'describe'
  require 'omq'
  # The engine is omq.rb itself: it implements ZMTP in Ruby rather than binding
  # anything. Grouping it with the omq family is the dashboard's job, through
  # the registry's `family` field, and does not belong in a target's report of
  # what it actually is.
  puts JSON.generate(
    engine: 'omq.rb',
    lib_version: OMQ::VERSION,
    binding_version: nil,
    lib_language: 'Ruby',
    impl: 'native',
    ffi_to: nil,
    language: 'Ruby',
    concurrency: 'async',
    threading: 'single',
    io: 'epoll'
  )
  exit 0
end

require 'omq'

def arg(name, default = nil)
  i = ARGV.index("--#{name}")
  i ? ARGV[i + 1] : default
end

def flag?(name) = ARGV.include?("--#{name}")

def knob(key)
  ARGV.each_with_index do |a, i|
    next unless a == '--knob'
    k, _, v = ARGV[i + 1].to_s.partition('=')
    return v if k == key
  end
  ENV["ARENA_KNOB_#{key.upcase}"]
end

ROLE     = arg('role')
KIND     = arg('kind', 'throughput')
ENDPOINT = arg('endpoint')
PAYLOAD  = 'x' * arg('payload-bytes').to_i
MESSAGES = arg('messages').to_i
WARMUP   = arg('warmup', '0').to_i
DURATION = arg('duration-secs', '0').to_f
BIND     = flag?('bind')

warn "omq-rb-target: role=#{ROLE} kind=#{KIND} ep=#{ENDPOINT} " \
     "payload=#{PAYLOAD.bytesize}B msgs=#{MESSAGES} variant=#{VARIANT}"

# The arena forwards one knob superset to every target; apply what this engine
# understands and ignore the rest, per the knob convention.
def tune(sock)
  if (v = knob('sndhwm')) && sock.respond_to?(:send_hwm=)
    sock.send_hwm = v.to_i
  end
  if (v = knob('rcvhwm')) && sock.respond_to?(:recv_hwm=)
    sock.recv_hwm = v.to_i
  end
  sock
end

def open_socket(type, endpoint, bind)
  sock = bind ? type.bind(endpoint) : type.connect(endpoint)
  tune(sock)
end

def print_latency(rtts)
  if rtts.empty?
    puts 'LATENCY 0 0 0 0 0 0 0'
    return
  end
  rtts.sort!
  q = ->(p) { rtts[[(rtts.size * p).to_i, rtts.size - 1].min] }
  puts format('LATENCY %d %d %d %d %d %d %d',
              rtts.size, rtts.first, q[0.50], q[0.90], q[0.99], q[0.999], rtts.last)
end

# Wait a bounded time for the first message, matching the compiled targets:
# they give up after ten seconds and report a zero window rather than blocking.
# Without this a starved cell would be a missing data point here and a zero
# everywhere else, which is not a comparison.
def first_or_bail(sock, seconds = 10)
  sock.read_timeout = seconds if sock.respond_to?(:read_timeout=)
  sock.receive
  sock.read_timeout = nil if sock.respond_to?(:read_timeout=)
  true
rescue StandardError
  puts 'THROUGHPUT 0 0.000001'
  false
end

def timed_drain(sock, seconds)
  return unless first_or_bail(sock)
  count = 1
  t0 = Process.clock_gettime(Process::CLOCK_MONOTONIC)
  deadline = t0 + seconds
  while Process.clock_gettime(Process::CLOCK_MONOTONIC) < deadline
    sock.receive
    count += 1
  end
  secs = Process.clock_gettime(Process::CLOCK_MONOTONIC) - t0
  puts format('THROUGHPUT %d %.6f', count, [secs, 1e-9].max)
end

# Everything runs inside an Async reactor.
#
# omq.rb is fibre-based and async-native. Outside a reactor it still works, on a
# shared IO thread, and its README offers that so simple scripts do not need
# boilerplate -- but it is not the path the library is built for. Measured on
# this wrapper, one process, 64B over TCP: 25k msgs/s on the shared IO thread
# against 628k inside a reactor, a factor of twenty-five. Benchmarking the
# convenience path would have published omq.rb as forty times slower than a
# Python binding, which says nothing about the library and everything about how
# the harness drove it.
Async do |task|
case KIND
when 'throughput'
  if ROLE == 'sub'
    pull = open_socket(OMQ::PULL, ENDPOINT, true)
    WARMUP.times { pull.receive }   # drained before the clock starts
    t0 = Process.clock_gettime(Process::CLOCK_MONOTONIC)
    MESSAGES.times { pull.receive }
    secs = Process.clock_gettime(Process::CLOCK_MONOTONIC) - t0
    puts format('THROUGHPUT %d %.6f', MESSAGES, [secs, 1e-9].max)
  else
    push = open_socket(OMQ::PUSH, ENDPOINT, false)
    (MESSAGES + WARMUP).times { push << PAYLOAD }
    push.close # drains queued frames rather than dropping them
  end

when 'latency'
  if ROLE == 'sub'
    rep = open_socket(OMQ::REP, ENDPOINT, true)
    loop do
      msg = rep.receive
      rep << msg.first
    end
  else
    req = open_socket(OMQ::REQ, ENDPOINT, false)
    WARMUP.times { req << PAYLOAD; req.receive }
    rtts = []
    MESSAGES.times do
      t = Process.clock_gettime(Process::CLOCK_MONOTONIC, :nanosecond)
      req << PAYLOAD
      req.receive
      rtts << (Process.clock_gettime(Process::CLOCK_MONOTONIC, :nanosecond) - t)
    end
    print_latency(rtts)
  end

when 'pubsub'
  if ROLE == 'pub'
    pub = open_socket(OMQ::PUB, ENDPOINT, true)
    # PUB drops what it publishes before a subscriber has finished subscribing.
    # Worse here than elsewhere: omq.rb is fibre-based, and a tight publish loop
    # never reaches an await point, so a subscriber arriving after the loop
    # starts can never complete its handshake and receives nothing at all. That
    # is why this cell reported zero before. The settle has to cover the arrival
    # of every subscriber, not just the first.
    sleep(ENV.fetch('ARENA_PUB_SETTLE', '2.0').to_f)
    n = 0
    loop do
      pub << PAYLOAD
      n += 1
      # Hand the reactor back regularly so it can service subscribers and
      # actually write the socket, rather than spinning in this fibre.
      task.yield if (n % 256).zero?
    end
  else
    sub = open_socket(OMQ::SUB, ENDPOINT, false)
    sub.subscribe('')
    timed_drain(sub, DURATION)
  end

when 'fanout', 'fanin'
  if ROLE == 'pub'
    push = open_socket(OMQ::PUSH, ENDPOINT, BIND)
    loop { push << PAYLOAD }
  else
    pull = open_socket(OMQ::PULL, ENDPOINT, BIND)
    timed_drain(pull, DURATION)
  end

else
  warn "omq-rb-target: unsupported kind #{KIND}"
  exit 1
end
# `Async { }` reports a failed task to the logger and returns normally, so a
# publisher that dies mid-window still exits 0 and reads as a healthy feeder.
# For a pubsub or fanout cell that is the worst outcome available: the consumer
# on the other side keeps timing a stream nobody is feeding any more and reports
# real arithmetic over a starved window. `wait` re-raises, which makes the
# process exit non-zero and the cell fail instead of publishing the number.
end.wait
