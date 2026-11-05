//! Verifies a published `reward-v1` cohort from its stored objects, as a
//! validator would, and writes the reward-v1 bundles (T12 shortcut) that the
//! native validator and the coordinator install.
//!
//! ```text
//! reward_v1_verify cohort DIR --trust TRUST.json [--snapshot-owner HOTKEY --snapshot UID:HOTKEY:REG ...
//!     [--snapshot-at BLOCK:HASH]]
//!     [--authorization-seed FILE --issued-epoch E --carrier DIR2]
//! reward_v1_verify launch --trust TRUST.json --authorization-seed FILE --issued-epoch E [--generation G] --out FILE
//! reward_v1_verify revoke --trust TRUST.json --authorization-seed FILE --bundle FILE --issued-epoch E
//!     --finalized-epoch F [--generation G] --out FILE
//! ```
//!
//! `cohort`: DIR holds the cohort's receipts-bucket objects at their key
//! suffixes (`manifest.signed`, `package.signed`, `target.signed` when the
//! cohort produced a target, `receipt-index.signed`, `chunk-map.json` and every
//! chunk object under `receipts/`, `published-contents.bin`,
//! `fingerprint-index.bin`, and after the closing boundary `closure.signed`,
//! `published-contents-after.bin`, `fingerprint-index-after.bin` and
//! `closure-fingerprints.json`). Every signed record is authenticated before
//! it is decoded under the pinned trust roots; the package is bound to the
//! manifest, the target to the package, the index to the package's root and
//! count, every receipt cut from a chunk to its index entry (each entry once),
//! the published index to the manifest's binding; the outcome is recomputed
//! (`check_outcome`), the closure is checked against the manifest's frozen
//! minimums, and the index after the closure is rebuilt from the closure's
//! fingerprints. With a snapshot, the canonical row is derived; `--snapshot-at`
//! binds it to a finalized block and hash (block 0 and a zero hash otherwise),
//! so a row a validator or the coordinator derived at that block is
//! reproduced digest for digest. With an
//! authorization seed (the T12 authorization key, read from a mode-600 file
//! and never printed), the reward-intent authorization for the active target
//! is signed and the bundles are written: `reward-v1-bundle.json` (receipt
//! index and chunks included, for validators) and
//! `reward-v1-bundle-coordinator.json` (the coordinator produced the receipts
//! and stores one value of at most 2 MB), and `reward-v1-bundle-v3.json`
//! (bundle schema v3, for validators: the same records, with the receipt
//! indexes and chunks named by absolute path, SHA-256 and length instead of
//! embedded, so its size does not grow with the receipts; the paths are the
//! objects in DIR and DIR2, which must stay in place on the validator's host,
//! so run the tool there). The v2 form embeds every receipt hex-encoded and
//! fits the native validator's 64 MiB file bound only up to about 150,000
//! receipts across both cohorts; the report says whether it does. `--carrier DIR2` names a later cohort
//! that produced no target and carries this one: the authorization then names
//! that cohort's manifest and package, and the bundles carry that cohort's
//! receipt index (and, for validators, its receipt chunks) and its published
//! index, so every holder recomputes the carrier's outcome. Each bundle is
//! verified here as a holder that already holds this cohort's target.
//!
//! `launch`: signs the launch authorization of the burn row and writes its bundle.
//!
//! No authorization expires: each record's `renew_by_epoch` is a non-binding
//! hint, reported as `renew_by`. A newer bundle supersedes an authorization;
//! only a signed revocation withdraws it.
//!
//! `revoke`: signs the revocation of the authorization in `--bundle` (either
//! form): it withdraws every authorization of that bundle's target issued at
//! or before `--issued-epoch`, which must not precede the authorization's
//! own issue and must not pass `--finalized-epoch` (the chain's finalized
//! epoch index now, as `/status` or the validator's `epoch_schedule` reports
//! it) by more than one: an authorization that re-authorizes the target must
//! be issued later than the revocation and cannot commit before its own
//! issue, so a mistyped far-future epoch would withdraw the target that long
//! (the coordinator's `POST /signing/revoke` applies the same bound). Writes
//! the signed record's bytes to `--out`, the body that `POST /signing/revoke`
//! and `slop_ninja_validator revoke` take. Never run against the live target
//! except to withdraw it.
//!
//! Prints a content-free JSON report.
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use slop_ninja_coordinator::target;
use slop_ninja_protocol::submission::CohortClosure;
use slop_ninja_protocol::{
    AccountId, CalculationTargetV2, ChainBinding, CohortManifestV2, Digest, EntrantFingerprint, FingerprintIndex,
    LaunchAuthorization, NeuronIdentity, PublishedContents, PublishedIndex, ReceiptIndex, Record,
    RegistrationSnapshotV2, RewardIntentAuthorization, RewardIntentRevocation, ScorePackageV2, Signed, SigningKey,
    TESTNET_GENESIS,
    TrustedKeys, sign,
};
use std::path::{Path, PathBuf};

fn fail(msg: impl std::fmt::Display) -> ! {
    eprintln!("reward_v1_verify: {msg}");
    std::process::exit(2)
}

fn key32(hex_str: &str, what: &str) -> [u8; 32] {
    hex::decode(hex_str.trim().trim_start_matches("0x"))
        .ok()
        .and_then(|b| b.try_into().ok())
        .unwrap_or_else(|| fail(format!("{what} is not 32-byte hex")))
}

struct Args {
    mode: String,
    dir: Option<PathBuf>,
    trust: PathBuf,
    seed: Option<PathBuf>,
    issued: Option<u64>,
    finalized: Option<u64>,
    carrier: Option<PathBuf>,
    out: Option<PathBuf>,
    bundle: Option<PathBuf>,
    generation: u64,
    owner: Option<[u8; 32]>,
    snapshot: Vec<NeuronIdentity>,
    at: (u64, [u8; 32]),
}

fn args() -> Args {
    let mut it = std::env::args().skip(1);
    let mode = it.next().unwrap_or_else(|| fail("usage: reward_v1_verify cohort DIR ... | launch ..."));
    let mut a = Args {
        mode,
        dir: None,
        trust: PathBuf::new(),
        seed: None,
        issued: None,
        finalized: None,
        carrier: None,
        out: None,
        bundle: None,
        generation: 1,
        owner: None,
        snapshot: Vec::new(),
        at: (0, [0; 32]),
    };
    while let Some(x) = it.next() {
        let mut val = |name: &str| it.next().unwrap_or_else(|| fail(format!("{name} needs a value")));
        match x.as_str() {
            "--trust" => a.trust = PathBuf::from(val("--trust")),
            "--authorization-seed" => a.seed = Some(PathBuf::from(val("--authorization-seed"))),
            "--issued-epoch" => a.issued = Some(val("--issued-epoch").parse().unwrap_or_else(|_| fail("epoch"))),
            "--finalized-epoch" => {
                a.finalized = Some(val("--finalized-epoch").parse().unwrap_or_else(|_| fail("finalized epoch")))
            }
            "--carrier" => a.carrier = Some(PathBuf::from(val("--carrier"))),
            "--out" => a.out = Some(PathBuf::from(val("--out"))),
            "--bundle" => a.bundle = Some(PathBuf::from(val("--bundle"))),
            "--generation" => a.generation = val("--generation").parse().unwrap_or_else(|_| fail("generation")),
            "--snapshot-owner" => a.owner = Some(key32(&val("--snapshot-owner"), "owner hotkey")),
            "--snapshot" => {
                let v = val("--snapshot");
                let p: Vec<&str> = v.split(':').collect();
                let [uid, hk, reg] = p.as_slice() else { fail("--snapshot UID:HOTKEY:REG") };
                a.snapshot.push(NeuronIdentity {
                    uid: uid.parse().unwrap_or_else(|_| fail("uid")),
                    hotkey: AccountId(key32(hk, "snapshot hotkey")),
                    registration_block: reg.parse().unwrap_or_else(|_| fail("registration block")),
                });
            }
            "--snapshot-at" => {
                let v = val("--snapshot-at");
                let (block, hash) = v.split_once(':').unwrap_or_else(|| fail("--snapshot-at BLOCK:HASH"));
                a.at = (block.parse().unwrap_or_else(|_| fail("snapshot block")), key32(hash, "snapshot block hash"));
            }
            d if a.dir.is_none() && !d.starts_with("--") => a.dir = Some(PathBuf::from(d)),
            other => fail(format!("unknown argument {other}")),
        }
    }
    a
}

fn trust(path: &Path) -> (TrustedKeys, [u8; 32]) {
    let v: Value = serde_json::from_slice(&std::fs::read(path).unwrap_or_else(|_| fail("read trust file")))
        .unwrap_or_else(|_| fail("trust json"));
    let k = |name: &str| key32(v[name].as_str().unwrap_or_else(|| fail(format!("{name} missing"))), name);
    let auth = k("authorization_public_key");
    let keys = TrustedKeys::new(k("package_public_key"), k("receipt_public_key"), auth)
        .unwrap_or_else(|_| fail("trusted keys"));
    (keys, auth)
}

/// The T12 authorization key from its seed file; it must derive the pinned root.
fn authorization_key(seed: &Path, root: &[u8; 32]) -> SigningKey {
    let s = std::fs::read_to_string(seed).unwrap_or_else(|_| fail("read authorization seed"));
    let k = SigningKey::from_bytes(&key32(&s, "authorization seed"));
    if &k.verifying_key().to_bytes() != root {
        fail("the authorization seed does not derive the pinned authorization root");
    }
    k
}

fn chain() -> ChainBinding {
    ChainBinding { genesis: TESTNET_GENESIS, netuid: 601, mechanism: 0 }
}

fn read(dir: &Path, name: &str) -> Vec<u8> {
    std::fs::read(dir.join(name)).unwrap_or_else(|_| fail(format!("read {name}")))
}

fn maybe(dir: &Path, name: &str) -> Option<Vec<u8>> {
    std::fs::read(dir.join(name)).ok()
}

fn main() {
    let a = args();
    let (keys, auth_root) = trust(&a.trust);
    match a.mode.as_str() {
        "launch" => launch(&a, &keys, &auth_root),
        "cohort" => cohort(&a, &keys, &auth_root),
        "revoke" => revoke(&a, &keys, &auth_root),
        m => fail(format!("unknown mode {m}")),
    }
}

fn launch(a: &Args, keys: &TrustedKeys, root: &[u8; 32]) {
    let k = authorization_key(a.seed.as_deref().unwrap_or_else(|| fail("--authorization-seed")), root);
    let issued = a.issued.unwrap_or_else(|| fail("--issued-epoch"));
    let auth = LaunchAuthorization::new(&chain(), a.generation, issued).unwrap_or_else(|e| fail(e));
    let signed = sign(auth, &k).and_then(|s| s.to_bytes()).unwrap_or_else(|e| fail(e));
    let b = target::BundleV2 {
        schema: target::BUNDLE_SCHEMA_V2.into(),
        shortcut: "T12".into(),
        source: None,
        authorization: target::AuthorizationHex {
            kind: "launch".into(),
            authorization: hex::encode(&signed),
            renewal: None,
            carrier: None,
        },
        receipt_chunks: Vec::new(),
        carrier_receipt_chunks: Vec::new(),
    };
    let bytes = serde_json::to_vec_pretty(&b).unwrap_or_else(|_| fail("bundle json"));
    let held = slop_ninja_protocol::ActiveTarget::launch(&chain()).unwrap_or_else(|e| fail(e));
    let v = target::verify_v2(&target::parse_v2(&bytes).unwrap_or_else(|r| fail(r.detail)), keys, &TESTNET_GENESIS.0, 601, &held)
        .unwrap_or_else(|r| fail(format!("{}: {}", r.code, r.detail)));
    let out = a.out.clone().unwrap_or_else(|| fail("--out"));
    std::fs::write(&out, &bytes).unwrap_or_else(|_| fail("write bundle"));
    let terms = v.checked_terms(keys).unwrap_or_else(|e| fail(e));
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({"verified": true, "kind": "launch", "target": v.target_digest.to_string(),
            "authorization": terms.digest.to_string(), "issued_epoch": terms.issued_epoch,
            "renew_by": terms.renew_by_epoch, "bundle": out.display().to_string(),
            "bundle_sha256": hex::encode(Sha256::digest(&bytes))}))
        .unwrap_or_default()
    );
}

fn cohort(a: &Args, keys: &TrustedKeys, root: &[u8; 32]) {
    let dir = a.dir.clone().unwrap_or_else(|| fail("DIR"));
    let manifest = Signed::<CohortManifestV2>::open(&read(&dir, "manifest.signed"), keys).unwrap_or_else(|e| fail(format!("manifest: {e}")));
    let package = Signed::<ScorePackageV2>::open(&read(&dir, "package.signed"), keys).unwrap_or_else(|e| fail(format!("package: {e}")));
    let target_rec = maybe(&dir, "target.signed")
        .map(|b| Signed::<CalculationTargetV2>::open(&b, keys).unwrap_or_else(|e| fail(format!("target: {e}"))));
    let index = Signed::<ReceiptIndex>::open(&read(&dir, "receipt-index.signed"), keys).unwrap_or_else(|e| fail(format!("index: {e}")));
    if package.chain != chain() || manifest.chain != chain() {
        fail("another chain or subnet");
    }
    package.check_manifest(&manifest).unwrap_or_else(|e| fail(format!("package manifest: {e}")));
    package.check_receipts(&index).unwrap_or_else(|e| fail(format!("receipt binding: {e}")));
    if let Some(t) = &target_rec {
        t.check_package(&package).unwrap_or_else(|e| fail(format!("target package: {e}")));
    }
    // Every chunk object, then every receipt once.
    let (chunks, chunk_bytes) = chunks_of(&dir, &manifest.cohort_label, &index);
    let refs: Vec<&[u8]> = chunks.iter().map(Vec::as_slice).collect();
    let receipts = slop_ninja_protocol::target_bundle::verify_receipt_chunks(&index, &refs)
        .unwrap_or_else(|e| fail(format!("receipts: {e}")));
    // The published index the manifest binds, and the recomputed outcome.
    let at_seal = PublishedIndex {
        contents: PublishedContents::from_bytes(&read(&dir, "published-contents.bin")).unwrap_or_else(|e| fail(e)),
        fingerprints: FingerprintIndex::from_bytes(&read(&dir, "fingerprint-index.bin")).unwrap_or_else(|e| fail(e)),
    };
    if at_seal.binding().ok() != Some(manifest.published) {
        fail("published index binding");
    }
    let evaluation = slop_ninja_protocol::reward_v1::check_outcome(&package, &at_seal)
        .unwrap_or_else(|e| fail(format!("recomputed outcome differs: {e}")));
    // The closure and the index after it, once the cohort closed.
    let closure = maybe(&dir, "closure.signed")
        .map(|b| Signed::<CohortClosure>::open(&b, keys).unwrap_or_else(|e| fail(format!("closure: {e}"))));
    let mut closure_report = Value::Null;
    if let Some(c) = &closure {
        c.check(&manifest, &package, target_rec.as_ref()).unwrap_or_else(|e| fail(format!("closure check: {e}")));
        let fps: Value = serde_json::from_slice(&read(&dir, "closure-fingerprints.json")).unwrap_or_else(|_| fail("fps"));
        let list: Vec<EntrantFingerprint> = fps["fingerprints"]
            .as_array()
            .unwrap_or_else(|| fail("fingerprints"))
            .iter()
            .map(|f| EntrantFingerprint {
                hotkey: AccountId(key32(f["hotkey"].as_str().unwrap_or_default(), "hotkey")),
                version: f["version"].as_u64().unwrap_or_else(|| fail("version")) as u32,
                fingerprint: Digest(key32(f["fingerprint"].as_str().unwrap_or_default(), "fingerprint")),
            })
            .collect();
        let rebuilt = at_seal.after_closure(&package, list).unwrap_or_else(|e| fail(format!("index after closure: {e}")));
        let after = PublishedIndex {
            contents: PublishedContents::from_bytes(&read(&dir, "published-contents-after.bin")).unwrap_or_else(|e| fail(e)),
            fingerprints: FingerprintIndex::from_bytes(&read(&dir, "fingerprint-index-after.bin")).unwrap_or_else(|e| fail(e)),
        };
        if rebuilt != after {
            fail("the published index after the closure differs from its rebuild");
        }
        closure_report = json!({"admission_closed_epoch": c.admission_closed_epoch, "published_epoch": c.published_epoch,
            "closed_epoch": c.closed_epoch, "target": c.target.map(|d| d.to_string()),
            "index_after": {"contents": after.contents.contents.len(), "fingerprints": after.fingerprints.entries.len(),
                "binding": format!("{:?}", after.binding().ok())}});
    }
    let mut report = json!({
        "verified": true,
        "cohort": manifest.cohort, "label": manifest.cohort_label,
        "manifest": manifest.digest().map(|d| d.to_string()).unwrap_or_default(),
        "package": package.digest().map(|d| d.to_string()).unwrap_or_default(),
        "target": target_rec.as_ref().and_then(|t| t.digest().ok()).map(|d| d.to_string()),
        "target_predecessor": target_rec.as_ref().and_then(|t| t.predecessor).map(|d| d.to_string()),
        "mode": format!("{:?}", package.mode),
        "promoted": package.promoted.map(|h| hex::encode(h.0)),
        "promotion_audits": package.promotion_audits.iter().map(|x| json!({"head": hex::encode(x.head.0),
            "strict_fpr_passed": x.strict_fpr_passed, "flags": x.canary.iter().map(|c| c.flag.clone()).collect::<Vec<_>>(),
            "blocked": x.blocked()})).collect::<Vec<_>>(),
        "reference_index": package.reference_index,
        "reference_score": package.reference.score.get().to_string(),
        "predecessor_score": package.predecessor.as_ref().map(|p| p.score.get().to_string()),
        "batch": {"cases": manifest.batch.cases, "mix": [manifest.batch.mix.human_only, manifest.batch.mix.mixed, manifest.batch.mix.model_only],
            "strata": manifest.batch.strata.len(), "source_groups": manifest.batch.source_groups},
        "probes": manifest.probes.iter().map(|p| json!({"version": p.version, "size": p.size})).collect::<Vec<_>>(),
        "published_at_seal": {"contents": at_seal.contents.contents.len(), "fingerprints": at_seal.fingerprints.entries.len()},
        "params": {"sigma": manifest.params.sigma.to_string(), "delta": manifest.params.margin().to_string()},
        "cutoff_q": evaluation.outcome.cutoff.map(|t| t.to_string()),
        "quant": {"width": package.quant.width.to_string(), "band_width": package.quant.band_width.map(|w| w.to_string()),
            "reference_q": package.quant.reference_q.get().to_string(), "charged": package.quant.charged,
            "result": package.result.to_string()},
        "entrants": package.entrants.iter().map(|e| json!({
            "uid": e.identity.uid, "hotkey": hex::encode(e.identity.hotkey.0), "registration_block": e.identity.registration_block,
            "content": e.content.to_string(), "score": e.score.map(|s| s.get().to_string()),
            "q": e.q.map(|s| s.get().to_string()),
            "assigned": e.assigned, "succeeded": e.succeeded, "failed": e.failed,
            "gates": e.gate_failures.iter().map(|g| g.reason.clone()).collect::<Vec<_>>(),
            "public_distance_current_ppb": e.public.as_ref().and_then(|p| p.current.distance_ppb()),
            "cluster_head": e.cluster_head.map(|h| hex::encode(h.0)), "units": e.units})).collect::<Vec<_>>(),
        "credits": package.credits.iter().map(|c| json!({"head": hex::encode(c.head.0), "credit": format!("{}/{}", c.num, c.den)})).collect::<Vec<_>>(),
        "distance_monitor": format!("{:?}", package.distance_monitor),
        "receipt_root": package.receipt_root.to_string(), "receipt_count": package.receipt_count,
        "receipts_verified": receipts.len(), "chunk_objects": chunks.len(), "chunk_bytes": chunk_bytes,
        "closure": closure_report,
    });
    // The row for a snapshot (the active target's: this cohort's when it
    // produced one, the launch burn row otherwise).
    if let Some(owner) = a.owner {
        let mut neurons = a.snapshot.clone();
        neurons.sort_by_key(|n| n.uid);
        let s = RegistrationSnapshotV2 { chain: chain(), block: a.at.0, block_hash: Digest(a.at.1), owner_hotkey: AccountId(owner), neurons };
        let row = match &target_rec {
            Some(t) => slop_ninja_protocol::QuantizedResult::of_package(&package)
                .and_then(|r| slop_ninja_protocol::reward_v1::derive_row(t, Some(&r), &s)),
            None => slop_ninja_protocol::reward_v1::derive_row(&CalculationTargetV2::launch_burn(chain()), None, &s),
        }
        .unwrap_or_else(|e| fail(format!("row: {e}")));
        report["row"] = json!({"entries": row.entries.iter().map(|e| json!([e.uid, e.weight])).collect::<Vec<_>>(),
            "digest": row.digest().map(|d| d.to_string()).unwrap_or_default(),
            "calculation_target": row.calculation_target.to_string()});
    }
    // The bundles, for a cohort target that activated at its closure.
    if let (Some(seed), Some(t), Some(c)) = (&a.seed, &target_rec, &closure) {
        let k = authorization_key(seed, root);
        let issued = a.issued.unwrap_or(c.closed_epoch);
        let mut carrier_chunks = Vec::new();
        let mut carrier_refs = Value::Null;
        let (carrier_manifest, carrier_package, carrier) = match &a.carrier {
            Some(d2) => {
                let m2 = Signed::<CohortManifestV2>::open(&read(d2, "manifest.signed"), keys).unwrap_or_else(|e| fail(format!("carrier manifest: {e}")));
                let p2 = Signed::<ScorePackageV2>::open(&read(d2, "package.signed"), keys).unwrap_or_else(|e| fail(format!("carrier package: {e}")));
                let i2 = Signed::<ReceiptIndex>::open(&read(d2, "receipt-index.signed"), keys).unwrap_or_else(|e| fail(format!("carrier index: {e}")));
                carrier_chunks = chunks_of(d2, &m2.cohort_label, &i2).0;
                carrier_refs = receipt_refs(d2, &m2.cohort_label);
                (
                    m2,
                    p2,
                    Some(target::CarrierHex {
                        manifest: hex::encode(read(d2, "manifest.signed")),
                        package: hex::encode(read(d2, "package.signed")),
                        receipt_index: Some(hex::encode(read(d2, "receipt-index.signed"))),
                        published_contents: hex::encode(read(d2, "published-contents.bin")),
                        fingerprint_index: hex::encode(read(d2, "fingerprint-index.bin")),
                    }),
                )
            }
            None => (manifest.clone(), package.clone(), None),
        };
        let checked = c.check(&manifest, &package, Some(t)).unwrap_or_else(|e| fail(format!("closure check: {e}")));
        let held = slop_ninja_protocol::ActiveTarget::activated(&checked).unwrap_or_else(|e| fail(e));
        let auth = RewardIntentAuthorization {
            chain: chain(),
            cohort: carrier_manifest.cohort,
            cohort_manifest: carrier_manifest.digest().unwrap_or_else(|e| fail(e)),
            score_package: carrier_package.digest().unwrap_or_else(|e| fail(e)),
            target: t.digest().unwrap_or_else(|e| fail(e)),
            checkpoint_generation: a.generation,
            issued_epoch: issued,
            renew_by_epoch: issued + u64::from(carrier_manifest.schedule.lease_tempos),
        };
        let signed = sign(auth, &k).and_then(|s| s.to_bytes()).unwrap_or_else(|e| fail(e));
        let source = target::SourceHex {
            manifest: hex::encode(read(&dir, "manifest.signed")),
            package: hex::encode(read(&dir, "package.signed")),
            target: hex::encode(read(&dir, "target.signed")),
            closure: hex::encode(read(&dir, "closure.signed")),
            receipt_index: Some(hex::encode(read(&dir, "receipt-index.signed"))),
            published_contents: hex::encode(read(&dir, "published-contents.bin")),
            fingerprint_index: hex::encode(read(&dir, "fingerprint-index.bin")),
        };
        let mut bundle = target::BundleV2 {
            schema: target::BUNDLE_SCHEMA_V2.into(),
            shortcut: "T12".into(),
            source: Some(source),
            authorization: target::AuthorizationHex { kind: "cohort".into(), authorization: hex::encode(&signed), renewal: None, carrier },
            receipt_chunks: chunks.iter().map(hex::encode).collect(),
            carrier_receipt_chunks: carrier_chunks.iter().map(hex::encode).collect(),
        };
        let full = serde_json::to_vec_pretty(&bundle).unwrap_or_else(|_| fail("bundle json"));
        // The validator form is a file (the native validator reads up to its
        // own file bound); only the coordinator form must fit one stored value.
        let reparsed: target::BundleV2 = serde_json::from_slice(&full).unwrap_or_else(|_| fail("bundle json"));
        let v = target::verify_v2(&reparsed, keys, &TESTNET_GENESIS.0, 601, &held)
            .unwrap_or_else(|r| fail(format!("{}: {}", r.code, r.detail)));
        std::fs::write(dir.join("reward-v1-bundle.json"), &full).unwrap_or_else(|_| fail("write bundle"));
        // Bundle schema v3: the same records, receipts by reference.
        let mut v3 = serde_json::to_value(&bundle).unwrap_or_else(|_| fail("bundle json"));
        v3["schema"] = json!(BUNDLE_SCHEMA_V3);
        let o = v3.as_object_mut().unwrap_or_else(|| fail("bundle json"));
        o.remove("receipt_chunks");
        o.remove("carrier_receipt_chunks");
        o.insert("receipts".into(), receipt_refs(&dir, &manifest.cohort_label));
        if !carrier_refs.is_null() {
            o.insert("carrier_receipts".into(), carrier_refs);
        }
        if let Some(s) = v3["source"].as_object_mut() {
            s.remove("receipt_index");
        }
        if let Some(c) = v3["authorization"]["carrier"].as_object_mut() {
            c.remove("receipt_index");
        }
        let v3 = serde_json::to_vec_pretty(&v3).unwrap_or_else(|_| fail("bundle json"));
        std::fs::write(dir.join("reward-v1-bundle-v3.json"), &v3).unwrap_or_else(|_| fail("write bundle"));
        bundle.receipt_chunks.clear();
        bundle.carrier_receipt_chunks.clear();
        if let Some(s) = bundle.source.as_mut() {
            s.receipt_index = None;
        }
        if let Some(c) = bundle.authorization.carrier.as_mut() {
            c.receipt_index = None;
        }
        let small = serde_json::to_vec(&bundle).unwrap_or_else(|_| fail("bundle json"));
        target::verify_v2(&target::parse_v2(&small).unwrap_or_else(|r| fail(r.detail)), keys, &TESTNET_GENESIS.0, 601, &held)
            .unwrap_or_else(|r| fail(format!("{}: {}", r.code, r.detail)));
        std::fs::write(dir.join("reward-v1-bundle-coordinator.json"), &small).unwrap_or_else(|_| fail("write bundle"));
        let terms = v.checked_terms(keys).unwrap_or_else(|e| fail(e));
        report["bundles"] = json!({"validator": "reward-v1-bundle.json", "validator_sha256": hex::encode(Sha256::digest(&full)),
            "validator_bytes": full.len(),
            "validator_fits_v2": full.len() as u64 <= slop_ninja_protocol::target_bundle::VALIDATOR_BUNDLE_BYTES,
            "validator_v3": "reward-v1-bundle-v3.json", "validator_v3_sha256": hex::encode(Sha256::digest(&v3)),
            "validator_v3_bytes": v3.len(), "coordinator": "reward-v1-bundle-coordinator.json",
            "coordinator_sha256": hex::encode(Sha256::digest(&small)), "coordinator_bytes": small.len(),
            "authorization": terms.digest.to_string(), "issued_epoch": terms.issued_epoch,
            "renew_by": terms.renew_by_epoch, "activated_epoch": c.closed_epoch});
    }
    println!("{}", serde_json::to_string_pretty(&report).unwrap_or_default());
}

/// The native validator's bundle schema v3 (receipts by reference).
const BUNDLE_SCHEMA_V3: &str = "slop_ninja.reward_v1_bundle/v3";

/// A file in DIR as a v3 object reference: absolute path, SHA-256, length.
fn object_ref(dir: &Path, rel: &str) -> Value {
    let path = std::fs::canonicalize(dir.join(rel)).unwrap_or_else(|e| fail(format!("{rel}: {e}")));
    let b = std::fs::read(&path).unwrap_or_else(|e| fail(format!("{rel}: {e}")));
    json!({"path": path.to_str().unwrap_or_else(|| fail("object path is not UTF-8")),
        "sha256": hex::encode(Sha256::digest(&b)), "bytes": b.len()})
}

/// A cohort's receipt index and every chunk its chunk map names (checked by
/// [`chunks_of`] already), as v3 references.
fn receipt_refs(dir: &Path, label: &str) -> Value {
    let map: Value = serde_json::from_slice(&read(dir, "chunk-map.json")).unwrap_or_else(|_| fail("chunk map"));
    let prefix = format!("cohorts/{label}/");
    let chunks: Vec<Value> = map["objects"]
        .as_array()
        .unwrap_or_else(|| fail("chunk objects"))
        .iter()
        .map(|o| {
            let k = o["key"].as_str().unwrap_or_else(|| fail("chunk key"));
            object_ref(dir, k.strip_prefix(&prefix).unwrap_or_else(|| fail("chunk outside the cohort prefix")))
        })
        .collect();
    json!({"index": object_ref(dir, "receipt-index.signed"), "chunks": chunks})
}

/// Signs the revocation of `--bundle`'s authorization (module docs).
fn revoke(a: &Args, keys: &TrustedKeys, root: &[u8; 32]) {
    let k = authorization_key(a.seed.as_deref().unwrap_or_else(|| fail("--authorization-seed")), root);
    let issued = a.issued.unwrap_or_else(|| fail("--issued-epoch"));
    let path = a.bundle.clone().unwrap_or_else(|| fail("--bundle"));
    let b: target::BundleV2 = serde_json::from_slice(&std::fs::read(&path).unwrap_or_else(|_| fail("read bundle")))
        .unwrap_or_else(|_| fail("bundle json"));
    let bytes = hex::decode(&b.authorization.authorization).unwrap_or_else(|_| fail("authorization hex"));
    let (auth_chain, target, digest, auth_issued, generation) = match b.authorization.kind.as_str() {
        "launch" => {
            let r = Signed::<LaunchAuthorization>::open(&bytes, keys).unwrap_or_else(|e| fail(format!("authorization: {e}")));
            (r.chain.clone(), r.target, r.digest().unwrap_or_else(|e| fail(e)), r.issued_epoch, r.checkpoint_generation)
        }
        "cohort" => {
            let r = Signed::<RewardIntentAuthorization>::open(&bytes, keys)
                .unwrap_or_else(|e| fail(format!("authorization: {e}")));
            (r.chain.clone(), r.target, r.digest().unwrap_or_else(|e| fail(e)), r.issued_epoch, r.checkpoint_generation)
        }
        other => fail(format!("unknown authorization kind {other}")),
    };
    if auth_chain != chain() {
        fail("the bundle's authorization names another chain or subnet");
    }
    if issued < auth_issued {
        fail(format!("--issued-epoch precedes the authorization's issue (epoch {auth_issued})"));
    }
    let finalized = a.finalized.unwrap_or_else(|| fail("--finalized-epoch"));
    if issued > finalized.saturating_add(1) {
        fail(format!("--issued-epoch {issued} is past the finalized epoch index {finalized} plus one"));
    }
    let revocation = RewardIntentRevocation {
        chain: chain(),
        target,
        authorization: digest,
        checkpoint_generation: a.generation.max(generation),
        issued_epoch: issued,
    };
    let signed = sign(revocation, &k).unwrap_or_else(|e| fail(e));
    let checked = RewardIntentRevocation::check(&signed, keys, &chain()).unwrap_or_else(|e| fail(e));
    let out_bytes = signed.to_bytes().unwrap_or_else(|e| fail(e));
    let out = a.out.clone().unwrap_or_else(|| fail("--out"));
    std::fs::write(&out, &out_bytes).unwrap_or_else(|_| fail("write revocation"));
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({"signed": true, "revocation": checked.digest().to_string(),
            "target": target.to_string(), "authorization": digest.to_string(), "issued_epoch": issued,
            "withdraws": "every authorization of the target issued at or before issued_epoch",
            "out": out.display().to_string(), "sha256": hex::encode(Sha256::digest(&out_bytes))}))
        .unwrap_or_default()
    );
}

/// Every chunk object of a cohort's receipt index, checked against its chunk
/// map (digest and length), with their total bytes.
fn chunks_of(dir: &Path, label: &str, index: &ReceiptIndex) -> (Vec<Vec<u8>>, u64) {
    let map: Value = serde_json::from_slice(&read(dir, "chunk-map.json")).unwrap_or_else(|_| fail("chunk map"));
    if map["receipt_root"] != index.digest().map(|d| d.to_string()).unwrap_or_default() {
        fail("chunk map root");
    }
    let prefix = format!("cohorts/{label}/");
    let mut chunks = Vec::new();
    let mut chunk_bytes = 0u64;
    for o in map["objects"].as_array().unwrap_or_else(|| fail("chunk objects")) {
        let k = o["key"].as_str().unwrap_or_else(|| fail("chunk key"));
        let rel = k.strip_prefix(&prefix).unwrap_or_else(|| fail("chunk outside the cohort prefix"));
        let b = read(dir, rel);
        if hex::encode(Sha256::digest(&b)) != o["sha256"].as_str().unwrap_or_default() || Some(b.len() as u64) != o["bytes"].as_u64() {
            fail(format!("{rel} digest or length"));
        }
        chunk_bytes += b.len() as u64;
        chunks.push(b);
    }
    (chunks, chunk_bytes)
}
