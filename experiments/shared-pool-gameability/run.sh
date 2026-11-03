#!/bin/sh
set -eu
experiment=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
bash "$experiment/upstream/verify.sh"
cargo fmt --manifest-path "$experiment/Cargo.toml" --check
cargo test --offline --locked --manifest-path "$experiment/Cargo.toml"
cargo clippy --offline --locked --manifest-path "$experiment/Cargo.toml" --all-targets -- -D warnings
cargo run --release --offline --locked --manifest-path "$experiment/Cargo.toml" -- "$experiment/results.json"
cd "$experiment"
rustc --version > toolchain.txt
cargo --version >> toolchain.txt
rg --files --hidden src upstream | LC_ALL=C sort > source-files.txt
shasum -a 256 Cargo.toml Cargo.lock .gitignore run.sh toolchain.txt source-files.txt results.json > artifacts.sha256
while IFS= read -r shared_pool_file; do
    shasum -a 256 "$shared_pool_file" >> artifacts.sha256
done < source-files.txt
