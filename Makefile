# zmq-arena dev workflow.
#
# Common flow on a dev host:
#   make build      # control plane + every runnable variant (11 series)
#   make run        # run the matrix and render the archive into docs/
#   make            # build + run + render in one go
#
# The run targets (bench, run, run-root, dry) regenerate matrix.linode.json from
# scripts/gen_matrix.py first, so the sweep is never stale. Override MATRIX to run
# a different file, which is left untouched:
#   make run MATRIX=matrix.example.json RUN_ID=2026-06-29-a
# Tune the generated sweep by editing scripts/gen_matrix.py (sizes, counts).

MATRIX  ?= matrix.linode.json
RUN_ID  ?= $(shell date -u +%F)
SCRATCH ?= scratch/$(RUN_ID)
ORCH    ?= ./target/release/zmq-arena
# The file scripts/gen_matrix.py writes. The run targets regenerate it before a
# run, but only when MATRIX still points at it, so an overridden MATRIX is left
# untouched.
GEN_MATRIX ?= matrix.linode.json
regen = @if [ "$(MATRIX)" = "$(GEN_MATRIX)" ]; then python3 scripts/gen_matrix.py; fi

.PHONY: all build orchestrator images image \
        matrix bench render variants run run-root dry dashboard clean help

all: build run            ## build everything, then run + render

# An engine that picks its runtime at compile time needs one build per runtime,
# each into its own target dir, because every shipped runtime is its own measured
# variant. omq selects its third model (blocking) at run time, so one build
# covers all three of its variants.
build: orchestrator images  ## build the control plane and every target image

# Targets are built inside pinned images and run from the exported filesystem,
# so the bench host needs no compiler, no Rust toolchain and no libzmq headers.
# The image is a build artifact, not a runtime: nothing runs under a container
# at measurement time. See docs/CONTAINERS.md.
IMAGES = libzmq:libzmq_cpp_target monocoque:monocoque_target \
         zeromq_rs:zeromq_rs_target omq_tokio:omq_tokio_target \
         rzmq:rzmq_target celerity:celerity_target \
         rust_zmq:rust_zmq_target tmq:tmq_target \
         pyzmq:pyzmq_target omq_rb:omq_rb_target netmq:netmq_target jeromq:jeromq_target

images:                   ## build and export every target image
	@for pair in $(IMAGES); do \
	  name=$${pair%%:*}; dir=$${pair##*:}; \
	  bash scripts/build-image.sh targets/$$dir $$name || exit 1; \
	done

image:                    ## build one target image, e.g. make image T=celerity_target N=celerity
	bash scripts/build-image.sh targets/$(T) $(N)

matrix:                   ## regenerate matrix.linode.json (payload sweep, all kinds)
	python3 scripts/gen_matrix.py

orchestrator:             ## build the Rust control plane
	cargo build --release -p zmq-arena-orchestrator

bench:                    ## regenerate the matrix (if default) and run it into scratch/<run-id>
	$(regen)
	$(ORCH) run --matrix $(MATRIX) --run-id $(RUN_ID) --out $(SCRATCH)

render: bench             ## run, then render the result archive into docs/
	python3 scripts/render_results.py --scratch $(SCRATCH) --run-id $(RUN_ID)

variants:                 ## publish docs/variants.json and check it covers the matrix
	python3 scripts/render_variants.py

run: render               ## alias: run the matrix and render (assumes built)

run-root:                 ## regenerate the matrix (if default), run under sudo, then render
	$(regen)
	sudo $(ORCH) run --matrix $(MATRIX) --run-id $(RUN_ID) --out $(SCRATCH)
	python3 scripts/render_results.py --scratch $(SCRATCH) --run-id $(RUN_ID)

dry:                      ## regenerate the matrix (if default) and print the expanded plan
	$(regen)
	$(ORCH) run --matrix $(MATRIX) --dry-run

dashboard:                ## serve docs/ over HTTP (Ctrl-C to stop)
	cd docs && python3 -m http.server

clean:                    ## remove scratch and all build artifacts
	rm -rf scratch
	cargo clean
	rm -rf targets/libzmq_cpp_target/build \
		targets/monocoque_target/target targets/monocoque_target/target-tokio targets/monocoque_target/target-smol \
		targets/zeromq_rs_target/target targets/zeromq_rs_target/target-async-std \
		targets/zeromq_rs_target/target-async-dispatcher \
		targets/rust_zmq_target/target targets/tmq_target/target \
		targets/rzmq_target/target targets/celerity_target/target \
		targets/omq_tokio_target/target

help:                     ## list these targets
	@grep -E '^[a-zA-Z_-]+:.*?##' $(MAKEFILE_LIST) \
		| awk 'BEGIN{FS=":.*?## "}{printf "  \033[36m%-12s\033[0m %s\n", $$1, $$2}'
