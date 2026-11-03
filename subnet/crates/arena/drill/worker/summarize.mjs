// Summarises a drill log (SELFTEST, MODEL, RESULT, JSHEAP, CONSISTENCY and
// EXPECT-FAIL lines from drive.mjs) as JSON.
// Usage: node summarize.mjs <log>
import { readFileSync } from "node:fs";

const MiB = 1048576;
const lines = readFileSync(process.argv[2], "utf8").split("\n");
const grab = (tag) => lines.filter((l) => l.startsWith(`${tag} `)).map((l) => JSON.parse(l.slice(tag.length + 1)));
const mib = (b) => (b === null || b === undefined ? null : Math.round((b / MiB) * 100) / 100);
const selftest = grab("SELFTEST")[0] ?? null;
const model = grab("MODEL")[0] ?? null;
const consistency = grab("CONSISTENCY")[0] ?? null;
const expectFails = grab("EXPECT-FAIL");
const heaps = grab("JSHEAP");

const rows = grab("RESULT").map((r) => {
  const h = heaps.find((x) => x.scenario === r.scenario && x.variant === r.variant);
  const S = (h?.samples ?? []).filter((s) => "totalSize" in s);
  const js = S.map((s) => s.totalSize + s.backingStorageSize + (s.embedderHeapUsedSize ?? 0));
  const row = {
    scenario: r.scenario,
    variant: r.variant,
    shapes: r.shapes,
    invocations: r.invocations,
    mem_after_1_mib: mib(r.memory_after[1]),
    mem_after_2_mib: mib(r.memory_after[2]),
    mem_after_last_mib: mib(r.memory_after[r.invocations]),
    mem_max_mib: mib(r.memory_max),
    ceiling_mib: mib(r.ceiling),
    within_ceiling: r.within_ceiling,
    growth_2_to_last: r.growth_after_2,
    generations: r.generations,
    recycles: r.recycles,
    durable: r.durable,
    aborted_by_reinit: r.aborted_by_reinit,
    pending_max: r.pending_max,
    statuses: r.statuses,
    js_high_water_mib: js.length ? mib(Math.max(...js)) : null,
    js_need_max_mib: S.length ? mib(Math.max(...S.map((s) => s.js_need))) : null,
  };
  if (r.arena_max) {
    row.arena_max = r.arena_max;
    row.arena_sum = r.arena_sum;
    row.violations = r.violations;
  }
  return row;
});

process.stdout.write(
  JSON.stringify(
    {
      selftest: selftest && { fails: selftest.fails, trap: selftest.trap, summary: selftest.report?.trim().split("\n").pop() },
      model,
      consistency,
      expect_failures: expectFails,
      rows,
    },
    null,
    2,
  ) + "\n",
);
