//! The application semantic guard at the signing boundary.
//!
//! There is no runtime guard. No spec or transaction version is compared
//! anywhere in this crate: the adapter resolves every operation by name in live
//! metadata at each signing attempt and defers only an operation that has no
//! type-correct encoding (see `slop_ninja_subnet::surface`). This guard adds
//! the application's own conditions, checked again for every attempt:
//!
//! 1. the operation and its exact row were verified from a signed package in
//!    this process (work recovered from an earlier process waits until its
//!    package is verified again);
//! 2. the call is a timelocked commit, never direct weights;
//! 3. commit/reveal is enabled at fresh best-block state. A disabled flag
//!    refuses the attempt, which pauses new commitments without any fallback.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use rtao_client::journal::{Incompatibility, OperationIntent, SemanticGuard};
use rtao_client::{BlockRef, Client, UnsignedExtrinsic, ViewExt};
use rtao_primitives::{IntoCoreValue, NetUid};
use rtao_runtime::storage::subtensor_module as st;
use slop_ninja_subnet::WeightCorrelation;

/// Semantic contract recorded with work enqueued by this validator: rows
/// derived from a verified coordinator package under `testnet-reward-v0`.
pub const PACKAGE_CONTRACT: &str = "slop_ninja.validator.package-row.v1";

/// The only weight calls this validator signs.
const TIMELOCKED_CALLS: [&str; 2] = [
    "commit_timelocked_mechanism_weights",
    "commit_timelocked_weights",
];

fn refusal(code: &str, detail: &str) -> Incompatibility {
    Incompatibility {
        code: code.into(),
        descriptor: None,
        detail: detail.into(),
        extensions: Default::default(),
    }
}

/// Operation IDs verified in this process, each with its row hash.
#[derive(Clone, Default)]
pub struct Authorized(Arc<Mutex<BTreeMap<String, String>>>);

impl Authorized {
    pub fn insert(&self, operation_id: &str, row_hash: &str) {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(operation_id.into(), row_hash.into());
    }

    pub fn row_hash(&self, operation_id: &str) -> Option<String> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(operation_id)
            .cloned()
    }
}

/// Check the intent against the verified set. Pure; shared with tests.
pub fn check_intent(
    authorized: &Authorized,
    intent: &OperationIntent,
) -> Result<(), Incompatibility> {
    let Some(expected) = authorized.row_hash(&intent.id) else {
        return Err(refusal(
            "slop_ninja_package_unverified",
            "This operation's package has not been verified in this process.",
        ));
    };
    let correlation: WeightCorrelation = intent
        .correlation_id
        .as_deref()
        .and_then(|text| serde_json::from_str(text).ok())
        .ok_or_else(|| {
            refusal(
                "slop_ninja_correlation_missing",
                "The operation carries no readable weight correlation.",
            )
        })?;
    if correlation.row_hash != expected {
        return Err(refusal(
            "slop_ninja_row_mismatch",
            "The operation's row differs from the verified package row.",
        ));
    }
    if intent.semantic_policy.as_deref() != Some(PACKAGE_CONTRACT) {
        return Err(refusal(
            "slop_ninja_contract_mismatch",
            "The operation was opened under another semantic contract.",
        ));
    }
    Ok(())
}

/// Check that a decoded call is a timelocked commit. Pure; shared with tests.
pub fn check_call_name(pallet: &str, call: &str) -> Result<(), Incompatibility> {
    if pallet == "SubtensorModule" && TIMELOCKED_CALLS.contains(&call) {
        Ok(())
    } else {
        Err(refusal(
            "slop_ninja_direct_weights_refused",
            "Only timelocked weight commits are signed; direct weights are never a fallback.",
        ))
    }
}

pub struct PackageGuard {
    pub authorized: Authorized,
    pub netuid: NetUid,
}

impl PackageGuard {
    async fn commit_reveal_enabled(&self, client: &Client) -> Result<bool, Incompatibility> {
        let unavailable = || {
            refusal(
                "commit_reveal_unreadable",
                "Commit/reveal state could not be read before signing; the attempt is retried later.",
            )
        };
        // `client` is the executor's (signing) transport; read at the best
        // head of the node that receives the submission.
        let client = client.receipt_client().await.map_err(|_| unavailable())?;
        let snapshot = client.at(BlockRef::Best).await.map_err(|_| unavailable())?;
        let typed = snapshot
            .fetch((
                st::NETWORKS_ADDED.at((self.netuid,)),
                st::COMMIT_REVEAL_WEIGHTS_ENABLED.at((self.netuid,)),
            ))
            .await;
        let (exists, enabled) = match typed {
            Ok(values) => values,
            Err(_) => {
                // Compiled descriptors no longer match: read the same items by
                // name with live key types and defaults.
                let key = vec![vec![self.netuid.into_core_value()]];
                let read = |item: &'static str| {
                    let snapshot = &snapshot;
                    let key = key.clone();
                    async move {
                        snapshot
                            .query_batch_dyn("SubtensorModule", item, key)
                            .await
                            .ok()
                            .and_then(|values| values.into_iter().next())
                            .and_then(|value| value.as_bool())
                    }
                };
                (
                    read("NetworksAdded").await.ok_or_else(unavailable)?,
                    read("CommitRevealWeightsEnabled")
                        .await
                        .ok_or_else(unavailable)?,
                )
            }
        };
        Ok(exists && enabled)
    }
}

impl SemanticGuard for PackageGuard {
    fn check<'a>(
        &'a self,
        client: &'a Client,
        intent: &'a OperationIntent,
        unsigned: &'a UnsignedExtrinsic,
    ) -> rtao_signer::BoxFuture<'a, Result<(), Incompatibility>> {
        Box::pin(async move {
            check_intent(&self.authorized, intent)?;
            // Live metadata and chain state come from the executor's signing
            // transport (its own budget when configured), inside rtao's
            // admission window.
            let live = client.receipt_client().await;
            let live = match live {
                Ok(route) => route.refresh_runtime().await,
                Err(error) => Err(error),
            }
            .map_err(|_| {
                refusal(
                    "live_metadata_unavailable",
                    "Live metadata could not be read before signing.",
                )
            })?;
            let (pallet, call, _) =
                slop_ninja_subnet::surface::decode_named(live.core(), unsigned.call().as_bytes())
                    .map_err(|deferral| refusal(&deferral.code, &deferral.detail))?;
            check_call_name(&pallet, &call)?;
            if !self.commit_reveal_enabled(client).await? {
                return Err(refusal(
                    "commit_reveal_disabled",
                    "Commit/reveal is disabled; new commitments pause and nothing falls back to direct weights.",
                ));
            }
            Ok(())
        })
    }
}
