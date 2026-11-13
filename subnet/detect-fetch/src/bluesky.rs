//! The Bluesky adapter: a post read through the public AppView
//! (`public.api.bsky.app`, no account), its author's self-thread joined.
//!
//! 1. A handle in the URL is resolved to its DID
//!    (`com.atproto.identity.resolveHandle`).
//! 2. `app.bsky.feed.getPostThread` for `at://<did>/app.bsky.feed.post/<rkey>`
//!    with [`THREAD_DEPTH`] levels of replies; the self-thread is the chain of
//!    earliest replies by the same author, read again from its last post when
//!    it reaches the depth, at most [`THREAD_CALLS`] calls.
//! 3. A post or author labelled `!no-unauthenticated` (the author asked that
//!    logged-out viewers not see their posts) ends the read
//!    `private_source`, keeping no text.
//!
//! Each response is read within the stripped bound and parsed into typed
//! structures; the joined text is held to the same bound.

use crate::fetch::{Ctx, bare, request_headers};
use crate::net::{Net, NetError, body_within};
use serde::Deserialize;
use slop_ninja_detect_fetch_wire::{Adapter, FetchAnswer, Meta, Outcome};
use url::Url;

/// The public AppView.
pub const APPVIEW: &str = "https://public.api.bsky.app";
/// Reply levels per thread call.
pub const THREAD_DEPTH: usize = 20;
/// Thread calls per post, at most (so a self-thread of up to 100 posts).
pub const THREAD_CALLS: usize = 5;
/// The label an author sets to hide their posts from logged-out viewers.
pub const NO_UNAUTHENTICATED: &str = "!no-unauthenticated";

#[derive(Deserialize)]
struct Resolved {
    did: String,
}

#[derive(Deserialize)]
struct ThreadResponse {
    thread: Thread,
}

#[derive(Deserialize)]
struct Thread {
    #[serde(rename = "$type", default)]
    kind: String,
    #[serde(default)]
    post: Option<Post>,
    /// Absent past the requested depth.
    #[serde(default)]
    replies: Option<Vec<Thread>>,
}

#[derive(Deserialize)]
struct Post {
    uri: String,
    author: Author,
    record: Record,
    #[serde(default)]
    labels: Vec<Label>,
}

#[derive(Deserialize)]
struct Author {
    did: String,
    #[serde(default)]
    handle: String,
    #[serde(rename = "displayName", default)]
    display_name: Option<String>,
    #[serde(default)]
    labels: Vec<Label>,
}

#[derive(Deserialize)]
struct Record {
    #[serde(default)]
    text: String,
    #[serde(rename = "createdAt", default)]
    created_at: String,
    #[serde(default)]
    langs: Vec<String>,
}

#[derive(Deserialize)]
struct Label {
    val: String,
}

const THREAD_VIEW: &str = "app.bsky.feed.defs#threadViewPost";

fn hidden(p: &Post) -> bool {
    p.labels.iter().chain(&p.author.labels).any(|l| l.val == NO_UNAUTHENTICATED)
}

/// Percent-encodes a query value (RFC 3986 unreserved kept).
fn q(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    for b in v.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

enum Got<T> {
    Ok(T),
    Stop(Outcome),
}

async fn get_json<N: Net, T: for<'de> Deserialize<'de>>(net: &mut N, ctx: &Ctx<'_>, url: &str, bound: usize, deadline_ms: u64) -> Got<T> {
    let Ok(u) = Url::parse(url) else { return Got::Stop(Outcome::FetchFailed) };
    let headers = request_headers(net, ctx, &u, "application/json");
    let head = match net.get(url, &headers, deadline_ms).await {
        Ok(h) => h,
        Err(NetError::Timeout) => return Got::Stop(Outcome::TooSlow),
        Err(NetError::Transport) => return Got::Stop(Outcome::FetchFailed),
    };
    if head.status != 200 {
        net.discard();
        // The AppView answers 400 with `NotFound` for a missing post or
        // handle.
        return Got::Stop(if matches!(head.status, 400 | 404 | 410) { Outcome::NotFound } else { Outcome::FetchFailed });
    }
    match body_within(net, bound, deadline_ms).await {
        Ok(Some(b)) => match serde_json::from_slice(&b) {
            Ok(v) => Got::Ok(v),
            Err(_) => Got::Stop(Outcome::FetchFailed),
        },
        Ok(None) => Got::Stop(Outcome::PageTooLarge),
        Err(NetError::Timeout) => Got::Stop(Outcome::TooSlow),
        Err(NetError::Transport) => Got::Stop(Outcome::FetchFailed),
    }
}

/// Reads the post `rkey` of `actor` (a handle or a DID).
pub async fn fetch<N: Net>(net: &mut N, ctx: &Ctx<'_>, url: &Url, actor: &str, rkey: &str, deadline_ms: u64, bound: usize) -> FetchAnswer {
    let stop = |o: Outcome| FetchAnswer { final_url: Some(url.to_string()), ..bare(o, Adapter::Bluesky) };
    let did = if actor.starts_with("did:") {
        actor.to_owned()
    } else {
        let u = format!("{APPVIEW}/xrpc/com.atproto.identity.resolveHandle?handle={}", q(actor));
        match get_json::<N, Resolved>(net, ctx, &u, bound, deadline_ms).await {
            Got::Ok(r) if r.did.starts_with("did:") => r.did,
            Got::Ok(_) => return stop(Outcome::FetchFailed),
            Got::Stop(o) => return stop(o),
        }
    };
    let mut uri = format!("at://{did}/app.bsky.feed.post/{rkey}");
    let mut texts: Vec<String> = Vec::new();
    let mut total = 0usize;
    let mut meta = Meta { site_name: Some("Bluesky".to_owned()), did: Some(did.clone()), ..Meta::default() };
    for call in 0..THREAD_CALLS {
        let u = format!("{APPVIEW}/xrpc/app.bsky.feed.getPostThread?uri={}&depth={THREAD_DEPTH}&parentHeight=0", q(&uri));
        let resp = match get_json::<N, ThreadResponse>(net, ctx, &u, bound, deadline_ms).await {
            Got::Ok(r) => r,
            Got::Stop(o) if call == 0 => return stop(o),
            // A later call failing ends the thread where it is.
            Got::Stop(_) => break,
        };
        let mut node = &resp.thread;
        if node.kind != THREAD_VIEW {
            if call == 0 {
                return stop(if node.kind.ends_with("#blockedPost") { Outcome::FetchFailed } else { Outcome::NotFound });
            }
            break;
        }
        let mut level = 0usize;
        let mut reached_depth = false;
        while let Some(post) = node.post.as_ref() {
            if hidden(post) {
                return stop(Outcome::PrivateSource);
            }
            // Later calls start at the last post already read.
            if !(call > 0 && level == 0) {
                if texts.is_empty() {
                    meta.author =
                        post.author.display_name.clone().filter(|n| !n.trim().is_empty()).or_else(|| Some(post.author.handle.clone()));
                    meta.published = Some(post.record.created_at.clone()).filter(|c| !c.is_empty());
                    meta.lang = post.record.langs.first().cloned();
                }
                let t = post.record.text.trim();
                if !t.is_empty() {
                    if total + t.len() + 2 > bound {
                        if texts.is_empty() {
                            return stop(Outcome::PageTooLarge);
                        }
                        break;
                    }
                    total += t.len() + 2;
                    texts.push(t.to_owned());
                }
            }
            uri = post.uri.clone();
            let Some(replies) = node.replies.as_ref() else {
                reached_depth = level >= THREAD_DEPTH;
                break;
            };
            let next =
                replies.iter().filter(|r| r.kind == THREAD_VIEW).filter(|r| r.post.as_ref().is_some_and(|p| p.author.did == did)).min_by(
                    |a, b| {
                        let ca = a.post.as_ref().map(|p| p.record.created_at.as_str()).unwrap_or("");
                        let cb = b.post.as_ref().map(|p| p.record.created_at.as_str()).unwrap_or("");
                        ca.cmp(cb)
                    },
                );
            match next {
                Some(n) => {
                    node = n;
                    level += 1;
                }
                None => break,
            }
        }
        if !reached_depth {
            break;
        }
    }
    if texts.is_empty() {
        return stop(Outcome::ExtractFailed);
    }
    let text = texts.iter().map(|t| crate::extract::paragraphs(t)).filter(|t| !t.is_empty()).collect::<Vec<_>>().join("\n\n");
    FetchAnswer { text: Some(text), meta: Some(meta), stripped_bytes: total as u64, ..stop(Outcome::Ok) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block_on;
    use crate::net::fake::{FakeNet, Reply};
    use slop_ninja_detect_fetch_wire::{FetchRequest, Limits};
    use slop_ninja_detect_url::Policy;

    const DID: &str = "did:plc:z72i7hdynmk6r22z27h6tvur";

    fn post(rkey: &str, did: &str, text: &str, created: &str, labels: &str, replies: Option<&str>) -> String {
        let replies = replies.map(|r| format!(",\"replies\":[{r}]")).unwrap_or_default();
        format!(
            r#"{{"$type":"app.bsky.feed.defs#threadViewPost","post":{{"uri":"at://{did}/app.bsky.feed.post/{rkey}","cid":"c","author":{{"did":"{did}","handle":"writer.example.com","displayName":"A Writer","labels":[{labels}]}},"record":{{"$type":"app.bsky.feed.post","text":"{text}","createdAt":"{created}","langs":["en"]}},"labels":[],"indexedAt":"{created}"}}{replies}}}"#
        )
    }

    fn thread_url(rkey: &str) -> String {
        format!(
            "{APPVIEW}/xrpc/app.bsky.feed.getPostThread?uri={}&depth={THREAD_DEPTH}&parentHeight=0",
            q(&format!("at://{DID}/app.bsky.feed.post/{rkey}"))
        )
    }

    fn run(net: &mut FakeNet, url: &str) -> FetchAnswer {
        let policy = Policy::production();
        let ctx = Ctx { policy: &policy, signer: None };
        let req = FetchRequest {
            url: url.into(),
            slot: "s".into(),
            limits: Limits { html_bound: 1 << 20, wait_ms: 30_000, hops: 20 },
            conditional: None,
        };
        block_on(crate::fetch::fetch(net, &ctx, &req, &mut |_, _| {}))
    }

    #[test]
    fn joins_the_self_thread() {
        let other = post("3zzz", "did:plc:otheruserotheruserother", "Nice thread!", "2026-01-01T00:00:30Z", "", Some(""));
        let third = post("3ccc", DID, "Third part.", "2026-01-01T00:02:00Z", "", Some(""));
        let second = post("3bbb", DID, "Second part, with  spaces.", "2026-01-01T00:01:00Z", "", Some(&third));
        let late = post("3bbz", DID, "A later reply by the author.", "2026-01-01T09:00:00Z", "", Some(""));
        let root = post("3aaa", DID, "First part.", "2026-01-01T00:00:00Z", "", Some(&format!("{other},{late},{second}")));
        let mut net = FakeNet::new()
            .site(
                &format!("{APPVIEW}/xrpc/com.atproto.identity.resolveHandle?handle=writer.example.com"),
                Reply::new(200, &[("content-type", "application/json")], format!(r#"{{"did":"{DID}"}}"#)),
            )
            .site(&thread_url("3aaa"), Reply::new(200, &[("content-type", "application/json")], format!(r#"{{"thread":{root}}}"#)));
        let a = run(&mut net, "https://bsky.app/profile/writer.example.com/post/3aaa");
        assert_eq!(a.outcome, Outcome::Ok);
        assert_eq!(a.adapter, Adapter::Bluesky);
        assert_eq!(a.text.as_deref(), Some("First part.\n\nSecond part, with spaces.\n\nThird part."));
        let m = a.meta.unwrap();
        assert_eq!(m.did.as_deref(), Some(DID));
        assert_eq!(m.author.as_deref(), Some("A Writer"));
        assert_eq!(m.published.as_deref(), Some("2026-01-01T00:00:00Z"));
        assert_eq!(m.lang.as_deref(), Some("en"));
    }

    #[test]
    fn honours_no_unauthenticated() {
        let root = post("3aaa", DID, "Hidden.", "2026-01-01T00:00:00Z", r#"{"val":"!no-unauthenticated","src":"x"}"#, Some(""));
        let mut net = FakeNet::new().site(&thread_url("3aaa"), Reply::new(200, &[], format!(r#"{{"thread":{root}}}"#)));
        let a = run(&mut net, &format!("https://bsky.app/profile/{DID}/post/3aaa"));
        assert_eq!(a.outcome, Outcome::PrivateSource);
        assert!(a.text.is_none());
    }

    #[test]
    fn missing_posts() {
        let mut net = FakeNet::new().site(&thread_url("3aaa"), Reply::new(400, &[], r#"{"error":"NotFound","message":"Post not found"}"#));
        assert_eq!(run(&mut net, &format!("https://bsky.app/profile/{DID}/post/3aaa")).outcome, Outcome::NotFound);
        let mut net = FakeNet::new().site(
            &thread_url("3aaa"),
            Reply::new(200, &[], r#"{"thread":{"$type":"app.bsky.feed.defs#notFoundPost","uri":"x","notFound":true}}"#),
        );
        assert_eq!(run(&mut net, &format!("https://bsky.app/profile/{DID}/post/3aaa")).outcome, Outcome::NotFound);
    }
}
