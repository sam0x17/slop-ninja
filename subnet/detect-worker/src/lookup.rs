//! Reads shared by the store's routes: switch states and detection lookups
//! by slug.
//!
//! - **Switches.** Each switch is the AND of its deploy-time var (the hard
//!   off) and its D1 `switches` row. The row is read in the same batch as
//!   the request's own reads, so a flip takes effect on the next request.
//!   When D1 cannot be read the var decides.
//! - **Slugs.** A slug is looked up only in its canonical form: 7
//!   characters for a public page, 20 for a secret one. Anything else is not
//!   found, with no store read. A secret slug is looked up under every live
//!   pepper version in one batch, and a row counts only under the version it
//!   was made with.

use slop_ninja_detect_page::slug::{self, PUBLIC_LEN, SECRET_LEN};

use crate::db::{DbError, Done, Io, Stmt, Val};
use crate::display::PageSwitches;
use crate::keys::{Pepper, Peppers, public_id_h};
use crate::store::{DetRow, SELECT_DET};
use crate::switches;

/// Every switch row (the table holds one row per switch name).
pub const SELECT_SWITCHES: &str = "SELECT name, state FROM switches";

/// The switch rows of one read.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SwitchRows {
    /// `None`: the rows could not be read, so each var decides.
    pub rows: Option<Vec<switches::Row>>,
}

impl SwitchRows {
    pub fn from_done(d: Option<&Done>) -> SwitchRows {
        SwitchRows {
            rows: d.map(|d| {
                d.rows
                    .iter()
                    .filter_map(|r| {
                        Some(switches::Row { name: r.text("name")?.to_owned(), state: r.text("state")?.to_owned() })
                    })
                    .collect()
            }),
        }
    }

    /// The effective state of `name` (a switch of the table), by the free
    /// check's own rule ([`switches::state_of`]).
    pub fn state(&self, io: &impl Io, name: &str) -> &'static str {
        switches::state_of(name, io.var(name).as_deref(), self.rows.as_deref())
    }

    pub fn on(&self, io: &impl Io, name: &str) -> bool {
        self.state(io, name) == "on"
    }

    /// The switches a page read depends on.
    pub fn page(&self, io: &impl Io) -> PageSwitches {
        PageSwitches {
            pages: self.on(io, "DETECT_PAGES"),
            cards_generic: self.state(io, "DETECT_CARDS") == "generic",
            index: self.on(io, "DETECT_INDEX"),
            excerpts: self.on(io, "DETECT_EXCERPTS"),
            x_purged: self.state(io, "DETECT_X") == "purged",
        }
    }
}

/// Reads the switch rows alone (a failed read leaves each var to decide).
pub async fn read_switches(io: &impl Io) -> SwitchRows {
    let d = io.run(Stmt::new(SELECT_SWITCHES, Vec::new())).await.ok();
    SwitchRows::from_done(d.as_ref())
}

/// What a slug is by its form.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SlugForm {
    Public,
    Secret,
}

/// The form of a canonical slug; `None` for anything else.
pub fn slug_form(s: &str) -> Option<SlugForm> {
    if slug::is_canonical(s, PUBLIC_LEN) {
        Some(SlugForm::Public)
    } else if slug::is_canonical(s, SECRET_LEN) {
        Some(SlugForm::Secret)
    } else {
        None
    }
}

/// A secret page found under its own pepper version.
#[derive(Clone, Debug)]
pub struct Found<'p> {
    pub row: DetRow,
    pub pepper: &'p Pepper,
}

/// Looks a secret slug up under every pepper version, with the switch rows
/// in the same batch. `Ok((None, _))`: no such page.
pub async fn secret_lookup<'p>(
    io: &impl Io,
    peppers: &'p Peppers,
    slug: &str,
) -> Result<(Option<Found<'p>>, SwitchRows), DbError> {
    let candidates: Vec<&Pepper> = peppers.all().collect();
    let mut batch = vec![Stmt::new(SELECT_SWITCHES, Vec::new())];
    for p in &candidates {
        batch.push(Stmt::new(SELECT_DET, vec![Val::blob(&p.id_h(slug))]));
    }
    let done = io.batch(batch).await?;
    let sw = SwitchRows::from_done(done.first());
    for (p, d) in candidates.into_iter().zip(done.iter().skip(1)) {
        let Some(r) = d.first() else { continue };
        let row = DetRow::from_row(r).map_err(|_| DbError::Shape)?;
        if row.kind.is_secret() && row.pepper_v == Some(p.v) {
            return Ok((Some(Found { row, pepper: p }), sw));
        }
    }
    Ok((None, sw))
}

/// Looks a public slug up, with the switch rows in the same batch.
pub async fn public_lookup(io: &impl Io, slug: &str) -> Result<(Option<DetRow>, SwitchRows), DbError> {
    let batch =
        vec![Stmt::new(SELECT_SWITCHES, Vec::new()), Stmt::new(SELECT_DET, vec![Val::blob(&public_id_h(slug))])];
    let done = io.batch(batch).await?;
    let sw = SwitchRows::from_done(done.first());
    let row = match done.get(1).and_then(Done::first) {
        Some(r) => Some(DetRow::from_row(r).map_err(|_| DbError::Shape)?),
        None => None,
    };
    Ok((row.filter(|r| r.kind == slop_ninja_detect_page::codes::Kind::PublicUrl), sw))
}
