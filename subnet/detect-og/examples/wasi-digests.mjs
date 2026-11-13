// Runs the `digest` example (built for wasm32-wasip1) on every golden case
// under Node's WASI and compares the digests with tests/golden/digests.txt.
// Usage: node examples/wasi-digests.mjs <digest.wasm>
import { readFileSync, readdirSync, openSync, closeSync, writeFileSync, mkdtempSync, rmSync } from 'node:fs';
import { join, dirname } from 'node:path';
import { tmpdir } from 'node:os';
import { fileURLToPath } from 'node:url';
import { WASI } from 'node:wasi';

const here = join(dirname(fileURLToPath(import.meta.url)), '..');
const cards = join(here, '../crates/detect-page/tests/fixtures/cards');
const want = new Map(readFileSync(join(here, 'tests/golden/digests.txt'), 'utf8').trim().split('\n').map((l) => l.split('  ').reverse()));
const mod = await WebAssembly.compile(readFileSync(process.argv[2]));
const tmp = mkdtempSync(join(tmpdir(), 'og-digest-'));
let bad = 0;
for (const [name, digest] of want) {
  const [card, ends] = name.split('.');
  const inFd = openSync(join(cards, `${card}.json`), 'r');
  const outPath = join(tmp, 'out');
  writeFileSync(outPath, '');
  const outFd = openSync(outPath, 'w');
  const wasi = new WASI({ version: 'preview1', args: ['digest', ends ?? 'icons'], env: {}, stdin: inFd, stdout: outFd, returnOnExit: true });
  const inst = await WebAssembly.instantiate(mod, wasi.getImportObject());
  const code = wasi.start(inst);
  closeSync(inFd);
  closeSync(outFd);
  const got = readFileSync(outPath, 'utf8').trim();
  const ok = code === 0 && got === digest;
  if (!ok) bad += 1;
  console.log(`${ok ? 'ok  ' : 'DIFF'} ${name} ${got}`);
}
rmSync(tmp, { recursive: true });
console.log(bad === 0 ? `wasm32 digests: all ${want.size} match` : `wasm32 digests: ${bad} of ${want.size} differ`);
process.exit(bad === 0 ? 0 : 1);
