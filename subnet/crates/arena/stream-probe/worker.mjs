// A FixedLengthStream(10) whose readable side is an R2 put body (the
// streamed package write, design §4.2). Five bytes are written, then the
// writer finishes per `mode`; the put is awaited under a bound, as the host
// awaits it inside the scope with an AbortSignal timeout.
export default {
  async fetch(req, env) {
    const url = new URL(req.url);
    const mode = url.searchParams.get("mode");
    const bound = Number(url.searchParams.get("bound") ?? 3000);
    const key = `fls-${mode}`;
    const { readable, writable } = new FixedLengthStream(10);
    const put = env.B.put(key, readable);
    const writer = writable.getWriter();
    const t0 = performance.now();
    await writer.write(new Uint8Array(5).fill(1));
    if (mode === "full") {
      await writer.write(new Uint8Array(5).fill(2));
      await writer.close();
    } else if (mode === "close") {
      writer.close().catch(() => {});
    } else if (mode === "abort") {
      writer.abort(new Error("recheck mismatch")).catch(() => {});
    } else if (mode === "short") {
      writer.releaseLock();
    }
    let out;
    try {
      out = await Promise.race([
        put.then(() => "stored"),
        new Promise((r) => setTimeout(() => r(`pending-after-${bound}ms`), bound)),
      ]);
    } catch (e) {
      out = `rejected:${String(e)}`;
    }
    const ms = Math.round(performance.now() - t0);
    const head = await env.B.head(key);
    return Response.json({ mode, out, ms, stored: head !== null, size: head?.size ?? null });
  },
};
