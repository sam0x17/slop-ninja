//! The stub's endpoint table against the account Worker's own provider
//! constants (`slop-ninja-auth-core` `providers`): every URL the sign-in code
//! calls or sends the browser to is a row of the table, and the dev build's
//! moved form (`Spec::endpoints_at`) is exactly the stub's path under the
//! stub origin. A provider URL changed on one side only fails here.

use slop_ninja_auth_core::providers::{self, GITHUB, GOOGLE, X};
use slop_ninja_detect_page::Origin;
use slop_ninja_dev_auth_stub::routes::Endpoint;
use slop_ninja_dev_http::session::STUB_ORIGIN;

fn bare(u: &str) -> &str {
    u.split_once('?').map_or(u, |(b, _)| b)
}

fn row(u: &str) -> Option<Endpoint> {
    Endpoint::ALL.into_iter().find(|e| e.url() == bare(u))
}

#[test]
fn every_provider_url_is_a_stub_endpoint() {
    let stub = Origin::parse(STUB_ORIGIN).expect("the stub origin parses");
    for spec in [&GOOGLE, &GITHUB, &X] {
        let real = spec.endpoints();
        let moved = spec.endpoints_at(&stub);
        let pairs = [
            (Some(real.authorize.as_ref()), Some(moved.authorize.as_ref())),
            (Some(real.token.as_ref()), Some(moved.token.as_ref())),
            (real.user.as_deref(), moved.user.as_deref()),
            (real.emails.as_deref(), moved.emails.as_deref()),
        ];
        for (r, m) in pairs {
            let (Some(r), Some(m)) = (r, m) else {
                assert_eq!(r.is_none(), m.is_none(), "{:?}: real and moved agree", spec.provider);
                continue;
            };
            let e = row(r).unwrap_or_else(|| panic!("{:?}: {r} is not in the stub's table", spec.provider));
            let query = r.split_once('?').map(|(_, q)| format!("?{q}")).unwrap_or_default();
            assert_eq!(m, format!("{STUB_ORIGIN}{}{query}", e.path()), "{:?}: the moved form is the stub's path", spec.provider);
        }
    }
    assert_eq!(row(providers::GOOGLE_DISCOVERY), Some(Endpoint::GoogleDiscovery));
    assert_eq!(providers::X_AVATAR_HOST, "pbs.twimg.com", "the stub serves X avatars at /pbs.twimg.com/<path>");
    assert!(!providers::USER_AGENT.is_empty(), "the stub refuses a GitHub call without a User-Agent");
}
