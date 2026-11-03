// Local qualification of the abort path against a miniflare-backed R2
// binding: miniflare runs its bundled workerd on 127.0.0.1 with an in-memory
// bucket (no Cloudflare or R2 calls, `cf: false` so nothing is fetched).
// Usage: node drive.mjs <miniflare index.js> <worker.mjs>
const [MF, WORKER] = process.argv.slice(2);
const { Miniflare, convertV4MiniflareOptions } = await import(MF);
const { readFileSync } = await import("node:fs");
const script = readFileSync(WORKER, "utf8");
const mf = new Miniflare(convertV4MiniflareOptions({ cf: false, modules: true, script, compatibilityDate: "2026-10-01", r2Buckets: ["B"] }));
try {
  for (const mode of ["full", "close", "abort", "short"]) {
    const r = await mf.dispatchFetch(`http://localhost/?mode=${mode}&bound=3000`);
    console.log("FLS " + JSON.stringify(await r.json()));
  }
} finally {
  await mf.dispose();
}
