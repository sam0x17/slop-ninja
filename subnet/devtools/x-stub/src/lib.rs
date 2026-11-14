//! Dev only: the X stub, a Worker that plays X's free embed (oEmbed) for
//! the X post checks of local dev sessions. Never deployed.
//!
//! It serves `GET https://publish.x.com/oembed?url=https://twitter.com/i/status/<id>&omit_script=1&dnt=true`
//! on its real host and path: `<stubs origin>/publish.x.com/oembed?...` from
//! a browser or a suite, and the real URL through a service binding (the
//! fetch Worker's dev config binds it as `X_STUB`). What each post answers,
//! the switches, the two control posts and the counters are in [`oembed`].
//! One Durable Object holds the state, so the per-id call counters and the
//! high-water mark of calls in flight cover every caller.

pub mod oembed;

#[cfg(target_arch = "wasm32")]
mod cf {
    use crate::oembed::{self, Controls, Route, State};
    use slop_ninja_dev_http::Reply;
    use slop_ninja_dev_http::cf::{call_of, response_of};
    use std::cell::RefCell;
    use worker::{Context, Date, DurableObject, Env, Request, Response, Result, durable_object, event};

    const KEY: &str = "x";
    const MAX_BODY: usize = 64 * 1024;

    #[event(fetch)]
    async fn fetch(req: Request, env: Env, _ctx: Context) -> Result<Response> {
        env.durable_object("X_STUB_STATE")?.id_from_name("state")?.get_stub()?.fetch_with_request(req).await
    }

    fn controls(env: &Env) -> Controls {
        let v = |k: &str| env.var(k).ok().map(|v| v.to_string()).filter(|v| !v.trim().is_empty());
        Controls { live: v("LIVE_CONTROL"), gone: v("GONE_CONTROL") }
    }

    #[durable_object(fetch)]
    pub struct XStubState {
        state: worker::State,
        env: Env,
        mem: RefCell<Option<State>>,
    }

    impl XStubState {
        async fn load(&self) -> Result<()> {
            if self.mem.borrow().is_none() {
                let s: State = self.state.storage().get(KEY).await?.unwrap_or_default();
                let mut m = self.mem.borrow_mut();
                if m.is_none() {
                    *m = Some(s);
                }
            }
            Ok(())
        }

        /// Runs `f` on the state (no borrow is held across an await) and
        /// stores what it left.
        async fn with<T>(&self, f: impl FnOnce(&mut State) -> T) -> Result<T> {
            self.load().await?;
            let (out, snapshot) = {
                let mut m = self.mem.borrow_mut();
                let s = m.get_or_insert_with(State::default);
                let out = f(s);
                (out, s.clone())
            };
            self.state.storage().put(KEY, &snapshot).await?;
            Ok(out)
        }
    }

    impl DurableObject for XStubState {
        fn new(state: worker::State, env: Env) -> Self {
            Self { state, env, mem: RefCell::new(None) }
        }

        async fn fetch(&self, mut req: Request) -> Result<Response> {
            let (call, oversize) = call_of(&mut req, MAX_BODY).await?;
            if oversize {
                return response_of(Reply::text(413, "x-stub: body too large")).await;
            }
            let controls = controls(&self.env);
            let now = Date::now().as_millis();
            let route = oembed::route(&call);
            let counted = matches!(route, Route::Oembed(_) | Route::BadOembed(_));
            let id = match &route {
                Route::Oembed(id) => Some(id.clone()),
                _ => None,
            };
            let reply = self
                .with(|s| {
                    if counted {
                        s.begin(id.as_deref());
                    }
                    oembed::handle(s, &controls, &call, now)
                })
                .await?;
            // The delay runs outside the state, so other calls are counted
            // in flight meanwhile.
            let res = response_of(reply).await;
            if counted {
                self.with(State::end).await?;
            }
            res
        }
    }
}
