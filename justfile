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
    cargo clippy --workspace --all-targets --all-features -- -D warnings

# Docs (rustdoc warnings are errors)
doc:
    RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps

# Unused dependency check (requires cargo-machete: `cargo install cargo-machete --locked`)
machete:
    cargo machete

# fmt + clippy + tests + docs + machete (regression gate)
check:
    just fmt
    just lint
    just test
    just doc
    just machete

# Run server (REST :7443, data dir ~/.mc2; embeds the local node)
run-server *args:
    cargo run -p mc2 -- server {{args}}

# Init data dir + token only (no listen)
init-server *args:
    cargo run -p mc2 -- server --init-only {{args}}

# Show CLI help
help:
    cargo run -p mc2 -- --help

# Clean target/
clean:
    cargo clean
