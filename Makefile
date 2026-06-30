# zmq-arena dev workflow.
#
# Common flow on a dev host:
#   make build      # orchestrator + the runnable targets (libzmq, monocoque, zmq.rs, rust-zmq)
#   make run        # run the matrix and render results into docs/
#   make            # build + run + render in one go
#
# Override any of these on the command line, e.g.
#   make run MATRIX=matrix.example.json RUN_ID=2026-06-29-a

MATRIX  ?= matrix.linode.json
RUN_ID  ?= $(shell date -u +%F)
SCRATCH ?= scratch/$(RUN_ID)
ORCH    ?= ./target/release/zmq-arena
CPU     := $(shell grep -m1 'model name' /proc/cpuinfo 2>/dev/null | cut -d: -f2 | sed 's/^ *//')
NOTE    ?= dev host; functional test, not admissible tail data

.PHONY: all build orchestrator libzmq monocoque zeromq-rs rust-zmq targets-all \
        matrix bench render run run-root dry dashboard clean help

all: build run            ## build everything, then run + render

build: orchestrator libzmq monocoque zeromq-rs rust-zmq  ## build the control plane and the runnable targets

matrix:                   ## regenerate matrix.linode.json (payload sweep, all kinds)
	python3 scripts/gen_matrix.py

orchestrator:             ## build the Rust control plane
	cargo build --release --manifest-path orchestrator/Cargo.toml

libzmq:                   ## configure (idempotent) and build the libzmq C++ target
	cmake -S targets/libzmq_cpp_target -B targets/libzmq_cpp_target/build -DCMAKE_BUILD_TYPE=Release
	cmake --build targets/libzmq_cpp_target/build -j

monocoque:                ## build the monocoque target
	cd targets/monocoque_target && cargo build --release

zeromq-rs:                ## build the zmq.rs target
	cd targets/zeromq_rs_target && cargo build --release

rust-zmq:                 ## build the rust-zmq target (links system libzmq)
	cd targets/rust_zmq_target && cargo build --release

targets-all:              ## build every target, including the stubbed engines
	./scripts/build-targets.sh

bench:                    ## run the matrix into scratch/<run-id> (no isolation)
	$(ORCH) run --matrix $(MATRIX) --run-id $(RUN_ID) --out $(SCRATCH)

render: bench             ## run, then render the result into docs/ + RANKING.md
	python3 scripts/render_results.py --scratch $(SCRATCH) --run-id $(RUN_ID) \
		--hardware-cpu "$(CPU)" --hardware-note "$(NOTE)"

run: render               ## alias: run the matrix and render (assumes built)

run-root:                 ## run with sudo so cgroup pinning applies, then render
	sudo $(ORCH) run --matrix $(MATRIX) --run-id $(RUN_ID) --out $(SCRATCH)
	python3 scripts/render_results.py --scratch $(SCRATCH) --run-id $(RUN_ID) \
		--hardware-cpu "$(CPU)" --hardware-note "$(NOTE)"

dry:                      ## print the expanded matrix plan without running
	$(ORCH) run --matrix $(MATRIX) --dry-run

dashboard:                ## serve docs/ over HTTP (Ctrl-C to stop)
	cd docs && python3 -m http.server

clean:                    ## remove scratch and all build artifacts
	rm -rf scratch
	cargo clean --manifest-path orchestrator/Cargo.toml
	rm -rf targets/libzmq_cpp_target/build targets/monocoque_target/target \
		targets/zeromq_rs_target/target targets/rust_zmq_target/target

help:                     ## list these targets
	@grep -E '^[a-zA-Z_-]+:.*?##' $(MAKEFILE_LIST) \
		| awk 'BEGIN{FS=":.*?## "}{printf "  \033[36m%-12s\033[0m %s\n", $$1, $$2}'
