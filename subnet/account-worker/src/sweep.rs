//! The crons: every 5 minutes the expired flows, email links, wallet
//! challenges and pending links go; hourly the sessions past either expiry.
//! Each table is swept through its expiry index, a bounded number of rows a
//! run (every insert also deletes up to 20 of its own table's expired rows,
//! so the tables stay near their live size between runs).

use crate::host::{Host, Stmt, Val};

/// Rows one run deletes from one table at most.
pub const PER_RUN: u32 = 1000;

/// The 5-minute cron.
pub const FLOWS_CRON: &str = "*/5 * * * *";
/// The hourly cron.
pub const SESSIONS_CRON: &str = "0 * * * *";

/// Runs the cron named `cron`; answers the rows deleted.
pub async fn run<H: Host>(host: &H, cron: &str) -> u64 {
    let now = host.now_ms();
    let stmts: Vec<Stmt> = match cron {
        FLOWS_CRON => ["auth_flows", "email_tokens", "wallet_chals", "pending_links"]
            .iter()
            .map(|t| {
                Stmt::new(
                    format!(
                        "DELETE FROM {t} WHERE rowid IN (SELECT rowid FROM {t} WHERE expires_at < ? LIMIT ?) RETURNING expires_at"
                    ),
                    vec![Val::from(now), Val::from(PER_RUN)],
                )
            })
            .collect(),
        SESSIONS_CRON => vec![Stmt::new(
            "DELETE FROM sessions WHERE rowid IN (SELECT rowid FROM sessions WHERE idle_expires_at <= ? OR abs_expires_at <= ? \
             LIMIT ?) RETURNING created_at",
            vec![Val::from(now), Val::from(now), Val::from(PER_RUN)],
        )],
        _ => return 0,
    };
    match host.batch(stmts).await {
        Ok(rows) => rows.iter().map(|r| r.len() as u64).sum(),
        Err(_) => {
            crate::metrics::error(host, "d1");
            0
        }
    }
}
