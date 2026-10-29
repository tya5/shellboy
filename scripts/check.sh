#!/usr/bin/env sh
set -eu
cd "$(dirname "$0")/.."
python3 scripts/check-test-layout.py
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --all-targets
