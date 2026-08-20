# Ranking

From the run on 2026-07-03. This file is rewritten on every run; for the full history and interactive charts, open the dashboard under `docs/`.

Host: `13th Gen Intel(R) Core(TM) i7-1355U` - dev host; functional test, not admissible tail data

> **These numbers are not a verdict.** This run self-identifies as a dev-host functional test, which means it validates that the harness and the targets work, not how the libraries compare. A ranking is only meaningful from a dedicated, pinned bench host.

> **Read this first.** These boards are only as good as the host they ran on. Each cell runs in a 4-core cpuset so the producer and consumer do not time-share one core; the 32-subscriber pub/sub cell necessarily oversubscribes it, which is inherent to the workload and applies equally to every library. This is still a shared host, not a dedicated bench. Read the numbers as the payload trend and the relative shape between libraries, not a final absolute verdict.

Each board is the **geometric mean of every variant's ratio to the `libzmq` baseline**, over the cells they share. This is magnitude-aware (a 3x win counts as 3x, unlike averaging rank positions) and dimensionless (so payloads and transports combine cleanly). Cells flagged as inverted are dropped as known-wrong; a win smaller than the two cells' combined replicate spread is counted as a tie (the `ties` column), so noisy cells do not decide the order. Higher is better on every board. `cells` shows coverage against the baseline; a partial count means the variant did not run every benchmark and its score is not directly comparable to a full-coverage one.

Each message-rate workload (throughput, pub/sub, fan-out, fan-in) gets its own board, because their winners differ and blending them would hide that. Only latency, CPU efficiency and memory are summarised across workloads.

## Latency (p99, lower raw is better)

Reciprocal p99 latency, so higher on this board is faster.

| # | variant | vs libzmq | cells | ties |
|---|---------|-----------|-------|------|
| 1 | monocoque_tokio | 2.80x | 10/10 | 0 |
| 2 | monocoque | 2.04x | 10/10 | 0 |
| 3 | omq_compio | 1.76x | 10/10 | 0 |
| 4 | zeromq_rs | 1.74x | 10/10 | 0 |
| 5 | omq_tokio | 1.72x | 10/10 | 0 |
| 6 | libzmq | 1.00x | 10/10 | 10 |
| 7 | omq_tokio_mt | 0.92x | 10/10 | 0 |
| 8 | rust_zmq | 0.91x | 10/10 | 0 |

## Throughput (1-to-1 PUSH/PULL, msgs/s)

One producer to one consumer. This board matches the throughput detail tables below because it is the same workload.

| # | variant | vs libzmq | cells | ties |
|---|---------|-----------|-------|------|
| 1 | monocoque_tokio | 2.70x | 10/10 | 0 |
| 2 | monocoque | 2.42x | 10/10 | 0 |
| 3 | omq_tokio_mt | 1.73x | 9/10 (partial) | 1 |
| 4 | omq_tokio | 1.08x | 10/10 | 4 |
| 5 | rust_zmq | 1.06x | 10/10 | 8 |
| 6 | libzmq | 1.00x | 10/10 | 10 |
| 7 | omq_compio | 0.37x | 10/10 | 1 |
| 8 | zeromq_rs | 0.26x | 8/10 (partial) | 1 |

## Pub/Sub (msgs/s)

One publisher fanning the same stream to every subscriber.

| # | variant | vs libzmq | cells | ties |
|---|---------|-----------|-------|------|
| 1 | omq_compio | 3.42x | 5/5 | 0 |
| 2 | omq_tokio | 3.32x | 5/5 | 0 |
| 3 | omq_tokio_mt | 2.67x | 5/5 | 0 |
| 4 | monocoque | 1.91x | 5/5 | 0 |
| 5 | libzmq | 1.00x | 5/5 | 5 |
| 6 | rust_zmq | 0.98x | 5/5 | 4 |
| 7 | zeromq_rs | 0.16x | 4/5 (partial) | 0 |
| 8 | monocoque_tokio | 0.04x | 5/5 | 0 |

## Fan-out (msgs/s)

One producer sharing work across many consumers.

| # | variant | vs libzmq | cells | ties |
|---|---------|-----------|-------|------|
| 1 | monocoque_tokio | 2.53x | 5/5 | 0 |
| 2 | monocoque | 2.33x | 5/5 | 0 |
| 3 | omq_tokio_mt | 2.03x | 5/5 | 0 |
| 4 | omq_compio | 1.38x | 5/5 | 0 |
| 5 | libzmq | 1.00x | 5/5 | 5 |
| 6 | rust_zmq | 0.97x | 5/5 | 3 |
| 7 | omq_tokio | 0.55x | 5/5 | 2 |

## Fan-in (msgs/s)

Many producers converging on one consumer.

| # | variant | vs libzmq | cells | ties |
|---|---------|-----------|-------|------|
| 1 | monocoque_tokio | 1.59x | 5/5 | 0 |
| 2 | monocoque | 1.16x | 5/5 | 1 |
| 3 | omq_tokio_mt | 1.14x | 5/5 | 0 |
| 4 | omq_tokio | 1.01x | 5/5 | 1 |
| 5 | libzmq | 1.00x | 5/5 | 5 |
| 6 | rust_zmq | 0.96x | 5/5 | 1 |
| 7 | omq_compio | 0.36x | 5/5 | 1 |

## CPU efficiency (messages per CPU-second)

Work done per core-second across the whole cell (both processes), averaged over every workload. Rewards doing the same traffic for less CPU, which raw throughput hides.

| # | variant | vs libzmq | cells | ties |
|---|---------|-----------|-------|------|
| 1 | monocoque | 2.41x | 35/35 | 0 |
| 2 | monocoque_tokio | 1.81x | 35/35 | 0 |
| 3 | omq_tokio | 1.40x | 35/35 | 0 |
| 4 | omq_tokio_mt | 1.20x | 34/35 (partial) | 0 |
| 5 | omq_compio | 1.18x | 35/35 | 0 |
| 6 | libzmq | 1.00x | 35/35 | 0 |
| 7 | rust_zmq | 0.96x | 35/35 | 0 |
| 8 | zeromq_rs | 0.60x | 22/35 (partial) | 0 |

## Context-switch efficiency (messages per context switch)

Messages moved per context switch (voluntary plus involuntary), averaged over every workload. Higher means less scheduler churn per message.

| # | variant | vs libzmq | cells | ties |
|---|---------|-----------|-------|------|
| 1 | omq_tokio | 15.78x | 35/35 | 0 |
| 2 | monocoque | 12.39x | 35/35 | 0 |
| 3 | omq_compio | 10.89x | 35/35 | 0 |
| 4 | monocoque_tokio | 10.28x | 35/35 | 0 |
| 5 | omq_tokio_mt | 2.20x | 34/35 (partial) | 0 |
| 6 | libzmq | 1.00x | 35/35 | 0 |
| 7 | rust_zmq | 0.99x | 35/35 | 0 |
| 8 | zeromq_rs | 0.34x | 22/35 (partial) | 0 |

## Syscall efficiency (fewer syscalls per message)

Inverse of kernel crossings per message, averaged over every workload, so a batched io_uring engine ranks above a per-message epoll one. Only cells whose perf counters were captured count.

| # | variant | vs libzmq | cells | ties |
|---|---------|-----------|-------|------|
| 1 | omq_tokio_mt | 9.80x | 34/35 (partial) | 0 |
| 2 | monocoque_tokio | 9.42x | 35/35 | 0 |
| 3 | omq_tokio | 7.35x | 35/35 | 0 |
| 4 | monocoque | 4.58x | 35/35 | 0 |
| 5 | omq_compio | 3.08x | 35/35 | 0 |
| 6 | rust_zmq | 1.03x | 35/35 | 0 |
| 7 | libzmq | 1.00x | 35/35 | 0 |
| 8 | zeromq_rs | 0.70x | 22/35 (partial) | 0 |

## Memory (footprint)

Reciprocal peak RSS across the cell's processes, averaged over every workload, so higher is leaner.

| # | variant | vs libzmq | cells | ties |
|---|---------|-----------|-------|------|
| 1 | zeromq_rs | 1.60x | 22/35 (partial) | 0 |
| 2 | monocoque_tokio | 1.28x | 35/35 | 0 |
| 3 | monocoque | 1.16x | 35/35 | 0 |
| 4 | libzmq | 1.00x | 35/35 | 0 |
| 5 | rust_zmq | 0.98x | 35/35 | 0 |
| 6 | omq_tokio | 0.80x | 35/35 | 0 |
| 7 | omq_compio | 0.62x | 35/35 | 0 |
| 8 | omq_tokio_mt | 0.34x | 34/35 (partial) | 0 |

## Per-benchmark detail

### p99 latency: latency, ipc, 64 B (lower is better)

| # | variant | p99 latency (µs) | spread | n | conf |
|---|---------|------|--------|---|------|
| 1 | monocoque_tokio | 6.39 | 2.9% | 4 | ok |
| 2 | monocoque | 8.93 | 0.1% | 5 | ok |
| 3 | zeromq_rs | 10.24 | 4.2% | 5 | ok |
| 4 | omq_compio | 10.76 | 0.8% | 4 | ok |
| 5 | omq_tokio | 12.64 | 0.5% | 5 | ok |
| 6 | libzmq | 20.21 | 0.3% | 4 | ok |
| 7 | rust_zmq | 23.35 | 2.2% | 5 | ok |
| 8 | omq_tokio_mt | 23.99 | 0.9% | 5 | ok |

### p99 latency: latency, tcp_netns, 64 B (lower is better)

| # | variant | p99 latency (µs) | spread | n | conf |
|---|---------|------|--------|---|------|
| 1 | monocoque_tokio | 9.36 | 1.3% | 4 | ok |
| 2 | monocoque | 12.61 | 0.1% | 4 | ok |
| 3 | omq_compio | 13.82 | 0.5% | 4 | ok |
| 4 | omq_tokio | 13.84 | 0.7% | 4 | ok |
| 5 | zeromq_rs | 14.82 | 4.7% | 6 | ok |
| 6 | libzmq | 23.61 | 0.2% | 4 | ok |
| 7 | rust_zmq | 25.30 | 1.0% | 6 | ok |
| 8 | omq_tokio_mt | 25.68 | 0.7% | 5 | ok |

### throughput: throughput, ipc, 64 B (higher is better)

| # | variant | throughput (msg/s) | spread | n | conf |
|---|---------|------|--------|---|------|
| 1 | monocoque_tokio | 20346643.02 | 4.9% | 4 | ok |
| 2 | monocoque | 14963339.82 | 7.9% | 9 | low |
| 3 | rust_zmq | 11488344.72 | 10.3% | 8 | low |
| 4 | libzmq | 5761502.53 | 5.6% | 8 | low |
| 5 | omq_tokio_mt | 5420788.72 | 17.4% | 11 | low |
| 6 | omq_tokio | 3817498.42 | 1.3% | 4 | ok |
| 7 | omq_compio | 1114911.51 | 0.7% | 4 | ok |
| 8 | zeromq_rs | 949996.91 | 4.1% | 5 | ok |

> conf=low means the cell's replicates did not converge (relative IQR above target); treat its rank as indicative, not decisive.

### throughput: throughput, tcp_netns, 64 B (higher is better)

| # | variant | throughput (msg/s) | spread | n | conf |
|---|---------|------|--------|---|------|
| 1 | monocoque_tokio | 13136289.00 | 4.8% | 5 | ok |
| 2 | monocoque | 10269576.38 | 25.2% | 11 | low |
| 3 | libzmq | 6554383.38 | 0.8% | 4 | ok |
| 4 | rust_zmq | 5904089.24 | 3.4% | 4 | ok |
| 5 | omq_tokio_mt | 4880548.57 | 13.7% | 11 | INVERTED |
| 6 | omq_tokio | 4714711.53 | 1.3% | 4 | ok |
| 7 | omq_compio | 420009.69 | 0.6% | 4 | ok |
| 8 | zeromq_rs | 397653.05 | 4.4% | 5 | ok |

> conf=low means the cell's replicates did not converge (relative IQR above target); treat its rank as indicative, not decisive.

> conf=INVERTED means the msgs/s here is beaten by a larger payload in the same sweep, which is physically impossible on one path; the number is a measurement or socket-config artifact, not a real result. Reproducible does not mean correct.
