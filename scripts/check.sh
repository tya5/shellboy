#!/usr/bin/env sh
set -eu
cd "$(dirname "$0")/.."
python3 scripts/check-test-layout.py
python3 -m unittest discover -s scripts/tests
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --all-targets
cargo test --locked --features test-db-observation --test integration db_observation_feature::integration_worker_samples_fast_statements -- --exact
