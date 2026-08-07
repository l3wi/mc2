# MicroCommandControl (MC2) — developer tasks
# https://github.com/casey/just

set shell := ["bash", "-euo", "pipefail", "-c"]

default:
    @just --list

# Build the mc2 binary (debug)
build:
    cargo build -p mc2

# Release build
release:
    cargo build -p mc2 --release

# Full workspace tests (unit + integration)
test:
    cargo test --workspace

# Directed unit tests only (library/bin crates under crates/)
test-unit:
    cargo test --workspace --exclude mc2-tests

# Integration: harness package + CLI black-box tests
test-integration:
    cargo test -p mc2-tests
    cargo test -p mc2 --test cli_smoke

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

# Run server (REST :7443, gRPC TLS :7444, data dir ~/.mc2)
run-server *args:
    cargo run -p mc2 -- server {{args}}

# Init data dir + tokens only (no listen)
init-server *args:
    cargo run -p mc2 -- server --init-only {{args}}

# Run agent (requires --server and --token; use --tls-ca for https)
run-agent *args:
    cargo run -p mc2 -- agent {{args}}

# Show CLI help
help:
    cargo run -p mc2 -- --help

# Clean target/
clean:
    cargo clean
