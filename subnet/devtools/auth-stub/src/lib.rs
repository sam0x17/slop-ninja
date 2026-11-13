//! Dev only: the auth stub, a Worker that plays the three OAuth providers
//! and captures sign-in mail in local dev sessions. Never deployed.
//!
//! Every provider endpoint is served on its real host and path, as the
//! account Worker's dev build calls it: `<stub origin>/<real host><real
//! path>` ([`routes::Endpoint`]).
//!
//! - **Google** (OIDC): discovery, authorize, token (an RS256 ID token from a
//!   key the stub generates once and keeps) and the key set.
//! - **GitHub**: authorize, token, and the API's `/user` and `/user/emails`.
//! - **X**: authorize, token (codes live 30 seconds), `/2/users/me`, revoke,
//!   and the avatar images, `https://pbs.twimg.com/<path>` served at
//!   `/pbs.twimg.com/<path>`.
//! - **Mail**: `POST /mail` stores one message (`to`, `subject`, `text`, and
//!   optionally `from`, `html`, `headers`); `GET /mail`, `GET /mail/latest`
//!   and `POST /mail/clear` read and clear them.
//! - **Switches**: `POST /__stub/switch` with `{"provider": ..., ...}`,
//!   `POST /__stub/reset`, `GET /__stub/state` ([`routes`]).
//!
//! The browser reaches it on the stubs session's own origin (authorize and
//! the consent page, `/__stub/consent/<provider>`); the account Worker's dev
//! build calls the token, user, revoke, avatar and mail routes through the
//! `AUTH_STUB` service binding.

pub mod routes;
pub mod users;

#[cfg(target_arch = "wasm32")]
mod cf {
    use crate::routes::{Config, Outcome, Stub, b64url, jwt_signing_input};
    use js_sys::{Array, Function, Object, Promise, Reflect, Uint8Array};
    use serde_json::{Value, json};
    use sha2::{Digest, Sha256};
    use slop_ninja_dev_http::Reply;
    use slop_ninja_dev_http::cf::{call_of, response_of};
    use wasm_bindgen::{JsCast, JsValue};
    use wasm_bindgen_futures::JsFuture;
    use worker::{Context, Date, DurableObject, Env, Request, Response, Result, State, Storage, durable_object, event};

    const STATE: &str = "stub";
    const KEY: &str = "rsa_jwk";
    const MAX_BODY: usize = 1 << 20;

    #[event(fetch)]
    async fn fetch(req: Request, env: Env, _ctx: Context) -> Result<Response> {
        let url = req.url()?;
        let query = url.query().map(|q| format!("?{q}")).unwrap_or_default();
        // The stubs session's door to its other stubs.
        if let Some(rest) = url.path().strip_prefix("/__scorer/") {
            let to = format!("https://stub-scorer.internal/{rest}{query}");
            return env.service("STUB_SCORER")?.fetch_request(Request::new_with_init(&to, &forwarded(req).await?)?).await;
        }
        if let Some(rest) = url.path().strip_prefix("/__fixture/") {
            let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
            let to = format!("https://{host}/{path}{query}");
            return env.service("FIXTURE_SITE")?.fetch_request(Request::new_with_init(&to, &forwarded(req).await?)?).await;
        }
        env.durable_object("AUTH_STUB_STATE")?.id_from_name("state")?.get_stub()?.fetch_with_request(req).await
    }

    /// The method, headers and body of `req`, for a request to another stub.
    async fn forwarded(mut req: Request) -> Result<worker::RequestInit> {
        let mut init = worker::RequestInit::new();
        init.with_method(req.method()).with_headers(req.headers().clone()).with_redirect(worker::RequestRedirect::Manual);
        if !matches!(req.method(), worker::Method::Get | worker::Method::Head) {
            init.with_body(Some(Uint8Array::from(req.bytes().await?.as_slice()).into()));
        }
        Ok(init)
    }

    fn js_err(e: JsValue) -> worker::Error {
        worker::Error::JsError(e.as_string().unwrap_or_else(|| "WebCrypto".into()))
    }

    fn subtle() -> Result<JsValue> {
        let crypto = Reflect::get(&js_sys::global(), &"crypto".into()).map_err(js_err)?;
        Reflect::get(&crypto, &"subtle".into()).map_err(js_err)
    }

    async fn call(obj: &JsValue, method: &str, args: &[JsValue]) -> Result<JsValue> {
        let f: Function = Reflect::get(obj, &method.into()).map_err(js_err)?.dyn_into().map_err(js_err)?;
        let p: Promise = f.apply(obj, &args.iter().collect::<Array>()).map_err(js_err)?.dyn_into().map_err(js_err)?;
        JsFuture::from(p).await.map_err(js_err)
    }

    fn algorithm() -> Result<JsValue> {
        let o = Object::new();
        Reflect::set(&o, &"name".into(), &"RSASSA-PKCS1-v1_5".into()).map_err(js_err)?;
        Reflect::set(&o, &"hash".into(), &"SHA-256".into()).map_err(js_err)?;
        Ok(o.into())
    }

    /// The stub's signing key as a private JWK, generated on first use and
    /// kept in the object's storage.
    async fn private_jwk(storage: &Storage) -> Result<Value> {
        if let Some(j) = storage.get::<Value>(KEY).await? {
            return Ok(j);
        }
        let subtle = subtle()?;
        let params = algorithm()?;
        Reflect::set(&params, &"modulusLength".into(), &JsValue::from_f64(2048.0)).map_err(js_err)?;
        Reflect::set(&params, &"publicExponent".into(), &Uint8Array::from(&[1u8, 0, 1][..]).into()).map_err(js_err)?;
        let usages: Array = ["sign", "verify"].iter().map(|s| JsValue::from_str(s)).collect();
        let pair = call(&subtle, "generateKey", &[params, JsValue::TRUE, usages.into()]).await?;
        let private = Reflect::get(&pair, &"privateKey".into()).map_err(js_err)?;
        let jwk = call(&subtle, "exportKey", &["jwk".into(), private]).await?;
        let text: String = js_sys::JSON::stringify(&jwk).map_err(js_err)?.into();
        let v: Value = serde_json::from_str(&text)?;
        storage.put(KEY, &v).await?;
        Ok(v)
    }

    fn kid(jwk: &Value) -> String {
        hex::encode(&Sha256::digest(jwk["n"].as_str().unwrap_or("").as_bytes())[..8])
    }

    fn public_jwk(jwk: &Value) -> Value {
        json!({"kty": "RSA", "n": jwk["n"], "e": jwk["e"], "alg": "RS256", "use": "sig", "kid": kid(jwk)})
    }

    async fn sign(jwk: &Value, input: &str) -> Result<Vec<u8>> {
        let subtle = subtle()?;
        let key_js = js_sys::JSON::parse(&jwk.to_string()).map_err(js_err)?;
        let usages: Array = [JsValue::from_str("sign")].into_iter().collect();
        let key = call(&subtle, "importKey", &["jwk".into(), key_js, algorithm()?, JsValue::FALSE, usages.into()]).await?;
        let sig = call(&subtle, "sign", &[algorithm()?, key, Uint8Array::from(input.as_bytes()).into()]).await?;
        Ok(Uint8Array::new(&sig).to_vec())
    }

    fn config(env: &Env) -> std::result::Result<Config, String> {
        Config::from_vars(|k| env.var(k).ok().map(|v| v.to_string()))
    }

    #[durable_object(fetch)]
    pub struct AuthStubState {
        state: State,
        env: Env,
    }

    impl DurableObject for AuthStubState {
        fn new(state: State, env: Env) -> Self {
            Self { state, env }
        }

        async fn fetch(&self, mut req: Request) -> Result<Response> {
            let (call, oversize) = call_of(&mut req, MAX_BODY).await?;
            if oversize {
                return response_of(Reply::json(413, &json!({"error": "body"}))).await;
            }
            let cfg = match config(&self.env) {
                Ok(c) => c,
                Err(e) => return response_of(Reply::json(500, &json!({"error": format!("config: {e}")}))).await,
            };
            let storage = self.state.storage();
            let mut stub: Stub = storage.get(STATE).await?.unwrap_or_default();
            let outcome = stub.handle(&cfg, &call, Date::now().as_millis());
            storage.put(STATE, &stub).await?;
            let reply = match outcome {
                Outcome::Reply(r) => r,
                Outcome::Jwks => {
                    let jwk = private_jwk(&storage).await?;
                    Reply::json(200, &json!({"keys": [public_jwk(&jwk)]})).with_header("cache-control", "no-store")
                }
                Outcome::IdToken { claims, mut body, delay_ms } => {
                    let jwk = private_jwk(&storage).await?;
                    let input = jwt_signing_input(&json!({"alg": "RS256", "kid": kid(&jwk), "typ": "JWT"}), &claims);
                    let sig = sign(&jwk, &input).await?;
                    body["id_token"] = json!(format!("{input}.{}", b64url(&sig)));
                    Reply::json(200, &body).with_header("cache-control", "no-store").with_delay(delay_ms)
                }
            };
            response_of(reply).await
        }
    }
}
