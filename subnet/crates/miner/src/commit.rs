//! Preparing a commitment: the opening, its 0600 file and the call payload.
//!
//! This step reads no chain state itself: the caller supplies the chain
//! binding, the admitting cohort and the hotkey's finalized registration. The
//! CLI reads those through rtao; tests supply them directly. It reads only
//! the object's header ([`ObjectHeader`]): the commitment binds the content
//! the header derives, so no file of the object is read or hashed.

use std::path::PathBuf;

use anyhow::{Result, bail, ensure};
use slop_ninja_protocol::submission::{SUBMISSION_PAYLOAD_LEN, SubmissionOpening};
use slop_ninja_protocol::{ChainBinding, NeuronIdentity};

use crate::client::ss58;
use crate::object::ObjectHeader;
use crate::opening::{NonceSource, OpeningFile, Store, make_opening};

/// A prepared, stored commitment.
#[derive(Clone, Debug)]
pub struct Prepared {
    pub path: PathBuf,
    pub file: OpeningFile,
    pub opening: SubmissionOpening,
    pub payload: [u8; SUBMISSION_PAYLOAD_LEN],
}

/// An opening that is not stored: a dry run's, discarded afterwards.
#[derive(Clone, Debug)]
pub struct Draft {
    pub file: OpeningFile,
    pub opening: SubmissionOpening,
    pub payload: [u8; SUBMISSION_PAYLOAD_LEN],
}

/// Makes a fresh opening for `cohort` and stores it before anything is
/// signed, so a commitment that lands is never without its opening.
///
/// Without `replace`, refuses when this hotkey already holds an opening for
/// the cohort: committing again overwrites the on-chain commitment, and the
/// new one's later position is the one that counts.
pub fn prepare(
    store: &Store,
    chain: &ChainBinding,
    cohort: u64,
    identity: &NeuronIdentity,
    object: &ObjectHeader,
    nonces: &mut dyn NonceSource,
    replace: bool,
) -> Result<Prepared> {
    let draft = make(store, chain, cohort, identity, object, nonces, replace)?;
    let path = store.save_new_opening(&draft.file)?;
    Ok(Prepared {
        path,
        file: draft.file,
        opening: draft.opening,
        payload: draft.payload,
    })
}

/// [`prepare`] without storing anything: a dry run's opening, which is
/// discarded and never committed.
pub fn make(
    store: &Store,
    chain: &ChainBinding,
    cohort: u64,
    identity: &NeuronIdentity,
    object: &ObjectHeader,
    nonces: &mut dyn NonceSource,
    replace: bool,
) -> Result<Draft> {
    let hotkey = ss58(&identity.hotkey);
    let existing = store.openings_for(chain.netuid, cohort, &hotkey)?;
    if !replace && !existing.is_empty() {
        let submitted = existing.iter().find(|(_, f)| f.submitted.is_some());
        match submitted {
            Some((path, _)) => bail!(
                "this hotkey already committed for cohort {cohort} ({}). Committing again \
                 overwrites that commitment on chain and moves the hotkey to the new \
                 commitment's position; pass --replace to do that",
                path.display()
            ),
            None => bail!(
                "an opening for cohort {cohort} exists that was never confirmed on chain ({}): an \
                 interrupted commit or a failed submission, which may still have landed. Run \
                 status to see the commitment the coordinator observed; pass --replace to commit \
                 a new one",
                existing[existing.len() - 1].0.display()
            ),
        }
    }
    let opening = make_opening(
        chain,
        cohort,
        identity.hotkey,
        identity.registration_block,
        object,
        nonces,
    )?;
    ensure!(
        existing
            .iter()
            .all(|(_, f)| f.commitment != opening.commitment().hex()),
        "the random source repeated a nonce"
    );
    let file = OpeningFile::new(&opening, object, &hotkey);
    Ok(Draft {
        payload: opening.chain_payload(),
        file,
        opening,
    })
}
