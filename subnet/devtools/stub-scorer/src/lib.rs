//! Dev only: the stub scorer, a Worker that stands in for the scorer
//! Container in local dev sessions. The detect Worker's dev build reaches it
//! through a service binding (`STUB_SCORER`) in place of the Container's
//! port and drives the same load protocol ([`instance`]); every answer comes
//! from markers in the scored text ([`markers`]), with the real scorer's
//! reply shape and token count ([`result`]). It holds one instance in a
//! Durable Object, as the real lane holds one Container. Never deployed.

pub mod instance;
pub mod markers;
pub mod result;

#[cfg(target_arch = "wasm32")]
mod cf {
    use crate::instance::{Instance, MAX_SCORE_BODY};
    use futures_util::StreamExt;
    use slop_ninja_dev_http::cf::{call_of, response_of};
    use worker::{Context, Date, DurableObject, Env, Method, Request, Response, Result, State, durable_object, event};

    const KEY: &str = "instance";

    #[event(fetch)]
    async fn fetch(req: Request, env: Env, _ctx: Context) -> Result<Response> {
        env.durable_object("STUB_SCORER_INSTANCE")?.id_from_name("instance")?.get_stub()?.fetch_with_request(req).await
    }

    #[durable_object(fetch)]
    pub struct StubScorerInstance {
        state: State,
    }

    impl DurableObject for StubScorerInstance {
        fn new(state: State, _env: Env) -> Self {
            Self { state }
        }

        async fn fetch(&self, mut req: Request) -> Result<Response> {
            let path = req.path();
            // A chunk is counted as it streams, never kept.
            let chunk_len = if req.method() == Method::Put && path.starts_with("/v2/files/") {
                let mut n = 0u64;
                let mut s = req.stream()?;
                while let Some(piece) = s.next().await {
                    n += piece?.len() as u64;
                }
                Some(n)
            } else {
                None
            };
            let (call, oversize) = if chunk_len.is_some() { Default::default() } else { call_of(&mut req, MAX_SCORE_BODY).await? };
            let now = Date::now().as_millis();
            let storage = self.state.storage();
            let mut inst: Instance = storage.get(KEY).await?.unwrap_or_else(|| Instance::new(now));
            let reply = match chunk_len {
                Some(n) => inst.chunk(&path, n, now),
                None => inst.handle(&call, now, oversize),
            };
            storage.put(KEY, &inst).await?;
            response_of(reply).await
        }
    }
}
