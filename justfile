# MicroCommandControl (MCC) — developer tasks
# https://github.com/casey/just

set shell := ["bash", "-euo", "pipefail", "-c"]

default:
    @just --list

# Build the mcc binary (debug)
build:
    cargo build -p mcc

# Release build
release:
    cargo build -p mcc --release

# Full workspace tests (unit + integration)
test:
    cargo test --workspace

# Directed unit tests only (library/bin crates under crates/)
test-unit:
    cargo test --workspace --exclude mcc-tests

# Integration: harness package + CLI black-box tests
test-integration:
    cargo test -p mcc-tests
    cargo test -p mcc --test cli_smoke

# Format check
fmt:
    cargo fmt --all -- --check

# Format fix
fmt-fix:
    cargo fmt --all

# Clippy
lint:
    cargo clippy --workspace --all-targets -- -D warnings

# fmt + clippy + full test suite (regression gate)
check:
    just fmt
    just lint
    just test

# Run server (default: listen on 127.0.0.1:7443, data dir ~/.mcc)
run-server *args:
    cargo run -p mcc -- server {{args}}

# Init data dir + tokens only (no listen)
init-server *args:
    cargo run -p mcc -- server --init-only {{args}}

# Run agent stub (Phase 2: join/heartbeat)
run-agent *args:
    cargo run -p mcc -- agent {{args}}

# Show CLI help
help:
    cargo run -p mcc -- --help

# Clean target/
clean:
    cargo clean
