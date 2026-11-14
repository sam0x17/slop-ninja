#!/usr/bin/env bash
# SlopNinja local dev session (dev only; nothing here deploys or logs in).
#
#   dev-session.sh cert       make the local TLS certificate (once)
#   dev-session.sh build      build every session Worker: the stubs, the joined subnet Workers (their dev
#                             feature) and, with WEB_REPO, the website and secret-host Workers
#   dev-session.sh secrets    write the local dev secrets and the session's operator key (.dev.vars.dev, untracked)
#   dev-session.sh migrate    apply both D1 stores' migrations (stores.toml) and put the dev package
#                             (dev-package/) into the local package bucket, in the site session's state
#   dev-session.sh stores     list the tables of each local store
#   dev-session.sh stubs      run the stubs session      (https://auth.slop.localhost:8789)
#   dev-session.sh site       run the site session       (https://slop.localhost:8787)
#   dev-session.sh secret     run the secret-host session (https://s.slop.localhost:8788)
#   dev-session.sh smoke      check a running stubs session (node smoke.mjs)
#   dev-session.sh run CMD... run all three sessions, wait until each answers, run CMD, stop them
#
# Each session command runs one `wrangler dev --local` in the foreground; run
# each in its own terminal (or unit), or all three under `run`. The sessions
# find each other through one dev registry under $DEV_STATE, so service
# bindings cross them.
#
# Environment:
#   DEV_STATE   state directory (default: subnet/devtools/.dev, ignored by Git)
#   WEB_REPO    checkout of the website repository (for site and secret)
#   SITE_PORT, SECRET_PORT, STUBS_PORT   (default 8787, 8788, 8789)
#   READY_S     how long `run` waits for the sessions to answer (default 900: the first start builds every Worker)
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

READY_S=${READY_S:-900}

STUBS=(auth-stub stub-scorer fixture-site)
# The subnet Workers that join the site session once they define [env.dev].
JOINED=(detect-worker account-worker detect-og detect-fetch)
# The detect Worker's dev package bucket (its [env.dev] DETECT_PACKAGES_TESTNET).
PACKAGES_BUCKET=dev-detect-packages

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

# Every Worker of the three sessions. `wrangler dev` would run each config's
# build command in its own working directory, which a multi-Worker session
# does not share, so the joined Workers' [env.dev] configs build nothing and
# this step builds each in its own directory: the stubs, the subnet Workers
# that joined (their dev feature where they have one), and with WEB_REPO the
# website and the secret-host Worker.
build() {
  local wb=${WORKER_BUILD:-worker-build} s w
  for s in "${STUBS[@]}"; do
    (cd "$DEVTOOLS/$s" && "$wb" --release)
  done
  for w in "${JOINED[@]}"; do
    local cfg=$SUBNET/$w/wrangler.toml
    [[ -f $cfg ]] && grep -Eq '^\[{1,2}env\.dev[].]' "$cfg" || continue
    if grep -Eq '^dev = ' "$SUBNET/$w/Cargo.toml"; then
      (cd "$SUBNET/$w" && "$wb" --release --features dev)
    else
      (cd "$SUBNET/$w" && "$wb" --release)
    fi
  done
  if [[ -n ${WEB_REPO:-} ]]; then
    (cd "$WEB_REPO" && npm run build && npm run build:secret)
  fi
}

# The detect Worker's store secret and slug pepper for the dev session:
# random once per state directory, kept there, and copied to the Worker's
# untracked .dev.vars.dev (its .gitignore), which `wrangler dev --env dev`
# reads. They never enter a tracked file.
#
# The session's operator key pair (Ed25519), also once per state directory:
# $STATE/dev_op_key holds the seed and dev_op_key.pub the public key, each as
# 64 lowercase hex digits (the operator CLI's key file format, mode 600). The
# public key is both Workers' DETECT_OP_KEY in the session (OP_ENV staging,
# as their [env.dev] vars say), so the dev op routes verify ops signed with
# the seed. No other environment trusts it: each holds an empty placeholder
# that only its own operator key replaces.
op_key() {
  local key=$STATE/dev_op_key
  if [[ -s $key && -s $key.pub ]]; then return; fi
  ( umask 077
    openssl genpkey -algorithm ed25519 -out "$key.pem" 2>/dev/null || die "openssl cannot make an Ed25519 key"
    openssl pkey -in "$key.pem" -outform DER 2>/dev/null | tail -c 32 | od -An -tx1 | tr -d ' \n' > "$key"
    openssl pkey -in "$key.pem" -pubout -outform DER 2>/dev/null | tail -c 32 | od -An -tx1 | tr -d ' \n' > "$key.pub"
    rm -f "$key.pem" )
  [[ $(wc -c < "$key") -eq 64 && $(wc -c < "$key.pub") -eq 64 ]] || die "the dev operator key is not 64 hex digits"
}

secrets() {
  mkdir -p "$STATE"
  local vars=$STATE/detect.dev.vars acct=$STATE/account.dev.vars pub
  if [[ ! -s $vars ]]; then
    ( umask 077
      printf 'DETECT_STORE_SECRET=%s\nDETECT_SLUG_PEPPER_V1=%s\n' "$(openssl rand -hex 32)" "$(openssl rand -hex 32)" > "$vars" )
  fi
  op_key
  pub=$(cat "$STATE/dev_op_key.pub")
  if ! grep -q '^DETECT_OP_KEY=' "$vars"; then printf 'DETECT_OP_KEY=%s\n' "$pub" >> "$vars"; fi
  ( umask 077; printf 'DETECT_OP_KEY=%s\n' "$pub" > "$acct" )
  install -m 600 "$vars" "$SUBNET/detect-worker/.dev.vars.dev"
  install -m 600 "$acct" "$SUBNET/account-worker/.dev.vars.dev"
}

# The site session's configs: the website's dev config first (the
# session's primary; the website is its repository's root package), then
# each subnet Worker whose config defines [env.dev] (README); one without it
# has not joined the dev session yet and is skipped.
site_configs() {
  [[ -n ${WEB_REPO:-} ]] || die "set WEB_REPO to the website repository checkout"
  local primary=$WEB_REPO/wrangler.dev.jsonc
  [[ -f $primary ]] || die "no $primary"
  local out=("-c" "$primary")
  for w in "${JOINED[@]}"; do
    local cfg=$SUBNET/$w/wrangler.toml
    if [[ -f $cfg ]] && grep -Eq '^\[{1,2}env\.dev[].]' "$cfg"; then out+=("-c" "$cfg"); else echo "dev-session: $w has no [env.dev] yet, skipped" >&2; fi
  done
  printf '%s\n' "${out[@]}"
}

# The Worker sees the session's own origin (`--local-upstream`, over HTTPS):
# Wrangler's local proxy rewrites URL-bearing request headers (Origin,
# Referer) from the origin the browser used to the upstream's, so the two
# must be the same or every Origin check would see another origin.
serve() { # <host> <port> <persist dir> <wrangler args...>
  local host=${1} port=${2}; shift 2
  local persist=${1}; shift
  cert
  exec wrangler dev "$@" --local --ip 127.0.0.1 --port "$port" --inspector-port $((port + 1000)) \
    --local-protocol https --https-key-path "$STATE/key.pem" --https-cert-path "$STATE/cert.pem" \
    --local-upstream "$host:$port" --upstream-protocol https \
    --infer-origin-from-routes=false --persist-to "$persist" --show-interactive-dev-session=false
}

# The dev package (dev-package/), into the site session's local package
# bucket under the keys its manifest names; local R2 tags each object with
# the MD5 of its bytes, which the manifest pins (a test checks both).
package() {
  local f
  for f in "$HERE"/dev-package/*; do
    wrangler r2 object put "$PACKAGES_BUCKET/dev-package/$(basename "$f")" --file "$f" --local \
      -c "$HERE/stores.toml" --persist-to "$STATE/site" >/dev/null
  done
  echo "dev-session: dev package in the local bucket $PACKAGES_BUCKET"
}

# Waits until an origin answers over the session's certificate, at most
# READY_S seconds (curl's own bounded retry).
ready() { # <origin> <path>
  local url=${1}${2}
  curl -s -o /dev/null --cacert "$STATE/cert.pem" --retry "$READY_S" --retry-delay 1 --retry-all-errors \
    --retry-max-time "$READY_S" --max-time "$READY_S" "$url"
}

# Stops the sessions `run` started: an interrupt (Wrangler's own clean
# stop, as Ctrl-C), then a kill for any that has not stopped within 20 s.
stop_all() {
  local p
  for p in "${pids[@]}"; do kill -INT "$p" 2>/dev/null || true; done
  for p in "${pids[@]}"; do
    timeout 20 tail --pid="$p" -f /dev/null 2>/dev/null || kill -KILL "$p" 2>/dev/null || true
  done
}

# All three sessions as children of this script, each logging to
# $STATE/logs/<session>.log; CMD runs once every origin answers, and the
# sessions stop when it ends (or when this script is stopped).
run_all() {
  (( $# )) || die "run: name a command"
  [[ -n ${WEB_REPO:-} ]] || die "set WEB_REPO to the website repository checkout"
  cert
  mkdir -p "$STATE/logs"
  local s rc=0
  pids=()
  trap stop_all EXIT
  for s in stubs site secret; do
    "$HERE/dev-session.sh" "$s" > "$STATE/logs/$s.log" 2>&1 &
    pids+=($!)
    # The registry must hold the stubs before the site session binds them.
    if [[ $s == stubs ]]; then ready "https://auth.slop.localhost:$STUBS_PORT" / || die "stubs session did not answer (logs/stubs.log)"; fi
    if [[ $s == site ]]; then ready "https://slop.localhost:$SITE_PORT" /health || die "site session did not answer (logs/site.log)"; fi
  done
  ready "https://s.slop.localhost:$SECRET_PORT" /robots.txt || die "secret session did not answer (logs/secret.log)"
  NODE_EXTRA_CA_CERTS=$STATE/cert.pem DEV_STATE=$STATE DEV_SUBNET=$SUBNET "$@" || rc=$?
  return "$rc"
}

case ${1:-} in
  cert) cert ;;
  build) build ;;
  secrets) secrets ;;
  run) shift; run_all "$@" ;;
  migrate|stores)
    # The site session's D1 stores (stores.toml): migrate applies each
    # Worker's own migrations/ to the session's local state and puts the
    # dev package into the local package bucket; stores lists each store's
    # tables.
    for db in $(sed -n '/^\[\[d1_databases\]\]/,/^$/s/^binding *= *"\([^"]*\)".*/\1/p' "$HERE/stores.toml"); do
      if [[ ${1:-} == migrate ]]; then
        wrangler d1 migrations apply "$db" --local -c "$HERE/stores.toml" --persist-to "$STATE/site"
      else
        wrangler d1 execute "$db" --local -c "$HERE/stores.toml" --persist-to "$STATE/site" \
          --command "SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name"
      fi
    done
    if [[ ${1:-} == migrate ]]; then package; fi ;;
  stubs)
    args=()
    for s in "${STUBS[@]}"; do
      [[ -f $DEVTOOLS/$s/build/index.js ]] || die "$s is not built: run dev-session.sh build"
      args+=(-c "$DEVTOOLS/$s/wrangler.toml")
    done
    serve auth.slop.localhost "$STUBS_PORT" "$STATE/stubs" "${args[@]}" ;;
  site)
    secrets
    mapfile -t args < <(site_configs)
    serve slop.localhost "$SITE_PORT" "$STATE/site" "${args[@]}" --env dev ;;
  secret)
    [[ -n ${WEB_REPO:-} ]] || die "set WEB_REPO to the website repository checkout"
    serve s.slop.localhost "$SECRET_PORT" "$STATE/secret" -c "$WEB_REPO/web-secret/wrangler.dev.jsonc" ;;
  smoke)
    cert
    NODE_EXTRA_CA_CERTS=$STATE/cert.pem exec node "$HERE/smoke.mjs" "https://auth.slop.localhost:$STUBS_PORT" ;;
  *) sed -n '2,26p' "${BASH_SOURCE[0]}"; exit 2 ;;
esac
