// workerd serve config for the glue probe. Usage:
// node config.mjs <build-dir> <port> > config.capnp
const [build, port] = process.argv.slice(2);
process.stdout.write(`using Workerd = import "/workerd/workerd.capnp";

const config :Workerd.Config = (
  services = [
    (name = "router", worker = .router),
    (name = "probe", worker = .probe),
  ],
  sockets = [(name = "http", address = "127.0.0.1:${port}", http = (), service = "router")],
);

const router :Workerd.Worker = (
  modules = [(name = "router.mjs", esModule = embed "router.mjs")],
  compatibilityDate = "2026-10-01",
  compatibilityFlags = ["service_binding_extra_handlers"],
  bindings = [(name = "PROBE", service = "probe")],
);

const probe :Workerd.Worker = (
  modules = [
    (name = "index.js", esModule = embed "${build}/index.js"),
    (name = "index_bg.wasm", wasm = embed "${build}/index_bg.wasm"),
  ],
  compatibilityDate = "2026-10-01",
  durableObjectNamespaces = [(className = "ProbeDo", uniqueKey = "slop-ninja-arena-probe-do")],
  durableObjectStorage = (inMemory = void),
  bindings = [(name = "PROBE_DO", durableObjectNamespace = "ProbeDo")],
);
`);
