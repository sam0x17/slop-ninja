#!/usr/bin/env bash
# Builds the drill modules (default allocator, arena, and arena with the
# model's fixed linear memory) for wasm32 into
# <out-dir>. Run from anywhere; honours CARGO_TARGET_DIR.
# Usage: drill/build.sh <out-dir>
# DRILL_EXTRA_FEATURES (optional) adds cargo features to both builds, e.g.
# `slop-ninja-arena/unmeasured-model` for a measurement build.
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
OUT=$1
mkdir -p "$OUT"
TARGET=${CARGO_TARGET_DIR:-$HERE/../target}
cd "$HERE"
args=(build --offline -j "${CARGO_BUILD_JOBS:-4}" --release -p slop-ninja-arena-drill --lib --target wasm32-unknown-unknown)
extra=${DRILL_EXTRA_FEATURES:-}
cargo "${args[@]}" ${extra:+--features "$extra"}
cp "$TARGET/wasm32-unknown-unknown/release/slop_ninja_arena_drill.wasm" "$OUT/drill-default.wasm"
cargo "${args[@]}" --features "arena${extra:+,$extra}"
cp "$TARGET/wasm32-unknown-unknown/release/slop_ninja_arena_drill.wasm" "$OUT/drill-arena.wasm"
cargo "${args[@]}" --features "fixed-memory${extra:+,$extra}"
cp "$TARGET/wasm32-unknown-unknown/release/slop_ninja_arena_drill.wasm" "$OUT/drill-arena-fixed.wasm"
ls -l "$OUT"/drill-*.wasm
