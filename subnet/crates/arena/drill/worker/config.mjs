// Writes a `workerd serve` config for a drill plan: one router service on a
// loopback socket plus one drill service (its own isolate) per (variant,
// scenario). The router forwards `/<service>/<path>` to that service.
// Usage: node config.mjs <plan.json> <port> > config.capnp
import { readFileSync } from "node:fs";

const plan = JSON.parse(readFileSync(process.argv[2], "utf8"));
const port = Number(process.argv[3]);
const date = plan.compatibility_date;
const ident = (s) => s.replace(/[^A-Za-z0-9]/g, "");
const services = [];
const workers = [];
const bindings = [];
for (const sc of plan.scenarios) {
  for (const variant of sc.variants ?? plan.variants) {
    const name = `drill-${variant}-${sc.name}`;
    const w = `w${ident(variant)}${ident(sc.name)}`;
    services.push(`    (name = "${name}", worker = .${w}),`);
    bindings.push(`    (name = "${name}", service = "${name}"),`);
    workers.push(`const ${w} :Workerd.Worker = (
  modules = [
    (name = "drill.mjs", esModule = embed "drill.mjs"),
    (name = "drill.wasm", wasm = embed "drill-${variant}.wasm"),
  ],
  compatibilityDate = "${date}",
);`);
  }
}
process.stdout.write(`using Workerd = import "/workerd/workerd.capnp";

const config :Workerd.Config = (
  services = [
    (name = "router", worker = .router),
${services.join("\n")}
  ],
  sockets = [(name = "http", address = "127.0.0.1:${port}", http = (), service = "router")],
);

const router :Workerd.Worker = (
  modules = [(name = "router.mjs", esModule = embed "router.mjs")],
  compatibilityDate = "${date}",
  bindings = [
${bindings.join("\n")}
  ],
);

${workers.join("\n\n")}
`);
