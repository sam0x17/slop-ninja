//! Reads and statements several flows share: the switches, identity
//! lookups, user states, labels and the per-insert sweep of the flow
//! tables.

use slop_ninja_account_wire::{Provider, UserState};
use slop_ninja_detect_page::switches::{self, Switch};
use slop_ninja_detect_page::{BillingState, UserId};

use crate::config::Config;
use crate::host::{DbError, Host, Row, Stmt, Val};

/// The account switches' effective states for this request: the D1 row
/// under the var's hard off. When the rows cannot be read the vars decide.
#[derive(Clone, Debug)]
pub struct Switches {
    rows: Vec<(String, String)>,
    vars: Vec<(&'static str, String)>,
}

impl Switches {
    pub async fn load<H: Host>(cfg: &Config, host: &H) -> Switches {
        let rows = match host.all(Stmt::new("SELECT name, state FROM switches", vec![])).await {
            Ok(rows) => {
                rows.iter().filter_map(|r| Some((r.text("name")?.to_owned(), r.text("state")?.to_owned()))).collect()
            }
            Err(_) => {
                crate::metrics::error(host, "d1");
                Vec::new()
            }
        };
        Switches { rows, vars: cfg.switch_vars.clone() }
    }

    /// The effective state of switch `name` (an account switch).
    pub fn state(&self, name: &str) -> &'static str {
        let Some(sw) = switches::switch(name) else { return "off" };
        self.of(sw)
    }

    fn of(&self, sw: &Switch) -> &'static str {
        let var = self.vars.iter().find(|(n, _)| *n == sw.name).map_or("", |(_, v)| v.as_str());
        let row = self.rows.iter().find(|(n, _)| n == sw.name).map(|(_, s)| s.as_str());
        switches::effective(sw, var, row)
    }

    /// `auth.<method>` is on.
    pub fn method_on(&self, method: Provider) -> bool {
        self.state(method.switch()) == "on"
    }

    /// `ACCOUNTS = read_only`: no new accounts, links or saves.
    pub fn read_only(&self) -> bool {
        self.state("ACCOUNTS") == "read_only"
    }

    pub fn billing(&self) -> BillingState {
        BillingState::parse(self.state("BILLING")).unwrap_or(BillingState::Off)
    }
}

/// An identity row with its account's state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Known {
    pub identity_id: i64,
    pub user_id: UserId,
    pub state: UserState,
}

/// The identity `(provider, subject)`, if any.
pub async fn identity<H: Host>(host: &H, provider: Provider, subject: &str) -> Result<Option<Known>, DbError> {
    let row = host
        .first(crate::stmt!(
            "SELECT i.identity_id, i.user_id, u.state FROM identities i JOIN users u ON u.user_id = i.user_id \
             WHERE i.provider = ? AND i.subject = ?",
            provider.as_str(),
            subject
        ))
        .await?;
    Ok(row.and_then(|r| known(&r)))
}

fn known(r: &Row) -> Option<Known> {
    Some(Known {
        identity_id: r.int("identity_id")?,
        user_id: UserId(r.b16("user_id")?),
        state: UserState::parse(r.text("state")?)?,
    })
}

/// A user's state, if the user exists.
pub async fn user_state<H: Host>(host: &H, user: &UserId) -> Result<Option<UserState>, DbError> {
    let row = host.first(crate::stmt!("SELECT state FROM users WHERE user_id = ?", user.0)).await?;
    Ok(row.and_then(|r| UserState::parse(r.text("state")?)))
}

/// The SQL condition "the user `?` is active".
pub const USER_ACTIVE: &str = "EXISTS (SELECT 1 FROM users WHERE user_id = ? AND state = 'active')";

/// The label a holder sees for one of their identities: the address, the
/// `@handle`, the GitHub login or the SS58 address.
pub fn label(provider: Provider, email: Option<&str>, handle: Option<&str>) -> String {
    match provider {
        Provider::Email => email.unwrap_or("").to_owned(),
        Provider::Google => email.map_or_else(|| "Google account".to_owned(), str::to_owned),
        Provider::Github => handle.unwrap_or("GitHub account").to_owned(),
        Provider::X => handle.map_or_else(|| "X account".to_owned(), |h| format!("@{h}")),
        Provider::Bittensor => handle.unwrap_or("").to_owned(),
    }
}

/// The label of a row with `provider`, `email` and `handle` columns.
pub fn row_label(r: &Row) -> String {
    let p = r.text("provider").and_then(Provider::parse).unwrap_or(Provider::Email);
    label(p, r.text("email"), r.text("handle"))
}

/// Deletes up to `n` expired rows of a flow table (before an insert into
/// it, in the same batch).
pub fn prune(table: &str, now_ms: u64, n: u32) -> Stmt {
    debug_assert!(matches!(table, "auth_flows" | "email_tokens" | "wallet_chals" | "pending_links"));
    Stmt::new(
        format!("DELETE FROM {table} WHERE rowid IN (SELECT rowid FROM {table} WHERE expires_at < ? LIMIT ?)"),
        vec![Val::from(now_ms), Val::from(n)],
    )
}

/// Whether both accounts hold money: an active payment with tokens left, or
/// an active plan period. A merge is refused then; the operator decides.
pub async fn both_hold_money<H: Host>(host: &H, a: &UserId, b: &UserId) -> Result<bool, DbError> {
    let mut both = true;
    for u in [a, b] {
        let r = host
            .first(crate::stmt!(
                "SELECT (SELECT COUNT(*) FROM payments WHERE user_id = ? AND state = 'active' AND tokens_left > 0) \
                 + (SELECT COUNT(*) FROM plan_periods WHERE user_id = ? AND state = 'active') AS n",
                u.0,
                u.0
            ))
            .await?;
        both &= r.and_then(|r| r.int("n")).unwrap_or(0) > 0;
    }
    Ok(both)
}
