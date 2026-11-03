#!/usr/bin/env bash
# The FixedLengthStream abort path against an R2 binding, in local workerd
# through miniflare (in-memory bucket, `cf: false`: no network, no deploy).
# Usage: stream-probe/run.sh <miniflare dist/src/index.js>
# Prints one FLS line per writer ending: full, close, abort, short.
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
exec node "$HERE/drive.mjs" "$1" "$HERE/worker.mjs"
