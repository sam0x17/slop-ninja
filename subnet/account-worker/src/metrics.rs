//! Content-free Analytics Engine points (dataset `account`).
//!
//! No point carries a token, an address, a user id, a handle or a slug:
//! only the method, the step, a refusal code, a closed result code or an
//! alert rule.

use slop_ninja_detect_page::RefusalCode;
use slop_ninja_detect_page::ae::account as ae;

use crate::host::Host;

/// `auth {method, step, reason}`.
pub fn auth<H: Host>(host: &H, method: &str, step: &str, reason: Option<RefusalCode>) {
    host.point(&ae::AUTH, &[method, step, reason.map_or("", RefusalCode::as_str)]);
}

/// `mail {result_code}`.
pub fn mail<H: Host>(host: &H, result_code: &str) {
    host.point(&ae::MAIL, &[result_code]);
}

/// `session {event}`: `issued`, `renewed`, `rotated`, `revoked`.
pub fn session<H: Host>(host: &H, event: &str) {
    host.point(&ae::SESSION, &[event]);
}

/// `ledger {event}`: `hold`, `committed`, `released`, `grant`, `void`.
pub fn ledger<H: Host>(host: &H, event: &str) {
    host.point(&ae::LEDGER, &[event]);
}

/// `alert {rule}`: one of `ae::account::ALERTS`.
pub fn alert<H: Host>(host: &H, rule: &str) {
    host.point(&ae::ALERT, &[rule]);
}

/// `error {code}`: `d1`, `quota`, `queue`, `mail`, `config`.
pub fn error<H: Host>(host: &H, code: &str) {
    host.point(&ae::ERROR, &[code]);
}
