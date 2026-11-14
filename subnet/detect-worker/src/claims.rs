//! Single flight for public URL checks: one claim per link and package.
//!
//! The claim row's primary key is the lock (`claims.claim_key =
//! H("claim_v1" ‖ key ‖ 0x00 ‖ package_load)`, the key being the link's
//! alias key or an X post's source key). D1 serializes writes, so one batch
//! that inserts the claim `ON CONFLICT DO NOTHING` and the pending detection
//! only where the claim names it lets exactly one submitter win:
//!
//! - the winner draws a 7-character slug, inserts the pending page and
//!   drives the check;
//! - everyone else gets the winner's slug at once and waits on the pending
//!   page, which polls its state;
//! - a claim whose lease has ended (the driver left or died) is taken over
//!   by the next submitter, or by "Run it" on the pending page, under a new
//!   fencing token.
//!
//! `claims.stage` holds the driving invocation's fencing token: every write
//! that ends the check names it, so a driver whose claim was taken over
//! changes nothing. The lease is derived, not tuned: the fetch wait, plus the
//! scan lease, plus one hold. The hourly cron deletes claims past their
//! lease; the operator's `release_claim` deletes one at once.

use slop_ninja_detect_page::codes::Platform;
use slop_ninja_detect_page::slug;

use crate::db::{Done, Io, IoError, Stmt, Val};
use crate::keys::{Bytes32, CLAIM_DOMAIN, h, public_id_h};

/// The claim key of `key` under `package_load`.
pub fn claim_key(key: &Bytes32, package_load: &str) -> Bytes32 {
    let mut data = Vec::with_capacity(33 + package_load.len());
    data.extend_from_slice(key);
    data.push(0);
    data.extend_from_slice(package_load.as_bytes());
    h(CLAIM_DOMAIN, &data)
}

/// Draws of a slug before giving up on a run of collisions (each draw is
/// 35 bits; a collision is a D1 UNIQUE refusal of the whole batch).
pub const SLUG_DRAWS: u32 = 4;

/// The claim, inserted only when no claim exists.
pub const INSERT_CLAIM: &str = "INSERT INTO claims (claim_key, det_id_h, lease_until, stage) \
VALUES (?1, ?2, ?3, ?4) ON CONFLICT (claim_key) DO NOTHING";

/// The pending page, inserted only where the claim names it.
pub const INSERT_PENDING: &str = "INSERT INTO detections (id_h, public_slug, kind, platform, state, state_v, \
created_at, updated_at, package_load) SELECT ?2, ?3, 'public_url', ?4, 'pending', 1, ?5, ?5, ?6 \
WHERE EXISTS (SELECT 1 FROM claims WHERE claim_key = ?1 AND det_id_h = ?2)";

/// The claim as it stands, with its page.
pub const SELECT_CLAIM: &str = "SELECT hex(c.det_id_h) AS det_id_h, c.lease_until AS lease_until, \
c.stage AS stage, d.public_slug AS public_slug, d.state AS state FROM claims c \
LEFT JOIN detections d ON d.id_h = c.det_id_h WHERE c.claim_key = ?1";

/// Takes over a claim whose lease has ended, under a new token.
pub const TAKE_OVER: &str = "UPDATE claims SET lease_until = ?3, stage = ?4 \
WHERE claim_key = ?1 AND det_id_h = ?2 AND lease_until <= ?5";

/// A claim whose page has ended (or is gone) frees the link.
pub const DROP_ENDED: &str = "DELETE FROM claims WHERE claim_key = ?1 AND NOT EXISTS \
(SELECT 1 FROM detections d WHERE d.id_h = claims.det_id_h AND d.state = 'pending')";

/// Releases a claim held under `stage`.
pub const RELEASE: &str = "DELETE FROM claims WHERE claim_key = ?1 AND stage = ?2";

/// The guard of every write that ends a check: the claim still names this
/// driver.
pub const FENCE: &str = "EXISTS (SELECT 1 FROM claims WHERE claim_key = ?1 AND stage = ?2)";

/// A driver's hold on a claim.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Held {
    pub claim_key: Bytes32,
    /// The fencing token (`claims.stage`), 32 hex digits.
    pub token: String,
    pub id_h: Bytes32,
    pub slug: String,
}

/// What a claim attempt found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Claim {
    /// This invocation drives the check: a new pending page, or one whose
    /// earlier driver's lease had ended.
    Drive(Held),
    /// Another invocation drives it: wait on its page.
    Joined { slug: String },
}

fn token(io: &impl Io) -> String {
    let mut b = [0u8; 16];
    io.fill(&mut b);
    hex::encode(b)
}

/// Claims `ckey` for a new pending page on `platform`, or joins (or takes
/// over) the check that holds it.
pub async fn claim(
    io: &impl Io,
    ckey: &Bytes32,
    platform: Platform,
    package_load: &str,
    lease_ms: u64,
) -> Result<Claim, IoError> {
    for _ in 0..SLUG_DRAWS {
        let now = io.now_ms();
        let mut fill = |b: &mut [u8]| io.fill(b);
        let Ok(slug_) = slug::draw_public(&mut fill) else { return Err(IoError) };
        let id = public_id_h(&slug_);
        let tok = token(io);
        let r = io
            .batch(vec![
                Stmt::new(DROP_ENDED, vec![Val::blob(ckey)]),
                Stmt::new(
                    INSERT_CLAIM,
                    vec![Val::blob(ckey), Val::blob(&id), Val::int(now.saturating_add(lease_ms)), Val::text(&tok)],
                ),
                Stmt::new(
                    INSERT_PENDING,
                    vec![
                        Val::blob(ckey),
                        Val::blob(&id),
                        Val::text(&slug_),
                        Val::text(platform.as_str()),
                        Val::int(now),
                        Val::text(package_load),
                    ],
                ),
                Stmt::new(SELECT_CLAIM, vec![Val::blob(ckey)]),
            ])
            .await;
        // A slug that collides with an existing page refuses the batch:
        // draw again.
        let Ok(done) = r else { continue };
        let Some(row) = done.get(3).and_then(Done::first) else { return Err(IoError) };
        let holder = row.hex32("det_id_h").ok_or(IoError)?;
        if holder == id {
            return Ok(Claim::Drive(Held { claim_key: *ckey, token: tok, id_h: id, slug: slug_ }));
        }
        let held_slug = row.text("public_slug").ok_or(IoError)?.to_owned();
        let lease_until = row.u64("lease_until").unwrap_or(0);
        if lease_until > now {
            return Ok(Claim::Joined { slug: held_slug });
        }
        let tok = token(io);
        let d = io
            .run(Stmt::new(
                TAKE_OVER,
                vec![
                    Val::blob(ckey),
                    Val::blob(&holder),
                    Val::int(now.saturating_add(lease_ms)),
                    Val::text(&tok),
                    Val::int(now),
                ],
            ))
            .await?;
        return Ok(if d.changes == 1 {
            // A claim taken over past its lease (`job {claim_expired}`).
            io.metric_n("job", &["claim_expired"], 1.0);
            Claim::Drive(Held { claim_key: *ckey, token: tok, id_h: holder, slug: held_slug })
        } else {
            Claim::Joined { slug: held_slug }
        });
    }
    Err(IoError)
}

/// Extends the hold (a driver about to start a long step).
pub async fn renew(io: &impl Io, held: &Held, lease_ms: u64) -> Result<bool, IoError> {
    let d = io
        .run(Stmt::new(
            "UPDATE claims SET lease_until = ?3 WHERE claim_key = ?1 AND stage = ?2",
            vec![Val::blob(&held.claim_key), Val::text(&held.token), Val::int(io.now_ms().saturating_add(lease_ms))],
        ))
        .await?;
    Ok(d.changes == 1)
}

/// The operator's `release_claim`: deletes the claim of `slug`'s page, so
/// the next submitter starts over (the page itself stays pending and offers
/// "Run it").
pub fn release_claim(slug_: &str) -> Stmt {
    Stmt::new("DELETE FROM claims WHERE det_id_h = ?1", vec![Val::blob(&public_id_h(slug_))])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claim_keys_separate_links_and_packages() {
        let a = claim_key(&[1; 32], "pkg-a");
        assert_ne!(a, claim_key(&[1; 32], "pkg-b"));
        assert_ne!(a, claim_key(&[2; 32], "pkg-a"));
        assert_eq!(a, claim_key(&[1; 32], "pkg-a"));
    }
}
