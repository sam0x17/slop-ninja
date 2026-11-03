#!/bin/sh
set -eu
experiment=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
cargo fmt --manifest-path "$experiment/Cargo.toml" --check
cargo test --offline --locked --manifest-path "$experiment/Cargo.toml"
cargo clippy --offline --locked --manifest-path "$experiment/Cargo.toml" --all-targets -- -D warnings
cargo run --release --offline --locked --manifest-path "$experiment/Cargo.toml" -- "$experiment/results.json"
jq -f "$experiment/summary.jq" "$experiment/results.json" > "$experiment/summary.json"
cd "$experiment"
shasum -a 256 Cargo.toml Cargo.lock src/main.rs run.sh summary.jq results.json summary.json > artifacts.sha256
