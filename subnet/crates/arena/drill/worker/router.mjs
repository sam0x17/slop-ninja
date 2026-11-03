// Routes `/<service>/<path>?<query>` to the drill service binding of that
// name (each drill service is its own isolate).
export default {
  async fetch(req, env) {
    const url = new URL(req.url);
    const parts = url.pathname.split("/");
    const svc = env[parts[1]];
    if (!svc) return new Response("no such service", { status: 404 });
    // Bodies (POST) stream through to the drill isolate unbuffered.
    return svc.fetch(`http://drill/${parts.slice(2).join("/")}${url.search}`, { method: req.method, body: req.body });
  },
};
