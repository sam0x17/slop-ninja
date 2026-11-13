#!/usr/bin/env bash
# SlopNinja local dev session (dev only; nothing here deploys or logs in).
#
#   dev-session.sh cert       make the local TLS certificate (once)
#   dev-session.sh build      build the three stub Workers (worker-build --release)
#   dev-session.sh migrate    apply both D1 stores' migrations (stores.toml) to the site session's local state
#   dev-session.sh stores     list the tables of each local store
#   dev-session.sh stubs      run the stubs session      (https://auth.slop.localhost:8789)
#   dev-session.sh site       run the site session       (https://slop.localhost:8787)
#   dev-session.sh secret     run the secret-host session (https://s.slop.localhost:8788)
#   dev-session.sh smoke      check a running stubs session (node smoke.mjs)
#
# Each session command runs one `wrangler dev --local` in the foreground; run
# each in its own terminal (or unit). The sessions find each other through
# one dev registry under $DEV_STATE, so service bindings cross them.
#
# Environment:
#   DEV_STATE   state directory (default: subnet/devtools/.dev, ignored by Git)
#   WEB_REPO    checkout of the website repository (for site and secret)
#   SITE_PORT, SECRET_PORT, STUBS_PORT   (default 8787, 8788, 8789)
set -euo pipefail

HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
DEVTOOLS=$(cd "$HERE/.." && pwd)
SUBNET=$(cd "$DEVTOOLS/.." && pwd)
STATE=${DEV_STATE:-$DEVTOOLS/.dev}
SITE_PORT=${SITE_PORT:-8787}
SECRET_PORT=${SECRET_PORT:-8788}
STUBS_PORT=${STUBS_PORT:-8789}

# No telemetry, no update check, no Request.cf download, no log files: the
# session needs no network and no Cloudflare login.
export WRANGLER_SEND_METRICS=false WRANGLER_SEND_ERROR_REPORTS=false WRANGLER_WRITE_LOGS=false
export WRANGLER_HIDE_BANNER=true CLOUDFLARE_CF_FETCH_ENABLED=false
export WRANGLER_REGISTRY_PATH=$STATE/registry

STUBS=(auth-stub stub-scorer fixture-site)

die() { echo "dev-session: $*" >&2; exit 2; }

cert() {
  mkdir -p "$STATE"
  if [[ -s $STATE/cert.pem && -s $STATE/key.pem ]]; then return; fi
  openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -days 365 \
    -subj "/CN=slop.localhost" \
    -addext "subjectAltName=DNS:slop.localhost,DNS:s.slop.localhost,DNS:*.slop.localhost,DNS:localhost,IP:127.0.0.1" \
    -addext "basicConstraints=critical,CA:TRUE" \
    -keyout "$STATE/key.pem" -out "$STATE/cert.pem" 2>/dev/null
  chmod 600 "$STATE/key.pem"
  echo "dev-session: certificate $STATE/cert.pem (trust it in the test browser, or NODE_EXTRA_CA_CERTS for node)"
}

build() {
  for s in "${STUBS[@]}"; do
    (cd "$DEVTOOLS/$s" && "${WORKER_BUILD:-worker-build}" --release)
  done
}

# The site session's configs: the website first (the session's primary),
# then each subnet Worker whose config defines [env.dev] (README); one
# without it has not joined the dev session yet and is skipped.
site_configs() {
  [[ -n ${WEB_REPO:-} ]] || die "set WEB_REPO to the website repository checkout"
  local primary=$WEB_REPO/web/wrangler.jsonc
  [[ -f $primary ]] || die "no $primary"
  local out=("-c" "$primary")
  for w in detect-worker account-worker detect-og detect-fetch; do
    local cfg=$SUBNET/$w/wrangler.toml
    if [[ -f $cfg ]] && grep -Eq '^\[{1,2}env\.dev[].]' "$cfg"; then out+=("-c" "$cfg"); else echo "dev-session: $w has no [env.dev] yet, skipped" >&2; fi
  done
  printf '%s\n' "${out[@]}"
}

serve() { # <port> <persist dir> <wrangler args...>
  local port=${1} persist=${2}; shift 2
  cert
  exec wrangler dev "$@" --local --ip 127.0.0.1 --port "$port" --inspector-port $((port + 1000)) \
    --local-protocol https --https-key-path "$STATE/key.pem" --https-cert-path "$STATE/cert.pem" \
    --infer-origin-from-routes=false --persist-to "$persist" --show-interactive-dev-session=false
}

case ${1:-} in
  cert) cert ;;
  build) build ;;
  migrate|stores)
    # The site session's D1 stores (stores.toml): migrate applies each
    # Worker's own migrations/ to the session's local state; stores lists
    # each store's tables.
    for db in $(sed -n 's/^binding *= *"\([^"]*\)".*/\1/p' "$HERE/stores.toml"); do
      if [[ ${1:-} == migrate ]]; then
        wrangler d1 migrations apply "$db" --local -c "$HERE/stores.toml" --persist-to "$STATE/site"
      else
        wrangler d1 execute "$db" --local -c "$HERE/stores.toml" --persist-to "$STATE/site" \
          --command "SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name"
      fi
    done ;;
  stubs)
    args=()
    for s in "${STUBS[@]}"; do
      [[ -f $DEVTOOLS/$s/build/index.js ]] || die "$s is not built: run dev-session.sh build"
      args+=(-c "$DEVTOOLS/$s/wrangler.toml")
    done
    serve "$STUBS_PORT" "$STATE/stubs" "${args[@]}" ;;
  site)
    mapfile -t args < <(site_configs)
    serve "$SITE_PORT" "$STATE/site" "${args[@]}" --env dev ;;
  secret)
    [[ -n ${WEB_REPO:-} ]] || die "set WEB_REPO to the website repository checkout"
    serve "$SECRET_PORT" "$STATE/secret" -c "$WEB_REPO/web-secret/wrangler.jsonc" --env dev ;;
  smoke)
    cert
    NODE_EXTRA_CA_CERTS=$STATE/cert.pem exec node "$HERE/smoke.mjs" "https://auth.slop.localhost:$STUBS_PORT" ;;
  *) sed -n '2,21p' "${BASH_SOURCE[0]}"; exit 2 ;;
esac
