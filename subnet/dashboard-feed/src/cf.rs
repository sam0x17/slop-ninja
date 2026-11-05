//! The Workers entry point (wasm32 only).

use worker::{Context, Env, Headers, Request, Response, Result, event};

use crate::{BUCKET, Route, Stored, headers, route, status};

fn empty(code: u16) -> Result<Response> {
    Ok(Response::empty()?.with_status(code))
}

#[event(fetch)]
async fn main(req: Request, env: Env, _ctx: Context) -> Result<Response> {
    let url = req.url()?;
    let (doc, head) = match route(req.method().as_ref(), url.path(), url.query().is_some()) {
        Route::Object { doc, head } => (doc, head),
        Route::NotFound => return empty(404),
        Route::MethodNotAllowed => {
            let h = Headers::new();
            h.set("allow", "GET, HEAD")?;
            return Ok(Response::empty()?.with_status(405).with_headers(h));
        }
    };
    let Ok(bucket) = env.bucket(BUCKET) else {
        return empty(503);
    };
    let key = doc.key();
    let object = if head {
        bucket.head(key).await
    } else {
        bucket.get(key).execute().await
    };
    let object = match object {
        Ok(Some(o)) => o,
        Ok(None) => return empty(status(&doc, Stored::Missing)),
        Err(_) => return empty(status(&doc, Stored::Error)),
    };
    let code = status(&doc, Stored::Present { size: object.size() });
    if code != 200 {
        return empty(code);
    }
    let h = Headers::new();
    for (name, value) in headers(&doc) {
        h.set(name, &value)?;
    }
    if head {
        return Ok(Response::empty()?.with_headers(h));
    }
    // Checked above, before any byte is read: the body streams through the
    // runtime, never through Wasm memory.
    match object.body() {
        Some(body) => Ok(Response::from_body(body.response_body()?)?.with_headers(h)),
        None => empty(503),
    }
}
