// Drill Worker: one wasm instance per isolate, one handler call per fetch
// invocation. The module is either build of the drill crate (default
// allocator or arena); both export the same functions.
//
// After a trap the instance is replaced on the next call, as worker-build
// 0.8.7's panic recovery does (`--experimental-reset-state-function`,
// `schedule_reinit`): everything the old instance held, pending invocations
// included, is gone. Before that, as worker-build's `re()` calls the
// registered `__abort_handler`, the host calls the module's abort hook
// (`drill_on_abort`), which hands the postmortem out through an import; the
// dying instance's memory is never read (worker-build cannot read it either:
// the new instance has fresh memory).
import wasmModule from "drill.wasm";

const HEX = Array.from({ length: 256 }, (_, i) => i.toString(16).padStart(2, "0"));
const BUNDLE_ENTRY = 155;
const OUT_WORDS = 30;

let exp = null;
let generation = 0;
let memoryAtInstantiate = 0;
// The body arrives as a request body (an ArrayBuffer, as an R2 object or a
// request body would): per-invocation JS memory is the body plus the sink,
// and both are garbage once the invocation ends (the JS reserve covers the
// backlog). Nothing large is generated inside this isolate.
let input = null;
// Stand-in for the platform side of a FixedLengthStream / R2 put body: one
// exact-size JS buffer per invocation.
let sink = new Uint8Array(0);
let sinkLen = 0;
let sinkAborted = 0;
// What the dying instance's hooks hand out (durable state in a coordinator).
let intent = null;
let postmortem = null;

function imports(getExports) {
  return {
    drill: {
      read_chunk(dst, offset, max) {
        const n = Math.max(0, Math.min(max, input.length - offset));
        new Uint8Array(getExports().memory.buffer, dst, n).set(input.subarray(offset, offset + n));
        return n;
      },
      write_chunk(src, len) {
        if (sinkLen + len > sink.length) throw new Error("sink overflow");
        sink.set(new Uint8Array(getExports().memory.buffer, src, len), sinkLen);
        sinkLen += len;
      },
      abort_stream(code) {
        sinkAborted = code;
      },
      recycle_intent(route, kind, pages) {
        intent = { route, kind, pages };
      },
      postmortem(...w) {
        postmortem = w;
      },
    },
  };
}

function instantiate() {
  const inst = new WebAssembly.Instance(wasmModule, imports(() => exp));
  exp = inst.exports;
  generation++;
  memoryAtInstantiate = exp.memory.buffer.byteLength;
}

function words(o, from) {
  let s = "";
  for (let i = from; i < from + 8; i++) {
    const w = o[i];
    s += HEX[w & 255] + HEX[(w >>> 8) & 255] + HEX[(w >>> 16) & 255] + HEX[(w >>> 24) & 255];
  }
  return s;
}

function text(e) {
  const n = e.drill_text_len();
  return new TextDecoder().decode(new Uint8Array(e.memory.buffer, e.drill_text(), n));
}

export default {
  async fetch(req) {
    const url = new URL(req.url);
    if (url.pathname === "/invoke") {
      if (!exp) instantiate();
      const e = exp;
      const gen = generation;
      const n = Number(url.searchParams.get("receipts"));
      const flags = Number(url.searchParams.get("flags") ?? 0);
      const now = Number(url.searchParams.get("now") ?? 0);
      const target = Number(url.searchParams.get("target") ?? 0);
      input = new Uint8Array(await req.arrayBuffer());
      const jsNeed = input.length + n * BUNDLE_ENTRY;
      sink = new Uint8Array(flags & 64 ? 0 : n * BUNDLE_ENTRY);
      sinkLen = 0;
      sinkAborted = 0;
      intent = null;
      postmortem = null;
      const before = e.memory.buffer.byteLength;
      let status;
      let trap = null;
      try {
        status = e.drill_invoke(input.length, n, flags, now >>> 0, jsNeed >>> 0, target);
      } catch (err) {
        trap = String(err);
        // worker-build semantics: the abort hook runs in the dying instance,
        // then the next call gets a fresh instance.
        try {
          e.drill_on_abort?.();
        } catch {}
        exp = null;
      }
      const after = e.memory.buffer.byteLength;
      // A dead instance's memory is not read: its words and stats are gone.
      const o = trap ? new Uint32Array(OUT_WORDS) : new Uint32Array(e.memory.buffer, e.drill_out(), OUT_WORDS);
      if (trap) o[0] = intent ? 0x7002 : 0x7005;
      const statsLen = trap ? 0 : e.drill_stats();
      const stats = statsLen ? JSON.parse(text(e)) : null;
      const inputLen = input.length;
      const emitted = sinkLen;
      // The body and the sink become garbage now.
      input = null;
      sink = new Uint8Array(0);
      return Response.json({
        generation: gen,
        before,
        after,
        memory_at_instantiate: memoryAtInstantiate,
        trap,
        status: o[0],
        count: o[1],
        bundle_len: o[2],
        index_len: o[3],
        peak_live: o[4],
        reserved_bytes: o[5],
        peak_pages: o[6],
        closed: o[7],
        live_at_close: o[8],
        bundle_sha256: words(o, 9),
        index_root: words(o, 17),
        recycle: o[25],
        slot: o[26],
        settled_status: o[27],
        settled_quarantined: o[28],
        dropped: o[29],
        emitted,
        sink_aborted: sinkAborted,
        recycle_intent: intent,
        postmortem,
        input_len: inputLen,
        js_need: jsNeed,
        stats,
      });
    }
    if (url.pathname === "/selftest") {
      // A fresh instance: the cases assert on instance-wide counters.
      let e = null;
      const inst = new WebAssembly.Instance(wasmModule, imports(() => e));
      e = inst.exports;
      const before = e.memory.buffer.byteLength;
      let fails;
      let trap = null;
      try {
        fails = e.selftest_run();
      } catch (err) {
        trap = String(err);
      }
      return Response.json({ arena: e.drill_arena(), fails, trap, report: text(e), memory_before: before, memory_after: e.memory.buffer.byteLength });
    }
    if (url.pathname === "/model") {
      let e = null;
      const inst = new WebAssembly.Instance(wasmModule, imports(() => e));
      e = inst.exports;
      e.drill_model();
      return new Response(text(e), { headers: { "content-type": "application/json" } });
    }
    return new Response("not found", { status: 404 });
  },
};
