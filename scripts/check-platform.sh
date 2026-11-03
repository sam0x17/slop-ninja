#!/usr/bin/env bash
# Compile every active Rust workspace and run small platform-relevant checks.
set -euo pipefail
platform_repo="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
platform_checks="${1:?Usage: bash scripts/check-platform.sh /absolute/new-output-directory}"
case "$platform_checks" in /*) ;; *) printf 'Output directory must be absolute\n' >&2; exit 2;; esac
mkdir "$platform_checks"
cd "$platform_repo"
{ uname -sm; rustc --version; cargo --version; } > "$platform_checks/host.txt"
for platform_project in root grammar detector transformer; do
  case "$platform_project" in
    root) platform_manifest=Cargo.toml;;
    grammar) platform_manifest=grammar/Cargo.toml;;
    detector) platform_manifest=baseline/detector/Cargo.toml;;
    transformer) platform_manifest=baseline/transformer/Cargo.toml;;
  esac
  cargo fmt --manifest-path "$platform_manifest" --all --check > "$platform_checks/$platform_project-fmt.log" 2>&1
  cargo check --locked --manifest-path "$platform_manifest" --all-targets > "$platform_checks/$platform_project-check.log" 2>&1
  printf '%s compiled\n' "$platform_project"
done
cargo test --locked --manifest-path Cargo.toml --lib receipt > "$platform_checks/receipts.log" 2>&1
cargo test --locked --manifest-path grammar/Cargo.toml -p grammar-core --lib > "$platform_checks/grammar-core.log" 2>&1
cargo test --locked --manifest-path baseline/transformer/Cargo.toml > "$platform_checks/transformer.log" 2>&1
printf 'Passed compilation and targeted checks; model/runtime replay is separate.\n' > "$platform_checks/completion.txt"
cat "$platform_checks/completion.txt"
