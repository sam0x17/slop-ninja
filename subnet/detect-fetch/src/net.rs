//! The outbound network the pipeline drives: one `GET` at a time, its head,
//! then its body streamed window by window. The Worker implements it over
//! the platform's Fetch (`cf.rs`); the native tests implement it with
//! in-process fakes ([`fake`]), so no test reaches the network.

/// A response head. Header names are lowercase.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Head {
    pub status: u16,
    pub headers: Vec<(String, String)>,
}

impl Head {
    /// The first value of `name` (lowercase).
    pub fn get(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }

    /// Every value of `name`, in order (the platform joins repeated headers
    /// with `, `; both forms are read the same way).
    pub fn all<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a str> + 'a {
        self.headers.iter().filter(move |(k, _)| k == name).map(|(_, v)| v.as_str())
    }
}

/// The most response headers read, and their total bytes: a head past
/// either is refused as a transport failure.
pub const MAX_HEADERS: usize = 256;
pub const MAX_HEADER_BYTES: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NetError {
    /// The wall-clock budget ran out.
    Timeout,
    /// Connection, TLS, protocol or stream failure.
    Transport,
}

/// One outbound request's headers.
pub type Headers = Vec<(&'static str, String)>;

pub trait Net {
    /// One `GET` of `url` with `headers`, following no redirect, settled by
    /// `deadline_ms` (absolute, [`Net::now_ms`] clock). A previous body
    /// still unread is discarded first.
    async fn get(&mut self, url: &str, headers: &Headers, deadline_ms: u64) -> Result<Head, NetError>;

    /// Streams the last response's body into `sink` a window at a time,
    /// until its end, `sink` answering `false` (the read stops and the
    /// stream is cancelled; `Ok`), or the deadline.
    async fn read_body(&mut self, deadline_ms: u64, sink: &mut dyn FnMut(&[u8]) -> bool) -> Result<(), NetError>;

    /// Cancels the last response's body unread.
    fn discard(&mut self);

    fn now_ms(&self) -> u64;

    /// Fills `out` from the platform's CSPRNG (signature nonces).
    fn random(&mut self, out: &mut [u8]);

    /// Dev builds: a host the security screen skips (the local fixture site).
    fn screen_exempt(&self, _host: &str) -> bool {
        false
    }
}

/// Reads a whole body of at most `bound` bytes; `Ok(None)` when it is
/// longer (the read stops at the bound).
pub async fn body_within<N: Net>(net: &mut N, bound: usize, deadline_ms: u64) -> Result<Option<Vec<u8>>, NetError> {
    let mut out = Vec::new();
    let mut over = false;
    net.read_body(deadline_ms, &mut |chunk| {
        if out.len() + chunk.len() > bound {
            over = true;
            return false;
        }
        out.extend_from_slice(chunk);
        true
    })
    .await?;
    Ok((!over).then_some(out))
}

/// In-process fakes for the native tests.
#[cfg(test)]
pub mod fake {
    use super::*;
    use std::collections::BTreeMap;

    /// A canned response.
    #[derive(Clone, Debug)]
    pub struct Reply {
        pub status: u16,
        pub headers: Vec<(String, String)>,
        pub body: Vec<u8>,
        /// The request times out instead.
        pub timeout: bool,
    }

    impl Reply {
        pub fn new(status: u16, headers: &[(&str, &str)], body: impl Into<Vec<u8>>) -> Reply {
            Reply {
                status,
                headers: headers.iter().map(|(k, v)| (k.to_ascii_lowercase(), (*v).to_owned())).collect(),
                body: body.into(),
                timeout: false,
            }
        }

        pub fn html(body: &str) -> Reply {
            Reply::new(200, &[("content-type", "text/html; charset=utf-8")], body)
        }

        pub fn redirect(status: u16, to: &str) -> Reply {
            Reply::new(status, &[("location", to)], "")
        }

        pub fn timeout() -> Reply {
            Reply { timeout: true, ..Reply::new(0, &[], "") }
        }
    }

    /// A site map from URL to reply; unknown URLs answer 404. Every request
    /// is recorded with its headers. The clock advances by `tick_ms` per
    /// request.
    #[derive(Default)]
    pub struct FakeNet {
        pub sites: BTreeMap<String, Reply>,
        pub requests: Vec<(String, Vec<(String, String)>)>,
        pub now: u64,
        pub tick_ms: u64,
        /// Body windows (to exercise chunk boundaries).
        pub window: usize,
        pending: Option<Vec<u8>>,
        pub exempt: Option<String>,
    }

    impl FakeNet {
        pub fn new() -> FakeNet {
            FakeNet { now: 1_000_000, window: 7, ..FakeNet::default() }
        }

        pub fn site(mut self, url: &str, r: Reply) -> FakeNet {
            self.sites.insert(url.to_owned(), r);
            self
        }

        /// The URLs requested, in order.
        pub fn urls(&self) -> Vec<&str> {
            self.requests.iter().map(|(u, _)| u.as_str()).collect()
        }
    }

    impl Net for FakeNet {
        async fn get(&mut self, url: &str, headers: &Headers, deadline_ms: u64) -> Result<Head, NetError> {
            self.pending = None;
            self.requests.push((url.to_owned(), headers.iter().map(|(k, v)| ((*k).to_owned(), v.clone())).collect()));
            self.now += self.tick_ms;
            if self.now > deadline_ms {
                return Err(NetError::Timeout);
            }
            let r = self.sites.get(url).cloned().unwrap_or_else(|| Reply::new(404, &[("content-type", "text/html")], "not here"));
            if r.timeout {
                return Err(NetError::Timeout);
            }
            self.pending = Some(r.body);
            Ok(Head { status: r.status, headers: r.headers })
        }

        async fn read_body(&mut self, deadline_ms: u64, sink: &mut dyn FnMut(&[u8]) -> bool) -> Result<(), NetError> {
            if self.now > deadline_ms {
                return Err(NetError::Timeout);
            }
            let body = self.pending.take().unwrap_or_default();
            for w in body.chunks(self.window.max(1)) {
                if !sink(w) {
                    break;
                }
            }
            Ok(())
        }

        fn discard(&mut self) {
            self.pending = None;
        }

        fn now_ms(&self) -> u64 {
            self.now
        }

        fn random(&mut self, out: &mut [u8]) {
            out.fill(7);
        }

        fn screen_exempt(&self, host: &str) -> bool {
            self.exempt.as_deref() == Some(host)
        }
    }
}
