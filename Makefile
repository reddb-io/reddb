.PHONY: help build release test test-fast test-persistent clean run run-grpc install fmt lint check link unlink dev which patch minor major docs publish publish-dry-run env-up env-down env-logs test-env test-env-shell test-env-rust

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
	@echo "  make release       - Build optimized release version"
	@echo "  make test          - Run the default local test layer"
	@echo "  make test-fast     - Run the default local test layer"
	@echo "  make test-persistent - Run the persistent multimodel integration layer"
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
	cargo build

# Build release version (optimized)
release:
	cargo build --release

# Run tests
test:
	$(MAKE) test-fast

test-fast:
	cargo test --locked

test-persistent:
	CARGO_TARGET_DIR=$${CARGO_TARGET_DIR:-target/persistent-tests} cargo test --locked --test integration_persistent_multimodel -- --ignored

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
	cargo check --locked

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

# Release bump helpers
patch:
	@./scripts/release.sh patch

minor:
	@./scripts/release.sh minor

major:
	@./scripts/release.sh major

publish:
	@./scripts/publish.sh

publish-dry-run:
	@./scripts/publish.sh --dry-run
