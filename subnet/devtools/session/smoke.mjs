// Smoke check of a running stubs session (dev only): node smoke.mjs <stubs origin>
//
// Drives each stub over the session's HTTPS origin: the three OAuth code
// flows with PKCE (Google's ID token checked against the stub's key set), the
// mail capture, an X slow-exchange switch, the stub scorer through its door
// (marker answers and the load protocol) and the fixture site (robots,
// redirects, a streamed large page and the request log). It calls nothing
// outside the session. Exit status 0 when every check passes.

import { createHash, randomBytes, webcrypto } from "node:crypto";

const BASE = (process.argv[2] || "https://auth.slop.localhost:8789").replace(/\/$/, "");
const SITE = "https://slop.localhost:8787";
let failed = 0;
const check = (name, ok, detail = "") => {
  console.log(`${ok ? "PASS" : "FAIL"} ${name}${ok || !detail ? "" : `: ${detail}`}`);
  if (!ok) failed++;
};
const b64url = (b) => Buffer.from(b).toString("base64url");
const get = (path, init = {}) => fetch(BASE + path, { redirect: "manual", ...init });
const post = (path, body, headers = {}) => get(path, { method: "POST", body, headers });
const postJson = (path, v) => post(path, JSON.stringify(v), { "content-type": "application/json" });
const form = (o) => new URLSearchParams(o).toString();
const FORM = { "content-type": "application/x-www-form-urlencoded" };

const ids = {
  google: "dev-google-client-id",
  github: "dev-github-client-id",
  x: "dev-x-client-id",
};
const secret = (p) => `dev-${p}-client-secret-not-real`;
const authorizePath = { google: "/google/o/oauth2/v2/auth", github: "/github/login/oauth/authorize", x: "/x/i/oauth2/authorize" };
const scope = { google: "openid email profile", github: "", x: "users.read tweet.read users.email" };

async function codeFor(p, verifier, extra = {}) {
  const q = {
    client_id: ids[p], redirect_uri: `${SITE}/auth/${p}/callback`, response_type: "code", state: "smoke-state",
    code_challenge: b64url(createHash("sha256").update(verifier).digest()), code_challenge_method: "S256",
    scope: scope[p], ...(p === "google" ? { nonce: "smoke-nonce" } : {}), ...extra,
  };
  const r = await get(`${authorizePath[p]}?${form(q)}`);
  const loc = new URL(r.headers.get("location") || "about:blank");
  check(`${p} authorize redirects to the registered callback`, r.status === 302 && loc.origin + loc.pathname === `${SITE}/auth/${p}/callback` && loc.searchParams.get("state") === "smoke-state");
  return loc.searchParams.get("code");
}

async function google() {
  const verifier = b64url(randomBytes(32));
  const code = await codeFor("google", verifier);
  const disc = await (await get("/google/.well-known/openid-configuration")).json();
  check("google discovery names the stub token endpoint", disc.token_endpoint === `${BASE}/google/token`);
  const t = await post("/google/token", form({ code, client_id: ids.google, client_secret: secret("google"), redirect_uri: `${SITE}/auth/google/callback`, grant_type: "authorization_code", code_verifier: verifier }), FORM);
  const body = await t.json();
  check("google token answers an ID token", t.status === 200 && typeof body.id_token === "string", JSON.stringify(body));
  const [h, c, s] = body.id_token.split(".");
  const claims = JSON.parse(Buffer.from(c, "base64url"));
  check("google claims", claims.iss === "https://accounts.google.com" && claims.aud === ids.google && claims.nonce === "smoke-nonce" && claims.email === "alice@example.com" && claims.email_verified === true);
  const jwks = await (await get("/google/oauth2/v3/certs")).json();
  const header = JSON.parse(Buffer.from(h, "base64url"));
  const jwk = jwks.keys.find((k) => k.kid === header.kid);
  const key = await webcrypto.subtle.importKey("jwk", jwk, { name: "RSASSA-PKCS1-v1_5", hash: "SHA-256" }, false, ["verify"]);
  const ok = await webcrypto.subtle.verify("RSASSA-PKCS1-v1_5", key, Buffer.from(s, "base64url"), Buffer.from(`${h}.${c}`));
  check("google ID token verifies under the stub's key set (RS256)", header.alg === "RS256" && ok);
  const again = await post("/google/token", form({ code, client_id: ids.google, client_secret: secret("google"), redirect_uri: `${SITE}/auth/google/callback`, grant_type: "authorization_code", code_verifier: verifier }), FORM);
  check("google code is single use", again.status === 400);
}

async function github() {
  const verifier = b64url(randomBytes(32));
  const code = await codeFor("github", verifier);
  const t = await post("/github/login/oauth/access_token", form({ client_id: ids.github, client_secret: secret("github"), code, redirect_uri: `${SITE}/auth/github/callback`, code_verifier: verifier }), { ...FORM, accept: "application/json" });
  const tok = (await t.json()).access_token;
  const auth = { authorization: `Bearer ${tok}`, "user-agent": "slop-ninja-smoke" };
  const user = await (await get("/github/api/user", { headers: auth })).json();
  const emails = await (await get("/github/api/user/emails", { headers: auth })).json();
  check("github user and primary verified email", user.id === 9000001 && emails.some((e) => e.primary && e.verified && e.email === "alice@example.com"));
  const bad = await get("/github/api/user", { headers: { authorization: "Bearer ghu_not_issued", "user-agent": "slop-ninja-smoke" } });
  check("github API refuses a token it did not issue", bad.status === 401);
}

async function x() {
  const verifier = b64url(randomBytes(32));
  const basic = `Basic ${Buffer.from(`${ids.x}:${secret("x")}`).toString("base64")}`;
  const exchange = async (code) => post("/x/2/oauth2/token", form({ code, grant_type: "authorization_code", redirect_uri: `${SITE}/auth/x/callback`, code_verifier: verifier }), { ...FORM, authorization: basic });
  const tok = (await (await exchange(await codeFor("x", verifier))).json()).access_token;
  const me = await (await get("/x/2/users/me?user.fields=confirmed_email,profile_image_url,username,name,verified_type", { headers: { authorization: `Bearer ${tok}` } })).json();
  check("x users/me with the asked fields", me.data?.id === "1900000000000000001" && me.data?.confirmed_email === "alice@example.com" && me.data?.profile_image_url?.startsWith("https://pbs.twimg.com/"));
  const path = new URL(me.data.profile_image_url).pathname;
  const av = Buffer.from(await (await get(`/x/pbs${path}`)).arrayBuffer());
  check("x avatar is a PNG", av.subarray(0, 8).equals(Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a])));
  await postJson("/__stub/switch", { provider: "x", token_delay_ms: 1500 });
  const t0 = Date.now();
  const slow = await exchange(await codeFor("x", verifier));
  check("x slow exchange switch delays the token answer", slow.status === 200 && Date.now() - t0 >= 1400);
  await post("/__stub/reset", "");
}

async function mail() {
  await post("/mail/clear", "");
  const r = await postJson("/mail", { to: "bob@example.com", from: "SlopNinja <sender@example.com>", subject: "Your SlopNinja sign-in link", text: `Sign in to SlopNinja: ${SITE}/auth/email/confirm#t=smoke123 The link works once, for 15 minutes.` });
  const m = await (await get("/mail/latest?to=bob%40example.com")).json();
  check("mail capture stores and returns the link", r.status === 201 && m.links?.[0] === `${SITE}/auth/email/confirm#t=smoke123`);
}

async function scorer() {
  await post("/__scorer/__stub/reset", "");
  const h = await (await get("/__scorer/healthz")).json();
  check("scorer healthz: empty, one session, two threads", h.state === "empty" && h.build.sessions === 1 && h.build.threads === 2 && h.build.limits.specs.length >= 3);
  // The load protocol with a one-file manifest.
  const manifest = Buffer.from(JSON.stringify({ schema: "s", profile_id: "p", contract: "c", graph: "f0", files: [{ name: "f0", role: "r", bytes: 5, sha256: "00".repeat(32), bucket: "admission", key: "k/0", etag: "e" }] }));
  const pkg = createHash("sha256").update(manifest).digest("hex");
  const spec = h.build.limits.specs.at(-1).sha256;
  const prep = await post(`/__scorer/v2/prepare?package=${pkg}&spec=${spec}`, manifest);
  const pj = await prep.json();
  const chunk = await get("/__scorer/v2/files/0/0", { method: "PUT", body: Buffer.from("12345") });
  const load = await post(`/__scorer/v2/load?package=${pkg}&spec=${spec}`, "");
  const ready = await (await get("/__scorer/healthz")).json();
  check("scorer load protocol: prepare, chunk, load, ready", prep.status === 201 && JSON.stringify(pj.missing) === "[[0,0]]" && chunk.status === 201 && load.status === 202 && ready.state === "ready" && ready.package === pkg);
  const text = `sn-stub:band=strong ${"The council met to talk about the bridge. ".repeat(8)}`;
  const s = await post("/__scorer/v1/score", JSON.stringify({ package: pkg, spans: true, docs: [{ id: "d", text }] }), { "content-type": "application/json" });
  const r = (await s.json()).results?.[0];
  const span = r?.prediction?.spans?.[0];
  check("scorer marker answer: strong band, one passage with exact offsets", s.status === 200 && r.prediction.ai_confidence_percent === 95 && span && Buffer.byteLength(text.slice(0, span.start_char)) === span.start_byte);
  const bound = await post("/__scorer/v1/score", JSON.stringify({ package: "ab".repeat(32), docs: [{ id: "d", text: "x" }] }), { "content-type": "application/json" });
  check("scorer refuses another package", bound.status === 409);
}

async function fixtures() {
  await post("/__fixture/plain.fixtures.test/__fixture/log/clear", "");
  const robots = await (await get("/__fixture/robots-disallow.fixtures.test/robots.txt")).text();
  check("fixture robots variant", robots === "User-agent: *\nDisallow: /\n");
  const hop = await get("/__fixture/redirects.fixtures.test/hop/2");
  check("fixture redirect chain (manual)", hop.status === 302 && hop.headers.get("location") === "/hop/1");
  const big = Buffer.from(await (await get("/__fixture/large.fixtures.test/bytes/2621440")).arrayBuffer());
  check("fixture large page streams exactly", big.length === 2621440);
  const log = await (await get("/__fixture/plain.fixtures.test/__fixture/log", { headers: { "user-agent": "SlopNinjaBot/1.0" } })).json();
  check("fixture request log", log.entries?.length >= 3 && log.entries.some((e) => e.path === "/robots.txt"));
}

for (const [name, f] of Object.entries({ google, github, x, mail, scorer, fixtures })) {
  try {
    await f();
  } catch (e) {
    check(`${name} ran`, false, String(e?.stack || e));
  }
}
console.log(failed ? `${failed} check(s) failed` : "all checks passed");
process.exit(failed ? 1 : 0);
