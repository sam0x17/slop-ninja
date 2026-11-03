// Runs the arena self-tests inside a drill wasm32 module under Node (no
// workerd, no network): instantiates the module with inert imports, calls
// `selftest_run`, prints the report and checks linear memory.
// Usage: node selftest-node.mjs <drill-arena*.wasm> [expected fixed linear memory bytes]
// Exit 0 when every case passes and, with an expected size, memory equals it
// before and after (the module never grew).
import { readFileSync } from "node:fs";

const [, , wasmPath, expectArg] = process.argv;
const mod = new WebAssembly.Module(readFileSync(wasmPath));
let exp = null;
const inert = {
  drill: {
    read_chunk: () => 0,
    write_chunk: () => {},
    abort_stream: () => {},
    recycle_intent: () => {},
    postmortem: () => {},
  },
};
const memLimits = WebAssembly.Module.exports(mod).find((e) => e.name === "memory");
exp = new WebAssembly.Instance(mod, inert).exports;
const before = exp.memory.buffer.byteLength;
const fails = exp.selftest_run();
const after = exp.memory.buffer.byteLength;
const text = new TextDecoder().decode(new Uint8Array(exp.memory.buffer, exp.drill_text(), exp.drill_text_len()));
process.stdout.write(text);
console.log(JSON.stringify({ module: wasmPath, memory: memLimits ? "exported" : "none", before, after, fails }));
let rc = fails === 0 ? 0 : 1;
if (expectArg !== undefined) {
  const expect = Number(expectArg);
  if (before !== expect || after !== expect) {
    console.log(`linear memory ${before} -> ${after}, expected fixed ${expect}`);
    rc = 2;
  }
}
process.exit(rc);
