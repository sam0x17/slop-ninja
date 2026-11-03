// Drill driver: serves the drill services from local `workerd serve` on a
// loopback socket (no remote bindings, no network beyond 127.0.0.1, no
// deploy), runs a plan's scenarios one invocation per request, and prints
// SELFTEST, MODEL, RESULT, SERIES, JSHEAP and CONSISTENCY lines.
//
// With --inspect, workerd's inspector (127.0.0.1) is attached to each drill
// isolate: after every invocation `Runtime.getHeapUsage` is read (JS heap and
// backing stores, which include the wasm memory), and every `gc_every`
// invocations `HeapProfiler.collectGarbage` is forced first, so both the
// garbage-backlog high-water and the live JS after collection are recorded.
//
// Usage: node drive.mjs <out-dir> <plan.json> <workerd> [--inspect]
// <out-dir> must hold drill-default.wasm and drill-arena.wasm.
import { execFileSync, spawn } from "node:child_process";
import { copyFileSync, readFileSync, writeFileSync } from "node:fs";
import { createServer } from "node:net";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const HERE = dirname(fileURLToPath(import.meta.url));
const [OUT, PLAN, WORKERD, ...rest] = process.argv.slice(2);
const INSPECT = rest.includes("--inspect");
const MiB = 1048576;
const ST_PENDING = 0x7001;

function freePort() {
  return new Promise((res) => {
    const s = createServer();
    s.listen(0, "127.0.0.1", () => {
      const p = s.address().port;
      s.close(() => res(p));
    });
  });
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

const HEX = Array.from({ length: 256 }, (_, i) => i.toString(16).padStart(2, "0"));
const SCHEMA = "slop_ninja_arena_drill_package_v1";

/// Deterministic compact body of `n` receipts with ascending attempt ids,
/// generated here (outside workerd) and sent as a request body.
function generate(n, seed) {
  let x = (seed | 1) >>> 0;
  const next = () => {
    x ^= x << 13;
    x >>>= 0;
    x ^= x >>> 17;
    x ^= x << 5;
    x >>>= 0;
    return x;
  };
  const hex = (k) => {
    let s = "";
    for (let i = 0; i < k; i++) s += HEX[next() & 255];
    return s;
  };
  const parts = [`{"schema":"${SCHEMA}","cohort_manifest":"${hex(32)}","artifact":"${hex(32)}","count":${n},"receipts":[`];
  for (let i = 0; i < n; i++) {
    const pfx = HEX[(i >>> 24) & 255] + HEX[(i >>> 16) & 255] + HEX[(i >>> 8) & 255] + HEX[i & 255];
    parts.push(
      `${i ? "," : ""}{"attempt":"${pfx}${hex(28)}","assignment":"${hex(32)}","number":1,"outcome":${1 + (next() % 5)},"result_commitment":"${hex(32)}"}`,
    );
  }
  parts.push("]}");
  return Buffer.from(parts.join(""));
}

const bodies = new Map();
function body(n, seed) {
  const key = `${n}:${seed}`;
  if (!bodies.has(key)) bodies.set(key, generate(n, seed));
  return bodies.get(key);
}

class Inspector {
  constructor(url) {
    this.url = url;
    this.id = 0;
    this.waiting = new Map();
  }
  open() {
    return new Promise((res, rej) => {
      this.ws = new WebSocket(this.url);
      this.ws.onopen = () => res(this);
      this.ws.onerror = (e) => rej(new Error(`inspector ${this.url}: ${e.message ?? e.type}`));
      this.ws.onmessage = (m) => {
        const msg = JSON.parse(typeof m.data === "string" ? m.data : Buffer.from(m.data).toString());
        if (!msg.id && process.env.INSPECT_DEBUG) console.log("INSPECTOR-EVENT " + JSON.stringify(msg).slice(0, 300));
        if (msg.id && this.waiting.has(msg.id)) {
          this.waiting.get(msg.id)(msg);
          this.waiting.delete(msg.id);
        }
      };
    });
  }
  call(method, params = {}, timeoutMs = 10000) {
    const id = ++this.id;
    return new Promise((res) => {
      const t = setTimeout(() => {
        this.waiting.delete(id);
        res({ error: { message: `timeout after ${timeoutMs} ms`, method } });
      }, timeoutMs);
      this.waiting.set(id, (m) => {
        clearTimeout(t);
        res(m);
      });
      this.ws.send(JSON.stringify({ id, method, params }));
    });
  }
  close() {
    try {
      this.ws.close();
    } catch {}
  }
}

async function inspectorFor(iport, name) {
  let targets = null;
  for (let i = 0; i < 50 && !targets; i++) {
    try {
      targets = await (await fetch(`http://127.0.0.1:${iport}/json/list`)).json();
    } catch {
      await sleep(100);
    }
  }
  const t = (targets ?? []).find((x) => (x.title ?? "").includes(name) || (x.webSocketDebuggerUrl ?? "").endsWith(`/${name}`));
  const url = t?.webSocketDebuggerUrl ?? `ws://127.0.0.1:${iport}/${name}`;
  const insp = await new Inspector(url).open();
  // Probe the session once; report what the inspector answers.
  const hello = await insp.call("Runtime.getHeapUsage", {}, 5000);
  console.log("INSPECTOR-HELLO " + JSON.stringify({ name, hello }).slice(0, 600));
  return { insp, targets: (targets ?? []).map((x) => ({ title: x.title, url: x.webSocketDebuggerUrl })) };
}

let WD_PID = null;
/// workerd's resident set (all isolates, wasm memories included), from /proc.
function rss() {
  try {
    const m = /VmRSS:\s+(\d+) kB/.exec(readFileSync(`/proc/${WD_PID}/status`, "utf8"));
    return m ? Number(m[1]) * 1024 : null;
  } catch {
    return null;
  }
}

async function heap(insp, gc) {
  if (gc) await insp.call("HeapProfiler.collectGarbage");
  const r = await insp.call("Runtime.getHeapUsage");
  return { ...(r.result ?? { error: r.error }), rss: rss() };
}

function fateFor(sc, i) {
  for (const a of sc.async ?? []) {
    if (i % a.every === a.offset) return a;
  }
  return null;
}

/// The shape of invocation `i`: cycling `shapes`, or by `phases` (each
/// `{invocations, shapes}`, run in order: small shapes first, then mixed).
function shapeFor(sc, i) {
  if (!sc.phases) return sc.shapes[(i - 1) % sc.shapes.length];
  let start = 1;
  for (const ph of sc.phases) {
    if (i < start + ph.invocations) return ph.shapes[(i - start) % ph.shapes.length];
    start += ph.invocations;
  }
  const last = sc.phases[sc.phases.length - 1];
  return last.shapes[(i - 1) % last.shapes.length];
}
const shapesOf = (sc) => (sc.phases ? [...new Set(sc.phases.flatMap((p) => p.shapes))] : sc.shapes);

const ST_TRAP = 0x7005;
/// Host-side status: the invocation's route is durably paused
/// (`arena_leak`); the work waits for an operator clear and is never skipped.
const ST_PAUSED = 0x7006;
/// The drill's routes: `ROUTE` (1, synchronous) and `ROUTE_ASYNC` (2).
const routeOf = (flags) => (flags & 4 ? 2 : 1);

async function scenario(base, sc, variant, inspector, model) {
  const svc = `drill-${variant}-${sc.name}`;
  const call = async (q, b) => (await fetch(`${base}/${svc}/invoke?${q}`, { method: "POST", body: b })).json();
  const stepMs = sc.step_ms ?? 1000;
  const after = [];
  const gens = [];
  const statuses = {};
  const digests = {};
  const pending = [];
  // The coordinator's durable state, kept here (the drill host stands in for
  // the Durable Object's SQL): recycle intents keyed by (route, cause kind,
  // memory-model version) with the stop rule, pauses, step crashes.
  const durable = { arena_poisoned: 0, arena_trap: [], step_crashes: 0, recycle_intents: [], arena_leak: [] };
  const recycleCount = new Map();
  const paused = new Set();
  // How each instance generation ended: a recycle (with its intent) or a
  // trap (with the abort hook's postmortem route).
  const endOf = new Map();
  const attributed = { recycle: 0, other_trap: 0, crash: 0 };
  let postmortemMatches = 0;
  let pausedInvocations = 0;
  let recycles = 0;
  let abortedByReinit = 0;
  let pendingMax = 0;
  let peakLive = 0;
  let gen = 0;
  const genStats = new Map();
  const heapSamples = [];
  let first = null;
  let last = null;
  const arenaMax = { glue_used_pages: 0, glue_live: 0, tickets_peak: 0, pinned_pages: 0, abandoned_scopes: 0, quarantined_scopes: 0, open_scopes_after: 0, stack_high_water: 0, route_bound_exceeded: 0, stale_allocs: 0 };
  const violations = { unscoped: 0, overdrafts: 0, leaks: 0 };
  const invoke = async (i, flags, target, shape) => {
    const receipts = shape ?? shapeFor(sc, i);
    const now = i * stepMs;
    const r = await call(`receipts=${receipts}&flags=${flags}&now=${now}&target=${target}`, flags & 64 ? new Uint8Array(0) : body(receipts, sc.seed ?? 1));
    r.receipts = receipts;
    if (r.stats?.arena) {
      genStats.set(r.generation, r.stats.arena);
      const a = r.stats.arena;
      for (const k of Object.keys(arenaMax)) if (k !== "open_scopes_after") arenaMax[k] = Math.max(arenaMax[k], a[k] ?? 0);
      if (!r.trap) arenaMax.open_scopes_after = Math.max(arenaMax.open_scopes_after, a.open_scopes - (r.stats.pending ?? 0));
      violations.unscoped += r.stats.violations?.unscoped ?? 0;
      violations.overdrafts += r.stats.violations?.overdrafts ?? 0;
      violations.leaks += r.stats.violations?.leaks ?? 0;
    }
    return r;
  };
  /// One invocation with the coordinator's durable protocol around it.
  /// `shape` overrides the scenario's shape (an async fate's own `shape`).
  const step = async (i, flags, target, shape) => {
    let r = await invoke(i, flags, target, shape);
    if (r.trap && r.recycle_intent) {
      // Durable recycle intent (written before the trap), keyed by cause.
      recycles++;
      durable.arena_poisoned++;
      const ri = r.recycle_intent;
      const key = `${ri.route}:${ri.kind}:${model.version}`;
      const n = (recycleCount.get(key) ?? 0) + 1;
      recycleCount.set(key, n);
      durable.recycle_intents.push({ i, ...ri, n });
      endOf.set(r.generation, { kind: "recycle", route: ri.route });
      if (n >= 2) {
        // Stop rule: the second recycle for the same cause pauses the route
        // durably (operator clear) instead of recycling it again and again.
        durable.arena_leak.push({ i, key });
        paused.add(ri.route);
      }
      if (paused.has(routeOf(flags)) && !(flags & 64)) return { ...r, status: ST_PAUSED, paused_now: true };
      // The same work on the fresh instance: never skipped.
      r = await invoke(i, flags & ~(16 | 32), 0, shape);
    } else if (r.trap) {
      // Step crash (the abort hook's postmortem names the trapping route).
      const pm = r.postmortem ? r.postmortem[2] - 1 : -1;
      if (pm === routeOf(flags)) postmortemMatches++;
      endOf.set(r.generation, { kind: "trap", route: pm >= 0 ? pm : routeOf(flags) });
      durable.step_crashes++;
      if (sc.rerun_on_trap) {
        const r2 = await invoke(i, flags & ~(16 | 32), 0, shape);
        if (r2.trap) {
          const pm2 = r2.postmortem ? r2.postmortem[2] - 1 : -1;
          if (pm2 === routeOf(flags)) postmortemMatches++;
          endOf.set(r2.generation, { kind: "trap", route: pm2 >= 0 ? pm2 : routeOf(flags) });
          durable.step_crashes++;
          durable.arena_trap.push(i);
        }
        r = r2;
      }
    }
    if (r.generation !== gen) {
      // Co-resident pending steps of the old instance: their markers are
      // stale. Attributed to the recycle or to another route's trap, they are
      // re-run without counting a crash of their own.
      for (let k = pending.length - 1; k >= 0; k--) {
        if (pending[k].gen !== r.generation) {
          const end = endOf.get(pending[k].gen);
          if (end?.kind === "recycle") attributed.recycle++;
          else if (end?.kind === "trap" && end.route !== 2) attributed.other_trap++;
          else attributed.crash++;
          pending.splice(k, 1);
          abortedByReinit++;
        }
      }
      gen = r.generation;
      gens.push({ generation: gen, first_invocation: i });
    }
    return r;
  };
  for (let i = 1; i <= sc.invocations; i++) {
    // A pending invocation due for its fate (settle or drop) in this generation.
    let fates = 0;
    let target = 0;
    const due = pending.findIndex((p) => p.due <= i && p.gen === gen);
    if (due >= 0) {
      const p = pending.splice(due, 1)[0];
      fates = p.fate === "settle" ? 16 : 32;
      target = p.slot;
    }
    let flags = (sc.flags ?? 0) | fates;
    const f = fateFor(sc, i);
    if (f) flags |= 4;
    if (sc.trap_every && i % sc.trap_every === (sc.trap_offset ?? 0)) flags |= 8;
    let r;
    if (paused.has(routeOf(flags))) {
      // Durably paused: no invocation of this route runs. A due fate is a JS
      // event (a promise settling), delivered on its own.
      pausedInvocations++;
      statuses[ST_PAUSED] = (statuses[ST_PAUSED] ?? 0) + 1;
      if (fates) await step(i, fates | 64, target);
      after.push(after.length ? after[after.length - 1] : 0);
      continue;
    }
    r = await step(i, flags, target, f?.shape);
    if (r.paused_now) {
      pausedInvocations++;
      statuses[ST_PAUSED] = (statuses[ST_PAUSED] ?? 0) + 1;
      after.push(r.after);
      continue;
    }
    if (r.status === ST_PENDING && f) {
      if (f.fate !== "abandon") pending.push({ slot: r.slot, gen: r.generation, due: i + f.after, fate: f.fate });
    }
    pendingMax = Math.max(pendingMax, r.stats?.pending ?? 0);
    if (first === null) first = r;
    after.push(r.after);
    statuses[r.status] = (statuses[r.status] ?? 0) + 1;
    if (r.status === 0 && !(flags & 4)) {
      const d = `${r.bundle_sha256}/${r.index_root}/${r.count}/${r.bundle_len}`;
      (digests[r.receipts] ??= new Set()).add(d);
    }
    peakLive = Math.max(peakLive, r.peak_live);
    if (inspector) {
      const gc = i % (sc.gc_every ?? 100) === 0 || i === sc.invocations;
      const h = await heap(inspector, gc);
      heapSamples.push({ i, gc, wasm: r.after, receipts: r.receipts, js_need: r.js_need, ...h });
    }
    last = r;
  }
  // Cumulative counters: the last snapshot of each generation, summed.
  const sum = {};
  for (const a of genStats.values()) {
    for (const k of ["abandoned", "abandon_recovered", "abandon_reclaimed", "quarantines", "quarantine_releases", "refusals_retry", "refusals_capacity", "refusals_poisoned", "overdrafts", "alloc_failures", "tickets_refused", "children_carved", "scopes_released", "blocks_served", "stale_allocs", "route_bound_exceeded"]) {
      sum[k] = (sum[k] ?? 0) + (a[k] ?? 0);
    }
  }
  last ??= first;
  const ceiling = last?.stats?.arena ? last.stats.arena.static_bytes + last.stats.arena.limit_pages * 65536 : null;
  const out = {
    scenario: sc.name,
    variant,
    shapes: shapesOf(sc),
    invocations: sc.invocations,
    input_len: first?.input_len,
    memory_at_instantiate: first?.memory_at_instantiate,
    memory_after: { 1: after[0], 2: after[1], 10: after[9] ?? null, 100: after[99] ?? null, [sc.invocations]: after[after.length - 1] },
    memory_max: Math.max(...after),
    ceiling,
    within_ceiling: ceiling === null ? null : Math.max(...after) <= ceiling,
    growth_after_2: after[after.length - 1] - after[1],
    generations: gens.length,
    recycles,
    recycles_by_cause: Object.fromEntries(recycleCount),
    recycles_per_cause_max: Math.max(0, ...recycleCount.values()),
    durable,
    attributed,
    postmortem_matches: postmortemMatches,
    paused_invocations: pausedInvocations,
    aborted_by_reinit: abortedByReinit,
    pending_max: pendingMax,
    peak_live_max: peakLive,
    statuses,
    digests: Object.fromEntries(Object.entries(digests).map(([k, v]) => [k, [...v]])),
    arena_max: last?.stats?.arena ? arenaMax : null,
    arena_sum: last?.stats?.arena ? sum : null,
    violations: last?.stats?.arena ? violations : null,
    last_stats: last?.stats,
  };
  return { out, after, heapSamples };
}

const plan = JSON.parse(await import("node:fs").then((fs) => fs.readFileSync(PLAN, "utf8")));
for (const f of ["drill.mjs", "router.mjs"]) copyFileSync(join(HERE, f), join(OUT, f));
copyFileSync(PLAN, join(OUT, "plan.json"));
const port = await freePort();
const iport = INSPECT ? await freePort() : null;
writeFileSync(join(OUT, "config.capnp"), execFileSync("node", [join(HERE, "config.mjs"), join(OUT, "plan.json"), String(port)]));
const args = ["serve", "config.capnp"];
if (INSPECT) args.push(`--inspector-addr=127.0.0.1:${iport}`);
const wd = spawn(WORKERD, args, { cwd: OUT, stdio: ["ignore", "inherit", "inherit"] });
WD_PID = wd.pid;
let exited = null;
wd.on("exit", (c, s) => (exited = { code: c, signal: s }));
const base = `http://127.0.0.1:${port}`;
let ready = false;
for (let i = 0; i < 300 && !ready && !exited; i++) {
  try {
    await fetch(`${base}/none`);
    ready = true;
  } catch {
    await sleep(100);
  }
}
let failed = !ready;
try {
  if (!ready) throw new Error(`workerd did not start: ${JSON.stringify(exited)}`);
  const firstSvc = `drill-arena-${plan.scenarios[0].name}`;
  if (plan.selftest !== false) {
    const st = await (await fetch(`${base}/${firstSvc}/selftest`)).json();
    console.log("SELFTEST " + JSON.stringify(st));
    failed ||= st.fails !== 0 || st.trap !== null;
  }
  const model = await (await fetch(`${base}/${firstSvc}/model`)).json();
  console.log("MODEL " + JSON.stringify(model));
  const consistency = {};
  for (const sc of plan.scenarios) {
    const byVariant = {};
    for (const variant of sc.variants ?? plan.variants) {
      let inspector = null;
      if (INSPECT) {
        const { insp, targets } = await inspectorFor(iport, `drill-${variant}-${sc.name}`);
        inspector = insp;
        console.log("INSPECTOR " + JSON.stringify({ scenario: sc.name, variant, url: insp.url, targets: targets.length }));
      }
      const { out, after, heapSamples } = await scenario(base, sc, variant, inspector, model);
      inspector?.close();
      byVariant[variant] = out;
      console.log("RESULT " + JSON.stringify(out));
      console.log("SERIES " + JSON.stringify({ scenario: sc.name, variant, after }));
      if (heapSamples.length) console.log("JSHEAP " + JSON.stringify({ scenario: sc.name, variant, samples: heapSamples }));
      if (variant === "arena" && out.within_ceiling === false) failed = true;
      if (variant === "arena" && sc.expect) {
        for (const [k, v] of Object.entries(sc.expect)) {
          const got = k.split(".").reduce((o, p) => o?.[p], out);
          const ok = typeof v === "object" ? (v.min === undefined || got >= v.min) && (v.max === undefined || got <= v.max) : got === v;
          if (!ok) {
            failed = true;
            console.log("EXPECT-FAIL " + JSON.stringify({ scenario: sc.name, key: k, want: v, got }));
          }
        }
      }
    }
    const variants = sc.variants ?? plan.variants;
    const shapes = new Set(shapesOf(sc).map(String));
    const same = [...shapes].every((s) => {
      const ds = variants.map((v) => JSON.stringify(byVariant[v].digests[s] ?? []));
      return ds.every((d) => d === ds[0]) && (byVariant[variants[0]].digests[s] ?? []).length <= 1;
    });
    consistency[sc.name] = same;
    if (!same && !sc.expect_refusals) failed = true;
  }
  console.log("CONSISTENCY " + JSON.stringify(consistency));
} catch (e) {
  console.log("DRIVER-ERROR " + String(e?.stack ?? e));
  failed = true;
} finally {
  wd.kill("SIGTERM");
}
process.exit(failed ? 1 : 0);
