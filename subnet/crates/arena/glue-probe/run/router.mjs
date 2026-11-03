// Routes HTTP to the probe Worker; `/queue` and `/scheduled` dispatch those
// events through the service binding (workerd `service_binding_extra_handlers`).
export default {
  async fetch(req, env) {
    const url = new URL(req.url);
    if (url.pathname === "/queue") {
      const ms = Number(url.searchParams.get("ms") ?? 400);
      const r = await env.PROBE.queue("probe", [{ id: crypto.randomUUID(), timestamp: new Date(), body: ms, attempts: 1 }]);
      return Response.json({ outcome: r.outcome, ackAll: r.ackAll, retryBatch: r.retryBatch });
    }
    if (url.pathname === "/scheduled") {
      const r = await env.PROBE.scheduled({ scheduledTime: new Date(), cron: "* * * * *" });
      return Response.json({ outcome: r.outcome });
    }
    // `/stats` goes as OPTIONS: the probe recognises it from the method
    // enum, without parsing the URL outside a scope.
    if (url.pathname === "/stats") return env.PROBE.fetch(req.url, { method: "OPTIONS" });
    return env.PROBE.fetch(req);
  },
};
