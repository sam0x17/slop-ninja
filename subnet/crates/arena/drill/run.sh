#!/usr/bin/env bash
# Local workerd drill: `workerd serve` on 127.0.0.1 only, no remote bindings,
# no deploy. Usage: drill/run.sh <out-dir> <plan.json> [workerd-binary] [--inspect]
# <out-dir> must hold drill-default.wasm and drill-arena.wasm (build.sh).
# Prints SELFTEST, MODEL, RESULT, SERIES, JSHEAP and CONSISTENCY lines; exits
# non-zero if the wasm32 self-tests fail, an expectation fails, arena memory
# passes its ceiling, or the two builds disagree on any output.
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
OUT=$1
PLAN=$2
WORKERD=${3:-workerd}
shift $(( $# >= 3 ? 3 : $# ))
exec node "$HERE/worker/drive.mjs" "$OUT" "$PLAN" "$WORKERD" "$@"
