.PHONY: help build build-fast release warm test test-fast test-persistent drill-nightly clean run run-grpc install fmt lint check check-driver-rust check-driver-python timings cold-start-bench binary-size image-size artifact-size link unlink dev which patch minor major release-push package-check docs publish publish-dry-run env-up env-down env-logs test-env test-env-shell test-env-rust perf-bench

# Paths
LOCAL_BIN := $(HOME)/.local/bin
LOCAL_BINARY := $(PWD)/target/release/reddb
LOCAL_GRPC_BINARY := $(PWD)/target/release/reddb-grpc

# Default target
help:
	@echo "RedDB - Makefile"
	@echo ""
	@echo "Core:"
	@echo "  make build         - Build debug version"
	@echo "  make build-fast    - Build the `red` binary with the release-fast profile"
	@echo "  make warm          - Prebuild the common dev artifacts (build + tests/benches check)"
	@echo "  make release       - Build optimized release version"
	@echo "  make test          - Run the default local test layer"
	@echo "  make test-fast     - Run the default local test layer"
	@echo "  make test-persistent - Run the persistent multimodel integration layer"
	@echo "  make drill-nightly - Run backup/restore drill tests and append drill history"
	@echo "  make test-env PROFILE=replica - Bring up a dedicated test environment and run shell + Rust external-env tests"
	@echo "  make test-env-shell PROFILE=replica - Bring up a dedicated test environment and run shell checks only"
	@echo "  make test-env-rust PROFILE=replica - Run Rust external-env tests against an already running environment"
	@echo "  make clean         - Clean build artifacts"
	@echo "  make run           - Run HTTP server (ARGS='--path ... --bind ...')"
	@echo "  make run-grpc      - Run gRPC server (ARGS='--path ... --bind ...')"
	@echo ""
	@echo "Quality:"
	@echo "  make fmt           - Format code"
	@echo "  make lint          - Run clippy"
	@echo "  make check         - Quick compile check"
	@echo "  make check-driver-rust - Compile-check the Rust SDK with gRPC enabled"
	@echo "  make check-driver-python - Compile-check the Python SDK"
	@echo "  make timings       - Generate cargo build timings for the `red` binary"
	@echo "  make cold-start-bench - Measure cold-start P50/P95/P99 baselines"
	@echo "  make binary-size   - Measure release-static binary size"
	@echo "  make image-size    - Measure Docker image size"
	@echo "  make perf-bench    - Capture an insert_sequential flamegraph (requires relaxed kernel knobs; see docs/perf/perf-knobs.md)"
	@echo ""
	@echo "Release:"
	@echo "  make patch         - Release bump + commit/tag (patch)"
	@echo "  make minor         - Release bump + commit/tag (minor)"
	@echo "  make major         - Release bump + commit/tag (major)"
	@echo "  make install       - Install binaries from source with cargo"
	@echo "  make publish       - Publish crate to crates.io"
	@echo "  make publish-dry-run - Validate package for crates.io"

# Build debug version
build:
	./scripts/cargo-fast.sh build

build-fast:
	REDB_USE_SCCACHE=1 ./scripts/cargo-fast.sh build --profile release-fast --bin red

# Build release version (optimized)
release:
	REDB_USE_SCCACHE=1 ./scripts/cargo-fast.sh build --release

warm:
	./scripts/cargo-fast.sh build
	./scripts/cargo-fast.sh check --tests
	./scripts/cargo-fast.sh check --benches

# Run tests
test:
	$(MAKE) test-fast

test-fast:
	./scripts/cargo-fast.sh test --locked

test-persistent:
	CARGO_TARGET_DIR=$${CARGO_TARGET_DIR:-target/persistent-tests} cargo test --locked --test integration_persistent_multimodel -- --ignored

drill-nightly:
	@./scripts/drill-nightly.sh

# Clean artifacts
clean:
	cargo clean

# Run debug HTTP server
run:
	cargo run -- $(ARGS)

# Run debug gRPC server
run-grpc:
	cargo run --bin reddb-grpc -- $(ARGS)

# Format code
fmt:
	cargo fmt

# Clippy
lint:
	cargo clippy -- -D warnings

# Quick compile check
check:
	./scripts/cargo-fast.sh check --locked

check-driver-rust:
	./scripts/cargo-fast.sh check -p reddb-client --features grpc

check-driver-python:
	./scripts/cargo-fast.sh check --manifest-path drivers/python/Cargo.toml

timings:
	REDB_USE_SCCACHE=1 ./scripts/cargo-fast.sh build --profile release-fast --bin red --timings

cold-start-bench:
	@./scripts/cold-start-bench.sh

binary-size:
	@./scripts/artifact-size.sh binary

image-size:
	@./scripts/artifact-size.sh image

artifact-size:
	@./scripts/artifact-size.sh all

# Install from source
install:
	cargo install --path .

# Link local release binary
link:
	cargo build --release
	@mkdir -p $(LOCAL_BIN)
	@ln -sf "$(LOCAL_BINARY)" "$(LOCAL_BIN)/reddb"
	@ln -sf "$(LOCAL_GRPC_BINARY)" "$(LOCAL_BIN)/reddb-grpc"
	@echo "✓ Linked to $(LOCAL_BIN)/reddb and $(LOCAL_BIN)/reddb-grpc"

# Remove local symlink and use cargo-installed binary
unlink:
	@if [ -L "$(LOCAL_BIN)/reddb" ]; then rm -f "$(LOCAL_BIN)/reddb"; fi
	@if [ -L "$(LOCAL_BIN)/reddb-grpc" ]; then rm -f "$(LOCAL_BIN)/reddb-grpc"; fi
	@echo "✓ Removed local links"

# Show which binary is currently in use
which:
	@command -v reddb || true
	@command -v reddb-grpc || true

# Local development mode (build + link)
dev: link
	@echo "✓ RedDB local dev binaries available"

env-up:
	@docker compose -f testdata/compose/$${PROFILE:-replica}.yml up -d --build

env-down:
	@docker compose -f testdata/compose/$${PROFILE:-replica}.yml down -v

env-logs:
	@docker compose -f testdata/compose/$${PROFILE:-replica}.yml logs -f

test-env:
	@./scripts/test-environment.sh $${PROFILE:-replica} all

test-env-shell:
	@./scripts/test-environment.sh $${PROFILE:-replica} shell

test-env-rust:
	@./scripts/test-environment.sh $${PROFILE:-replica} rust

# Release bump helpers — bump version in Cargo.toml + Cargo.lock,
# commit, tag. Operator runs `make release-push` to fire the
# release.yml workflow (binary builds + GitHub release +
# cargo publish + npm publish).
patch:
	@./scripts/release.sh patch

minor:
	@./scripts/release.sh minor

major:
	@./scripts/release.sh major

# Push the latest tag + commit so the release workflow fires.
# Equivalent to `git push --follow-tags`. Run this immediately
# after `make patch|minor|major` once the working tree is clean.
release-push:
	@git push --follow-tags

# Manual cargo publish (engine only). Prefer the GitHub release
# workflow (triggered by tag push) for the canonical path. Use
# this only when the workflow is unavailable.
publish:
	@./scripts/publish.sh

publish-dry-run:
	@./scripts/publish.sh --dry-run

# Local cargo package dry-run for engine + rust client. Mirrors
# the publish-dry-run CI job — catches missing include paths,
# stale path-only deps, and license mismatches without hitting
# the registry.
package-check:
	@echo "==> cargo package (engine, with verify)"
	@cargo package --allow-dirty
	@echo "==> cargo package (rust client, no verify)"
	@cargo package -p reddb-client --allow-dirty --no-verify

# Capture a CPU flamegraph of `red` while the bench-runner drives
# `insert_sequential`. Implements P5 from
# docs/perf/insert_sequential-2026-05-05.md. See
# docs/perf/perf-knobs.md for the host requirements
# (kernel.perf_event_paranoid <= 1, kernel.yama.ptrace_scope = 0)
# and how to relax them.
#
# Bench load comes from the sibling repo at
# /home/cyber/Work/reddb.io/rdb-benchmark — assumed cloned there.
# If the bench finishes before the 30 s perf-record window closes,
# re-run the bench loop in another terminal until perf record exits.
#
# Output: target/perf/insert_sequential.svg (+ perf.data alongside).
PERF_OUT_DIR := target/perf
PERF_RED_BIN := target/release/red
PERF_DB_PATH := /tmp/reddb-perf-bench.rdb
PERF_BIND := 127.0.0.1:5050
PERF_DURATION := 30
RDB_BENCHMARK_DIR := /home/cyber/Work/reddb.io/rdb-benchmark

perf-bench:
	@set -e; \
	if ! command -v perf >/dev/null 2>&1; then \
		echo "ERROR: 'perf' is not installed."; \
		echo "  Install with: sudo apt install linux-tools-common linux-tools-$$(uname -r)"; \
		exit 1; \
	fi; \
	paranoid=$$(cat /proc/sys/kernel/perf_event_paranoid 2>/dev/null || echo 4); \
	if [ "$$paranoid" -gt 1 ]; then \
		echo "ERROR: kernel.perf_event_paranoid is $$paranoid — must be <= 1 for unprivileged perf record."; \
		echo "  Fix (until reboot): sudo sysctl kernel.perf_event_paranoid=1 kernel.yama.ptrace_scope=0"; \
		echo "  See docs/perf/perf-knobs.md for the security trade-off and a permanent fix."; \
		exit 1; \
	fi; \
	ptrace=$$(cat /proc/sys/kernel/yama/ptrace_scope 2>/dev/null || echo 1); \
	if [ "$$ptrace" -gt 0 ]; then \
		echo "ERROR: kernel.yama.ptrace_scope is $$ptrace — must be 0 to attach perf to a sibling-shell process."; \
		echo "  Fix (until reboot): sudo sysctl kernel.yama.ptrace_scope=0"; \
		echo "  See docs/perf/perf-knobs.md for the security trade-off."; \
		exit 1; \
	fi; \
	if ! command -v inferno-flamegraph >/dev/null 2>&1; then \
		echo "ERROR: 'inferno-flamegraph' is not installed."; \
		echo "  Install with: cargo install inferno"; \
		exit 1; \
	fi; \
	echo "==> building red with frame pointers"; \
	RUSTFLAGS="-Cforce-frame-pointers=yes" cargo build --release --bin red; \
	mkdir -p $(PERF_OUT_DIR); \
	rm -f $(PERF_DB_PATH); \
	echo "==> starting red on $(PERF_BIND) (db=$(PERF_DB_PATH))"; \
	$(PERF_RED_BIN) server --wire-bind $(PERF_BIND) --path $(PERF_DB_PATH) > $(PERF_OUT_DIR)/red.log 2>&1 & \
	RED_PID=$$!; \
	trap "kill $$RED_PID 2>/dev/null || true; wait $$RED_PID 2>/dev/null || true" EXIT INT TERM; \
	sleep 2; \
	if ! kill -0 $$RED_PID 2>/dev/null; then \
		echo "ERROR: red failed to start; see $(PERF_OUT_DIR)/red.log"; \
		exit 1; \
	fi; \
	echo "==> recording perf for $(PERF_DURATION) s against pid $$RED_PID"; \
	echo "    (drive load from $(RDB_BENCHMARK_DIR) — see comments in Makefile)"; \
	perf record -F 99 -g -p $$RED_PID -o $(PERF_OUT_DIR)/perf.data -- sleep $(PERF_DURATION); \
	echo "==> rendering flamegraph to $(PERF_OUT_DIR)/insert_sequential.svg"; \
	perf script -i $(PERF_OUT_DIR)/perf.data | inferno-flamegraph > $(PERF_OUT_DIR)/insert_sequential.svg; \
	echo "==> stopping red (pid $$RED_PID)"; \
	kill $$RED_PID 2>/dev/null || true; \
	wait $$RED_PID 2>/dev/null || true; \
	trap - EXIT INT TERM; \
	echo "OK: $(PERF_OUT_DIR)/insert_sequential.svg"
