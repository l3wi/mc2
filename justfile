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

# Unit tests (workspace)
test:
    cargo test --workspace

# Format check
fmt:
    cargo fmt --all -- --check

# Format fix
fmt-fix:
    cargo fmt --all

# Clippy
lint:
    cargo clippy --workspace --all-targets -- -D warnings

# fmt + clippy + test
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
