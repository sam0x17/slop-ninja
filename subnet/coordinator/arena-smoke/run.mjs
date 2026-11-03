// Local wasm32 smoke run of the coordinator build on the bounded arena.
//
// Loads worker-build's output (`build/`) into miniflare's bundled workerd on
// 127.0.0.1 with in-memory Durable Objects and R2, test-only bindings and an
// outbound service that answers every subrequest with 503 (no network, no
// Cloudflare account, nothing deployed). It drives the hand-written exports
// and checks, from the arena counters every `/status` reports:
//
// - linear memory stays at or below the model's ceiling (an observe build
//   links growable memory under it; an enforcing build links it fixed);
// - repeated requests leave no scope quarantined or abandoned and no pages
//   in use beyond the first request's (per-invocation reset);
// - the ChainJournal and EvalCohort classes construct and answer inside their
//   scopes; refusals and bounds answer their codes (401, 413, 404);
// - authenticated deliveries (signed with the native tests' public fixture
//   receiver) against a mock JSON-RPC chain on a loopback HTTP server, reached
//   through a JS relay Worker (the coordinator's outbound service) so replies
//   cross workerd's own network fetch: once plain, once gzip (workerd decodes
//   it and keeps the encoded Content-Length, as on Cloudflare). Both anchor
//   the watchdog with no RPC failure, and their waitUntil continuations leave
//   nothing in use, quarantined or abandoned;
// - the cron export (`scheduled`, miniflare's local trigger route) runs in
//   its own scope and completes.
//
// Usage: node run.mjs <miniflare dist/src/index.js> <build dir> <linear memory bytes> [requests] [static upper bytes]
// Exit 0 when every check passes.

import { resolve } from "node:path";
import { createHmac } from "node:crypto";
import { gzipSync } from "node:zlib";
import http from "node:http";

// The static upper bound is `arena_routes::STATIC_UPPER_BYTES` (printed by
// `examples/arena_wasm_check` as `static_upper_bytes`).
const [MF, BUILD, LINEAR, N = "200", STATIC_UPPER = "2097152"] = process.argv.slice(2);
const { Miniflare, convertV4MiniflareOptions } = await import(MF);
const linear = Number(LINEAR);
const rounds = Number(N);
// Test-only values: a status token of the minimum length and the public
// testnet chain identity (the build refuses any other genesis).
const TOKEN = "t".repeat(64);
const GENESIS = "0x8f9cf856bf558a14440e75569c9e58594757048d7b3a84b5d25f6bd978263105";

function options(role, outbound = () => new Response("offline", { status: 503 })) {
  return convertV4MiniflareOptions(config(role, outbound));
}

function config(role, outbound) {
  const bindings = {
    NETWORK: "testnet",
    CHAIN_GENESIS: GENESIS,
    SUBNET_NETUID: "601",
    RPC_URL: "https://127.0.0.1",
    COORDINATOR_STATUS_TOKEN: TOKEN,
    COORDINATOR_OPERATOR_TOKEN: TOKEN,
    // The native tests' public fixture receiver (src/testing.rs), never a
    // deployed secret.
    RTAO_WEBHOOK_KEY_ID: "pilot-v1",
    RTAO_WEBHOOK_SIGNING_SECRET: "42".repeat(32),
    RTAO_WEBHOOK_SUBSCRIPTION_ID: "0x" + "11".repeat(32),
  };
  if (role === "eval") bindings.EVAL_ROLE = "eval";
  return {
    cf: false,
    modulesRoot: resolve(BUILD),
    modules: [
      { type: "ESModule", path: resolve(BUILD, "index.js") },
      { type: "CompiledWasm", path: resolve(BUILD, "index_bg.wasm") },
    ],
    compatibilityDate: "2026-10-01",
    bindings,
    durableObjects: {
      CHAIN_JOURNAL_TESTNET: { className: "ChainJournal", useSQLite: true },
      EVAL_COHORT_TESTNET: { className: "EvalCohort", useSQLite: true },
    },
    r2Buckets: ["RECEIPTS_TESTNET", "EVAL_TESTNET"],
    outboundService: outbound,
  };
}

const failures = [];
function check(ok, what, detail) {
  console.log(JSON.stringify({ check: what, ok, ...(detail === undefined ? {} : { detail }) }));
  if (!ok) failures.push(what);
}

async function status(mf, path = "/status") {
  const r = await mf.dispatchFetch(`http://localhost${path}`, { headers: { authorization: `Bearer ${TOKEN}` } });
  const text = await r.text();
  let body = null;
  try {
    body = JSON.parse(text);
  } catch {}
  return { code: r.status, body, text };
}

function pick(a) {
  if (!a) return null;
  const k = ["memory_bytes", "linear_memory_model_bytes", "static_bytes", "limit_pages", "glue_budget_pages", "in_use_pages", "peak_in_use_pages", "open_scopes", "quarantined_scopes", "abandoned_scopes", "scopes_opened", "scopes_released", "overdrafts", "overdraft_pages", "refusals_retry", "refusals_capacity", "refusals_poisoned", "glue_spills", "glue_peak", "tickets_peak", "ticket_limit", "unscoped_allocs", "alloc_failures", "stack_high_water", "mode"];
  return Object.fromEntries(k.map((x) => [x, a[x]]));
}

// ------------------------------------------------------------ coordinator role
const mf = new Miniflare(options("coordinator"));
try {
  const unauth = await mf.dispatchFetch("http://localhost/status");
  check(unauth.status === 401, "status without a token is 401", unauth.status);

  const first = await status(mf);
  check(first.code === 200, "status with the token is 200", first.code);
  const a1 = first.body?.arena;
  const j1 = first.body?.chain_journal?.instance?.arena;
  check(a1?.memory_bytes > 0 && a1?.memory_bytes <= linear, "the Worker's linear memory is within the model's ceiling", pick(a1));
  check(j1?.memory_bytes > 0 && j1?.memory_bytes <= linear, "the ChainJournal reports a size within the ceiling from its scope", pick(j1));
  check(a1?.linear_memory_model_bytes === linear, "the arena's model is the coordinator's", a1?.linear_memory_model_bytes);
  check(a1?.static_bytes > 0 && a1.static_bytes <= Number(STATIC_UPPER), "static data and stack end within the route table's assumption", [a1?.static_bytes, Number(STATIC_UPPER)]);

  // A signature header of the right shape for the fixture receiver (key,
  // subscription, attempt, timestamp) with a MAC that cannot match: it
  // passes the header check, so the body bounds and deadline are reached.
  const shaped = () => JSON.stringify({
    version: 1, key_id: "pilot-v1", timestamp: String(Math.floor(Date.now() / 1000)),
    context: { subscription: "0x" + "11".repeat(32), event_id: "0x" + "22".repeat(32), delivery_id: "0x" + "33".repeat(32) },
    attempt: 1, mac: "0x" + "00".repeat(32),
  });
  const big = await mf.dispatchFetch("http://localhost/hook", { method: "POST", headers: { "content-length": String(256 * 1024 + 1), "X-Rtao-Signature": shaped() }, body: "x".repeat(256 * 1024 + 1) });
  check(big.status === 413, "a webhook body above its bound is refused (413)", big.status);
  // The same bound without a declared length: a streamed body past it.
  const chunk = new Uint8Array(64 * 1024).fill(120);
  let sent = 0;
  const streamed = new ReadableStream({
    pull(c) {
      if (sent >= 5) return c.close();
      sent++;
      c.enqueue(chunk);
    },
  });
  const overlong = await mf.dispatchFetch("http://localhost/hook", { method: "POST", body: streamed, duplex: "half", headers: { "X-Rtao-Signature": shaped() } });
  check(overlong.status === 413, "a streamed webhook body past its bound is refused (413)", overlong.status);
  const unsigned = await mf.dispatchFetch("http://localhost/hook", { method: "POST", body: "{}" });
  check(unsigned.status === 401, "an unsigned delivery is shed before admission (401)", unsigned.status);
  const forged = await mf.dispatchFetch("http://localhost/hook", { method: "POST", body: "{}", headers: { "X-Rtao-Signature": shaped() } });
  check(forged.status === 401, "a well-shaped header with a wrong MAC is refused after the body (401)", forged.status);
  // A body that starts and then stalls: cancelled at the provider's 5 s
  // acknowledgement window (408), holding nothing after.
  const stall = new ReadableStream({ start(c) { c.enqueue(new Uint8Array(16).fill(123)); } });
  const t0 = Date.now();
  const stalled = await mf.dispatchFetch("http://localhost/hook", { method: "POST", body: stall, duplex: "half", headers: { "X-Rtao-Signature": shaped() } });
  const waited = Date.now() - t0;
  check(stalled.status === 408 && waited >= 4500 && waited < 20000, "a stalled webhook body is cancelled at the acknowledgement window (408)", [stalled.status, waited]);
  const other = await mf.dispatchFetch("http://localhost/nope");
  check(other.status === 404, "an unknown route is 404", other.status);

  // Warm up, then many requests: nothing may accumulate.
  await status(mf);
  const warm = (await status(mf)).body;
  for (let i = 0; i < rounds; i++) {
    const r = await status(mf);
    if (r.code !== 200) {
      check(false, `request ${i} answered`, r.code);
      break;
    }
  }
  const last = (await status(mf)).body;
  const w = warm?.arena, l = last?.arena;
  const wj = warm?.chain_journal?.instance?.arena, lj = last?.chain_journal?.instance?.arena;
  console.log(JSON.stringify({ worker_after_warmup: pick(w), worker_after: pick(l), journal_after_warmup: pick(wj), journal_after: pick(lj), routes: lj?.routes, home: lj?.home }));
  const hook = l?.routes?.["worker/hook"];
  check(hook?.opened >= 4 && hook?.quarantined === 0 && hook?.overdrafts === 0 && hook?.abandoned === 0, "refused, rejected and stalled deliveries stay inside their footprint and leave nothing behind", hook);
  check(l?.memory_bytes <= linear && lj?.memory_bytes <= linear, "linear memory stayed within the ceiling", [l?.memory_bytes, lj?.memory_bytes]);
  check(l?.quarantined_scopes === 0 && l?.abandoned_scopes === 0 && lj?.quarantined_scopes === 0 && lj?.abandoned_scopes === 0, "no scope quarantined or abandoned", [l?.quarantined_scopes, lj?.quarantined_scopes]);
  check(l?.in_use_pages === w?.in_use_pages && lj?.in_use_pages === wj?.in_use_pages, "pages in use return to their idle value", [w?.in_use_pages, l?.in_use_pages, wj?.in_use_pages, lj?.in_use_pages]);
  check(l?.scopes_opened - w?.scopes_opened >= rounds, "every request opened its own scope", l?.scopes_opened - w?.scopes_opened);
  check(l?.alloc_failures === 0 && lj?.alloc_failures === 0, "no allocation failed", [l?.alloc_failures, lj?.alloc_failures]);
  check(l?.glue_spills === 0 && lj?.glue_spills === 0, "no glue spilled (no unwrapped entry point ran)", [l?.glue_spills, lj?.glue_spills]);
} finally {
  await mf.dispose();
}

// ------------------------------------------------- deliveries and a mock chain
// The fixture receiver of src/testing.rs (public test values).
const SECRET = Buffer.from("42".repeat(32), "hex");
const SUBSCRIPTION = "0x" + "11".repeat(32);
const HMAC_DOMAIN = Buffer.from("rtao.webhooks.hmac-sha256.v1\0", "latin1");
const hex32 = (tag, n) => "0x" + tag + BigInt(n).toString(16).padStart(16, "0") + "5a".repeat(23);
const blockHash = (n) => hex32("b1", n);
const be = (n, bytes) => {
  const b = Buffer.alloc(bytes);
  if (bytes === 8) b.writeBigUInt64BE(BigInt(n));
  else b.writeUInt16BE(n);
  return b;
};
function mac(fields, raw) {
  const h = createHmac("sha256", SECRET);
  h.update(HMAC_DOMAIN);
  h.update(Buffer.from([fields.version]));
  h.update(be(Buffer.byteLength(fields.key_id), 8));
  h.update(Buffer.from(fields.key_id));
  h.update(be(fields.timestamp, 8));
  for (const id of [fields.subscription, fields.event_id, fields.delivery_id]) h.update(Buffer.from(id.slice(2), "hex"));
  h.update(be(fields.attempt, 2));
  h.update(be(raw.length, 8));
  h.update(raw);
  return "0x" + h.digest("hex");
}
function delivery(n) {
  const event_id = hex32("e1", n), delivery_id = hex32("d1", n);
  const raw = Buffer.from(JSON.stringify({
    event_id,
    event: {
      version: 1, chain: GENESIS, journal_position: String(n), observed_at_ms: String(Date.now()),
      kind: { type: "finalized", block: {
        number: String(n), hash: blockHash(n), parent_hash: blockHash(n - 1), state_root: hex32("ee", n),
        extrinsics_root: hex32("ef", n), chain_timestamp_ms: String(Date.now()), state_readiness: "header_only",
      } },
    },
  }));
  const fields = { version: 1, key_id: "pilot-v1", timestamp: Math.floor(Date.now() / 1000), subscription: SUBSCRIPTION, event_id, delivery_id, attempt: 1 };
  const header = JSON.stringify({
    version: 1, key_id: "pilot-v1", timestamp: String(fields.timestamp),
    context: { subscription: SUBSCRIPTION, event_id, delivery_id }, attempt: 1, mac: mac(fields, raw),
  });
  return { raw, header };
}
// A mock chain at `head` on a real loopback HTTP server, answering the
// watchdog's JSON-RPC calls. The coordinator's fetch reaches it through a
// small JS relay Worker (its outbound service), so the reply crosses
// workerd's own network fetch: with `gzip`, the server compresses, and the
// coordinator sees what it sees on Cloudflare, the decoded body with the
// encoded Content-Length and Content-Encoding gzip.
async function chainServer(state) {
  const srv = http.createServer(async (req, res) => {
    let raw = "";
    for await (const c of req) raw += c;
    let body = {};
    try { body = JSON.parse(raw); } catch {}
    state.calls[body.method] = (state.calls[body.method] ?? 0) + 1;
    if (process.env.SMOKE_TRACE) console.error("rpc", body.method);
    let result = null;
    if (body.method === "chain_getFinalizedHead") result = blockHash(state.head);
    else if (body.method === "chain_getHeader") {
      const n = Number(BigInt("0x" + body.params[0].slice(4, 20)));
      result = { number: "0x" + n.toString(16), parentHash: blockHash(n - 1), stateRoot: hex32("ee", n), extrinsicsRoot: hex32("ef", n), digest: { logs: [] } };
    } else if (body.method === "chain_getBlockHash") result = body.params[0].map((h) => (h <= state.head ? blockHash(h) : null));
    else {
      res.writeHead(503).end("not mocked");
      return;
    }
    // A padded reply, so the gzip body is much shorter than the decoded one.
    const text = Buffer.from(JSON.stringify({ jsonrpc: "2.0", id: body.id, result, pad: " ".repeat(state.pad) }));
    const out = state.gzip ? gzipSync(text) : text;
    const headers = { "content-type": "application/json", "content-length": String(out.length) };
    if (state.gzip) headers["content-encoding"] = "gzip";
    res.writeHead(200, headers).end(out);
  });
  await new Promise((r) => srv.listen(0, "127.0.0.1", r));
  return srv;
}

async function deliveries(gzip) {
  const state = { head: 2_000_040, calls: {}, gzip, pad: 16_000 };
  const srv = await chainServer(state);
  const port = srv.address().port;
  const relay = {
    name: "rpc-relay",
    modules: true,
    compatibilityDate: "2026-10-01",
    script: `export default { async fetch(req) {
      return fetch("http://127.0.0.1:${port}/", { method: "POST", body: await req.text(), headers: { "content-type": "application/json", "accept-encoding": req.headers.get("accept-encoding") ?? "" } });
    } }`,
  };
  const mf = new Miniflare(convertV4MiniflareOptions({ unsafeTriggerHandlers: true, workers: [{ ...config("coordinator", "rpc-relay"), name: "coordinator" }, relay] }));
  const label = gzip ? "gzip" : "plain";
  try {
    const statuses = [];
    for (let n = 2_000_001; n <= 2_000_040; n++) {
      const { raw, header } = delivery(n);
      const r = await mf.dispatchFetch("http://localhost/hook", { method: "POST", body: raw, headers: { "X-Rtao-Signature": header, "content-type": "application/json" } });
      statuses.push(r.status);
      const t = await r.text();
      if (process.env.SMOKE_TRACE) console.error("delivery", n, r.status, t.slice(0, 120));
    }
    // Let the last continuations settle (each is bounded by its 30 s budget;
    // the mock answers at once).
    await new Promise((res) => setTimeout(res, 3000));
    if (process.env.SMOKE_TRACE) console.error("before cron", JSON.stringify((await status(mf)).body?.chain_journal?.counters ?? {}).slice(0, 400));
    // The cron export, through miniflare's local trigger route.
    if (process.env.SMOKE_TRACE) console.error("scheduled: start");
    const cron = await Promise.race([
      mf.dispatchFetch("http://localhost/cdn-cgi/local/scheduled?cron=*+*+*+*+*").then(async (r) => ({ done: r.status, text: (await r.text()).slice(0, 80) })),
      new Promise((res) => setTimeout(() => res({ timeout: true }), 25_000)),
    ]);
    if (process.env.SMOKE_TRACE) console.error("scheduled:", JSON.stringify(cron));
    check(cron.done >= 200 && cron.done < 300, `${label}: the cron tick completed`, cron);
    const st = await status(mf);
    const hb = st.body?.chain_journal ?? {};
    const flat = JSON.stringify(hb);
    const num = (k) => {
      const m = flat.match(new RegExp(`"${k}":(\\d+)`));
      return m ? Number(m[1]) : null;
    };
    const a = st.body?.arena, ja = st.body?.chain_journal?.instance?.arena;
    console.log(JSON.stringify({ phase: `deliveries-${label}`, statuses: [...new Set(statuses)], calls: state.calls, anchors: num("anchors"), rpc_failures: num("rpc_failures"), applied_rpc: num("applied_rpc"), worker: pick(a), journal: pick(ja), routes: a?.routes }));
    check(statuses.every((s) => s >= 200 && s < 300), `${label}: every authenticated delivery is acknowledged (2xx)`, [...new Set(statuses)]);
    check((state.calls.chain_getFinalizedHead ?? 0) >= 1, `${label}: the continuation made the watchdog's RPC reads`, state.calls);
    check(num("rpc_failures") === 0 || num("rpc_failures") === null && (state.calls.chain_getFinalizedHead ?? 0) <= 2, `${label}: no RPC read failed (the reply's encoding is decoded and bounded, not refused)`, { rpc_failures: num("rpc_failures"), calls: state.calls });
    const cont = a?.routes?.["worker/hook-continuation"];
    check(cont?.opened >= 1 && cont?.abandoned === 0 && cont?.quarantined === 0, `${label}: the delivery continuations ran in their scopes and left nothing behind`, cont);
    const sched = a?.routes?.["worker/scheduled"];
    check(sched?.opened >= 1 && sched?.alloc_failures === 0, `${label}: the cron export ran in its own scope`, sched);
    check(a?.alloc_failures === 0 && ja?.alloc_failures === 0, `${label}: no allocation failed`, [a?.alloc_failures, ja?.alloc_failures]);
    check(a?.abandoned_scopes === 0 && ja?.abandoned_scopes === 0, `${label}: no scope abandoned`, [a?.abandoned_scopes, ja?.abandoned_scopes]);
    check(a?.memory_bytes <= linear, `${label}: linear memory stayed within the ceiling`, a?.memory_bytes);
  } finally {
    await mf.dispose();
    srv.close();
  }
}
await deliveries(false);
await deliveries(true);

// ------------------------------------------------------------ eval role
const ev = new Miniflare(options("eval"));
try {
  const r = await status(ev, "/eval/status");
  // Without the signing keys the cohort answers 503 not_configured from
  // inside its own scope; either way the class constructed and answered.
  check(r.code === 200 || (r.code === 503 && r.text.includes("not_configured")), "the EvalCohort constructs and answers in its scope", [r.code, r.text.slice(0, 80)]);
  const s = await status(ev);
  const e = s.body?.arena;
  check(e?.memory_bytes <= linear && e?.quarantined_scopes === 0, "the eval Worker runs within the same ceiling", pick(e));
} finally {
  await ev.dispose();
}

console.log(JSON.stringify({ failures }));
process.exit(failures.length === 0 ? 0 : 1);
