// Glue probe driver: serves a worker-build output from local `workerd serve`
// on 127.0.0.1 and measures the arena's glue counters per entry kind.
//
// For each kind (Worker fetch, Durable Object fetch, alarm, queue, cron) and
// k in-flight invocations, it resets the glue peak, fires k invocations that
// each hold for HOLD ms inside their handler scope, waits for them, and reads
// the glue high-water (requested bytes and dlmalloc footprint). The per
// invocation cost is the least-squares slope over k. Durable Object
// residency is the live glue added per new instance. A trap test checks what
// a trap does to a co-resident in-flight request and to the instance, and
// that the abort hook handed the dying instance's postmortem to the next.
// With every export hand-written (the admitted build) each entry takes its
// own ticket, so the cost per entry kind is derived: a Durable Object
// fetch's own glue is the Worker-to-Durable-Object slope minus the Worker
// fetch slope.
//
// Usage: node drive.mjs <out-dir> <build-dir> <workerd> <label>
import { execFileSync, spawn } from "node:child_process";
import { copyFileSync, mkdirSync, writeFileSync } from "node:fs";
import { createServer } from "node:net";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const HERE = dirname(fileURLToPath(import.meta.url));
const [OUT, BUILD, WORKERD, LABEL] = process.argv.slice(2);
const HOLD = 400;
const KS = [1, 2, 4, 8, 16];
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

function freePort() {
  return new Promise((res) => {
    const s = createServer();
    s.listen(0, "127.0.0.1", () => {
      const p = s.address().port;
      s.close(() => res(p));
    });
  });
}

copyFileSync(join(HERE, "router.mjs"), join(OUT, "router.mjs"));
// workerd embeds paths relative to the config file.
mkdirSync(join(OUT, "build"), { recursive: true });
for (const f of ["index.js", "index_bg.wasm"]) copyFileSync(join(resolve(BUILD), f), join(OUT, "build", f));
const port = await freePort();
writeFileSync(join(OUT, "config.capnp"), execFileSync("node", [join(HERE, "config.mjs"), "build", String(port)]));
// `--experimental` enables `service_binding_extra_handlers` (queue and cron dispatch).
const wd = spawn(WORKERD, ["serve", "--experimental", "config.capnp"], { cwd: OUT, stdio: ["ignore", "inherit", "inherit"] });
let exited = null;
wd.on("exit", (c, s) => (exited = { code: c, signal: s }));
const base = `http://127.0.0.1:${port}`;
const get = async (path) => {
  const r = await fetch(base + path);
  return { status: r.status, text: await r.text() };
};
const stats = async (path = "/stats") => JSON.parse((await get(path)).text);

function fit(points) {
  const n = points.length;
  const sx = points.reduce((a, p) => a + p[0], 0);
  const sy = points.reduce((a, p) => a + p[1], 0);
  const sxx = points.reduce((a, p) => a + p[0] * p[0], 0);
  const sxy = points.reduce((a, p) => a + p[0] * p[1], 0);
  const slope = (n * sxy - sx * sy) / (n * sxx - sx * sx);
  const intercept = (sy - slope * sx) / n;
  const maxResidual = Math.max(...points.map(([x, y]) => Math.abs(y - (slope * x + intercept))));
  return { slope, intercept, max_residual: maxResidual };
}

async function measure(kind, fire, setup) {
  const rows = [];
  for (const k of KS) {
    if (setup) await setup(k);
    await get("/reset");
    const before = await stats();
    await Promise.all(Array.from({ length: k }, (_, i) => fire(i, k)));
    if (kind === "alarm" || kind === "wait_until") await sleep(HOLD + 300);
    const after = await stats();
    rows.push({
      k,
      glue_live_before: before.arena.glue_live,
      glue_peak_delta: after.arena.glue_peak - before.arena.glue_peak,
      glue_fp_peak_delta: after.arena.glue_fp_peak - before.arena.glue_fp_peak,
      glue_used_pages: after.arena.glue_used_pages,
      tickets_peak: after.arena.tickets_peak,
      overdrafts: after.arena.overdrafts,
      quarantines: after.arena.quarantines,
    });
  }
  const bytes = fit(rows.map((r) => [r.k, r.glue_peak_delta]));
  const fp = fit(rows.map((r) => [r.k, r.glue_fp_peak_delta]));
  const out = { label: LABEL, kind, rows, per_invocation_bytes: bytes, per_invocation_footprint: fp };
  console.log("GLUE " + JSON.stringify(out));
  return out;
}

let failed = false;
try {
  for (let i = 0; i < 300; i++) {
    try {
      await fetch(`${base}/stats`);
      break;
    } catch {
      if (exited) throw new Error(`workerd exited ${JSON.stringify(exited)}`);
      await sleep(100);
    }
  }
  const idle = await stats();
  console.log("IDLE " + JSON.stringify({ label: LABEL, arena: idle.arena }));
  const results = {};
  results.fetch = await measure("fetch", () => get(`/hold?ms=${HOLD}`));
  // Long URLs: the macro export converts the URL into Rust strings in glue.
  const pad = "x".repeat(4000);
  results.fetch_long_url = await measure("fetch_long_url", () => get(`/hold?ms=${HOLD}&pad=${pad}`));
  let gen = 0;
  const names = (k) => Array.from({ length: k }, (_, i) => `g${gen}-${i}`);
  results.do_fetch = await measure(
    "do_fetch",
    (i) => get(`/do/hold?ms=${HOLD}&name=g${gen}-${i}`),
    async (k) => {
      gen++;
      for (const n of names(k)) await get(`/do/ping?name=${n}`);
    },
  );
  results.alarm = await measure(
    "alarm",
    (i) => get(`/do/alarm?ms=${HOLD}&name=g${gen}-${i}`),
    async (k) => {
      gen++;
      for (const n of names(k)) await get(`/do/ping?name=${n}`);
    },
  );
  results.queue = await measure("queue", () => get(`/queue?ms=${HOLD}`));
  results.scheduled = await measure("scheduled", () => get("/scheduled"));
  results.wait_until = await measure("wait_until", () => get(`/wait?ms=${HOLD}`));
  // Glue per entry (footprint slope): the ticket cost of each entry kind.
  const fp = (k) => results[k].per_invocation_footprint.slope;
  const kinds = {
    label: LABEL,
    fetch: fp("fetch"),
    fetch_long_url: fp("fetch_long_url"),
    do_fetch_entry: fp("do_fetch") - fp("fetch"),
    do_fetch_chain: fp("do_fetch"),
    alarm: fp("alarm"),
    queue: fp("queue"),
    scheduled: fp("scheduled"),
    wait_until: fp("wait_until"),
    tickets_per_do_request: results.do_fetch.rows[results.do_fetch.rows.length - 1].tickets_peak / KS[KS.length - 1],
  };
  console.log("KINDS " + JSON.stringify(kinds));
  // Residency: live glue added per new Durable Object instance.
  await get("/reset");
  const r0 = await stats();
  const N = 64;
  for (let i = 0; i < N; i++) await get(`/do/ping?name=resident-${i}`);
  const r1 = await stats();
  const resident = {
    label: LABEL,
    instances: N,
    glue_live_per_instance: (r1.arena.glue_live - r0.arena.glue_live) / N,
    glue_fp_live_per_instance: (r1.arena.glue_fp_live - r0.arena.glue_fp_live) / N,
    glue_allocs_per_instance: (r1.arena.glue_allocs_live - r0.arena.glue_allocs_live) / N,
  };
  console.log("RESIDENT " + JSON.stringify(resident));
  // Ticket burst: more concurrent requests than the derived ticket count.
  // The admitted export answers the excess with a JS-only 503 before any
  // glue is allocated; glue stays inside its partition either way.
  {
    await get("/reset");
    const s0 = await stats();
    const limit = s0.arena.ticket_limit;
    const n = Math.min(limit + 32, 2000);
    const rs = await Promise.all(Array.from({ length: n }, () => get(`/hold?ms=${HOLD * 2}`)));
    const s1 = await stats();
    const burst = {
      label: LABEL,
      ticket_limit: limit,
      requests: n,
      ok: rs.filter((r) => r.status === 200).length,
      refused_503: rs.filter((r) => r.status === 503).length,
      other: rs.filter((r) => r.status !== 200 && r.status !== 503).length,
      tickets_peak: s1.arena.tickets_peak,
      tickets_refused: s1.arena.tickets_refused - s0.arena.tickets_refused,
      glue_used_pages_max: s1.arena.glue_used_pages,
      glue_budget_pages: s1.arena.glue_budget_pages,
      glue_fp_peak: s1.arena.glue_fp_peak,
      glue_alloc_failures: s1.arena.last_failure_glue ? s1.arena.alloc_failures : 0,
    };
    console.log("BURST " + JSON.stringify(burst));
  }
  // Trap: one request traps while another is in flight in the same isolate.
  const inst0 = (await get("/instance")).text;
  const held = get("/hold?ms=1500");
  await sleep(200);
  const trap = await get("/trap");
  const heldResult = await held;
  const inst1 = (await get("/instance")).text;
  const after = await stats();
  // The abort hook's postmortem, read by the new instance: [version, scope,
  // route + 1, failure kind, glue, failure route, failure size, recycle
  // kind, recycle route, recycle pages, pinned, stack high-water, alloc
  // failures, epoch].
  const pm = after.prev_postmortem;
  const trapOut = {
    label: LABEL,
    instance_before: inst0,
    trap_status: trap.status,
    co_resident_status: heldResult.status,
    co_resident_body: heldResult.text.slice(0, 200),
    instance_after: inst1,
    reinstantiated: inst0 !== inst1,
    epoch_after: after.arena.epoch,
    postmortem: pm,
    postmortem_route: pm && pm[0] === 1 ? pm[2] - 1 : null,
    postmortem_names_the_trapping_route: !!pm && pm[0] === 1 && pm[2] - 1 === 1,
  };
  console.log("TRAP " + JSON.stringify(trapOut));
  const final = await stats();
  console.log("FINAL " + JSON.stringify({ label: LABEL, arena: final.arena }));
} catch (e) {
  console.log("DRIVER-ERROR " + String(e?.stack ?? e));
  failed = true;
} finally {
  wd.kill("SIGTERM");
}
process.exit(failed ? 1 : 0);
