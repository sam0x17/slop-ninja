//! The scorer client's pure half lives in `slop-ninja-scorer-client`
//! (`subnet/crates/scorer-client`), shared with the detect Worker's lane. It
//! is re-exported here under its former path, so the coordinator's lanes and
//! tests read it unchanged. Only the mapping onto the lane logic's restart
//! failure stays here, because that type is the coordinator's.

pub use slop_ninja_scorer_client::*;

use crate::eval::lane_logic::RestartFailure;

/// The lane logic's restart failure for a failed prepare.
pub fn restart(p: Prep) -> RestartFailure {
    match p {
        Prep::Infra(c) => RestartFailure::Infra(c),
        Prep::Artifact(code, definitive) => RestartFailure::Artifact { code, definitive },
    }
}
