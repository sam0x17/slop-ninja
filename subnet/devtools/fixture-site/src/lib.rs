//! Dev only: the fixture site, a Worker of synthetic pages for the URL
//! check. The fetch Worker's dev build reaches it through the
//! `FIXTURE_SITE` service binding for any host under [`pages::DOMAIN`]
//! (the request keeps its URL, so the host picks the scenario). Every
//! request is logged in a Durable Object (`GET /__fixture/log` on any
//! fixture host; `POST /__fixture/log/clear`), so tests can see what the
//! fetch Worker asked for and with which headers. Never deployed.

pub mod log;
pub mod pages;

#[cfg(target_arch = "wasm32")]
mod cf {
    use crate::log::{Entry, Log};
    use crate::pages;
    use slop_ninja_dev_http::cf::{call_of, response_of};
    use slop_ninja_dev_http::{Call, Reply};
    use worker::{Context, Date, DurableObject, Env, Request, Response, Result, State, durable_object, event};

    const KEY: &str = "log";

    #[event(fetch)]
    async fn fetch(mut req: Request, env: Env, _ctx: Context) -> Result<Response> {
        let (call, _) = call_of(&mut req, 1 << 20).await?;
        let stub = env.durable_object("FIXTURE_LOG")?.id_from_name("log")?.get_stub()?;
        let entry = Entry::of(&call, Date::now().as_millis());
        let mut init = worker::RequestInit::new();
        init.with_method(worker::Method::Post).with_body(Some(serde_json::to_string(&entry)?.into()));
        let path = match call.path.as_str() {
            "/__fixture/log" if call.method == "GET" => "read",
            "/__fixture/log/clear" if call.method == "POST" => "clear",
            _ => "append",
        };
        let mut answer = stub.fetch_with_request(Request::new_with_init(&format!("https://log/{path}"), &init)?).await?;
        if path != "append" {
            return Ok(answer);
        }
        let _ = answer.text().await;
        response_of(pages::page(&call)).await
    }

    #[durable_object(fetch)]
    pub struct FixtureLog {
        state: State,
    }

    impl DurableObject for FixtureLog {
        fn new(state: State, _env: Env) -> Self {
            Self { state }
        }

        async fn fetch(&self, mut req: Request) -> Result<Response> {
            let (call, _): (Call, bool) = call_of(&mut req, 1 << 20).await?;
            let storage = self.state.storage();
            let mut log: Log = storage.get(KEY).await?.unwrap_or_default();
            let reply = match call.path.as_str() {
                "/append" => {
                    if let Ok(e) = serde_json::from_slice::<Entry>(&call.body) {
                        log.push(e);
                    }
                    Reply::new(204)
                }
                "/clear" => {
                    log = Log::default();
                    Reply::json(200, &serde_json::json!({"cleared": true}))
                }
                _ => Reply::json(200, &log),
            };
            storage.put(KEY, &log).await?;
            response_of(reply).await
        }
    }
}
