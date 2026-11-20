//! Synthetic input exporter for the deployed package-step qualification.
//! Generation and scoring use the existing native simulator; packaging does not.
//! No network, model, customer corpus or production signing key is used here.

use super::*;
use crate::sql::{Sql, SqlValue};
use crate::eval::package_fixture_import::{self, Chunk, Table};
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::Path;

#[path = "package_fixture_receipts.rs"]
mod fixture_receipts;
use fixture_receipts::fixture_index;

fn fixture_identity(index: u16) -> super::super::plan::IdentityJson {
    let mut id = super::super::plan::tests::identity(index, 0);
    id.hotkey = format!("0x{}", hex::encode(Sha256::digest(format!("package-fixture/{index}"))));
    id
}

fn fixture_artifact(sim: &mut Sim<'_>, index: u16) -> super::super::plan::ArtifactJson {
    let [lo, hi] = index.to_le_bytes();
    v3_artifact(sim, fixture_identity(index), [lo, hi, 3, 4, 5])
}

fn prediction(job: i64, kind: &str, sha: &str) -> (f64, f64) {
    let digest = Sha256::digest(format!("fixture-model/{job}/{sha}"));
    let noise = f64::from(u16::from_le_bytes([digest[0], digest[1]])) / 65535.0;
    let human = matches!(kind, "human_only" | "control");
    let confidence = if job < 2 {
        if human { 35.0 + noise * 5.0 } else { 60.0 + noise * 5.0 }
    } else if human {
        1.0 + noise * 20.0
    } else {
        75.0 + noise * 24.0
    };
    (confidence, confidence * 0.4)
}

fn stop_before_package(sim: &mut Sim<'_>, seq: i64) {
    sim.hold_package = true;
    for _ in 0..100_000 {
        sim.tick();
        assert_eq!(fault_of(sim.db, seq), None, "fixture fault");
        if sim.package_ready == Some(seq) {
            assert_eq!(phase(sim.db, seq), "packaging");
            assert!(sim.packages_v3.is_empty());
            return;
        }
    }
    panic!("fixture did not reach package step");
}

fn sql_value(value: &SqlValue) -> Value {
    match value {
        SqlValue::Null => Value::Null,
        SqlValue::Integer(n) => json!(n),
        SqlValue::Text(s) => json!(s),
    }
}

/// Stream bounded pages, preserving SQLite integer/text/null types. Table and
/// column names come only from this binary's schema, never external input.
fn export_sql(db: &Sqlite, out: &Path) -> Vec<Value> {
    fs::create_dir(out).unwrap();
    let tables = db.exec("SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name", &[]).unwrap();
    let mut index = Vec::new();
    for row in tables {
        let name = sql::text(&row, 0).unwrap();
        assert!(name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'));
        let columns: Vec<String> = db.exec(&format!("PRAGMA table_info(\"{name}\")"), &[]).unwrap()
            .iter().map(|r| sql::text(r, 1).unwrap().to_owned()).collect();
        let path = out.join(format!("{name}.jsonl"));
        let mut file = BufWriter::new(File::create(&path).unwrap());
        let mut count = 0_i64;
        let mut after = 0_i64;
        let mut chunks = Vec::new();
        fs::create_dir_all(out.join("parts").join(name)).unwrap();
        loop {
            let rows = db.exec(&format!("SELECT rowid, * FROM \"{name}\" WHERE rowid > ? ORDER BY rowid LIMIT 256"), &[after.into()]).unwrap();
            if rows.is_empty() { break; }
            let before = after;
            let mut bytes = Vec::new();
            for row in &rows {
                after = sql::int(row, 0).unwrap();
                serde_json::to_writer(&mut bytes, &row.iter().map(sql_value).collect::<Vec<_>>()).unwrap();
                writeln!(&mut bytes).unwrap();
            }
            file.write_all(&bytes).unwrap();
            let part = format!("parts/{name}/{:08}.jsonl", chunks.len());
            fs::write(out.join(&part), &bytes).unwrap();
            chunks.push(Chunk { file: format!("sql/{part}"), sha256: lane_logic::sha_hex(&bytes), bytes: bytes.len(),
                offset: count as u64, rows: rows.len() as u64, after_rowid: before, last_rowid: after });
            count += rows.len() as i64;
        }
        file.flush().unwrap();
        index.push(json!({"table": name, "columns": columns, "rowid_included": true, "rows": count,
            "file": format!("sql/{name}.jsonl"), "sha256": hash_file(&path), "chunks": chunks}));
    }
    index
}

fn hash_file(path: &Path) -> String {
    use std::io::Read;
    let mut file = File::open(path).unwrap();
    let mut hash = Sha256::new();
    let mut buf = [0; 65536];
    loop {
        let n = file.read(&mut buf).unwrap();
        if n == 0 { break; }
        hash.update(&buf[..n]);
    }
    hex::encode(hash.finalize())
}

fn export_objects(objects: &BTreeMap<String, Vec<u8>>, root: &Path, bucket: &str) -> Vec<Value> {
    fs::create_dir(root.join(bucket)).unwrap();
    objects.iter().enumerate().map(|(i, (key, bytes))| {
        let file = format!("{bucket}/{i:08}.bin");
        fs::write(root.join(&file), bytes).unwrap();
        json!({"bucket": bucket, "key": key, "file": file, "bytes": bytes.len(), "sha256": lane_logic::sha_hex(bytes)})
    }).collect()
}

fn restore_sql(root: &Path, tables: &[Value], baseline: &[Value]) -> Sqlite {
    let db = Sqlite::open();
    sql::migrate(&db, &EVAL_COHORT_SCHEMA).unwrap();
    let typed = |v: &[Value]| v.iter().map(|t| serde_json::from_value::<Table>(t.clone()).unwrap()).collect::<Vec<_>>();
    tx(&db, |db| package_fixture_import::clear_pristine(db, &typed(baseline), &typed(tables))).unwrap();
    for table in tables {
        let table: Table = serde_json::from_value(table.clone()).unwrap();
        package_fixture_import::check_table(&db, &table).unwrap();
        assert_eq!(hash_file(&root.join(&table.file)), table.sha256);
        for (part, chunk) in table.chunks.iter().enumerate() {
            let bytes = fs::read(root.join(&chunk.file)).unwrap();
            tx(&db, |db| package_fixture_import::import_chunk(db, &table, part, &bytes)).unwrap();
            // An uncertain response can retry the exact same chunk safely.
            tx(&db, |db| package_fixture_import::import_chunk(db, &table, part, &bytes)).unwrap();
        }
        assert_eq!(db.count(&table.table) as u64, table.rows);
        package_fixture_import::verify_table(&db, &table).unwrap();
    }
    db
}

/// Expected output bytes for a later deployment comparison, evaluated from
/// restored files and tables. This is a native oracle using the production
/// protocol, not a measurement of the Worker's package method.
fn expected_package(db: &Sqlite, cfg: &EvalConfig, seq: i64, now: u64, root: &Path, objects: &[Value]) -> Value {
    use slop_ninja_protocol::{Signed, SigningKey};
    let load = |bucket: &str, key: &str| {
        let object = objects.iter().find(|o| o["bucket"] == bucket && o["key"] == key).unwrap();
        let bytes = fs::read(root.join(object["file"].as_str().unwrap())).unwrap();
        assert_eq!(lane_logic::sha_hex(&bytes), object["sha256"]);
        assert_eq!(bytes.len() as u64, object["bytes"].as_u64().unwrap());
        bytes
    };
    let cohort = engine::cohort(db, seq).unwrap();
    let summaries: Vec<engine::JobSummary> = db.exec(
        "SELECT summary_key, summary_sha256 FROM jobs WHERE seq = ? ORDER BY job", &[seq.into()]
    ).unwrap().iter().map(|r| {
        let bytes = load("eval", sql::text(r, 0).unwrap());
        assert_eq!(lane_logic::sha_hex(&bytes), sql::text(r, 1).unwrap());
        serde_json::from_slice(&bytes).unwrap()
    }).collect();
    let (index, chunk_map) = fixture_index(db, seq);
    let st = v3::state(db, seq).unwrap();
    let sealed = v3::index_at_seal(&st).unwrap();
    let (_, keys) = v3::published_index(db).unwrap();
    let mut fps = BTreeMap::new();
    for fp in &sealed.fingerprints.entries {
        if !cohort.plan.reward_v1.as_ref().unwrap().probes.iter().any(|p| p.version == fp.version) { continue; }
        let vector = v3::vector_from_bytes(&load("eval", &keys[&(fp.content, fp.version)])).unwrap();
        assert_eq!(v3::vector_digest(&vector), fp.fingerprint);
        fps.insert((fp.content, fp.version), vector);
    }
    let b = tx(db, |s| v3::prepare_package(s, cfg, &cohort, &summaries, &fps, index.digest(), index.count())).unwrap();
    assert!(b.package.entrants.iter().all(|e| e.gate_failures.is_empty()), "fixture must exercise every entrant through the cluster walk");
    assert!(b.package.entrants.iter().all(|e| e.cluster_head == Some(e.identity.hotkey)), "fixture must retain every independent head");
    b.package.check_manifest(&b.manifest).unwrap();
    index.check_package(&b.package);
    b.result.check_manifest(&b.manifest).unwrap();
    assert_eq!(b.result.digest().unwrap(), b.package.result);
    slop_ninja_protocol::reward_v1::check_draws(&b.package, &engine::cohort_seed(&cfg.private_seed, &cohort.label)).unwrap();
    if let Some(target) = &b.target {
        target.check_package(&b.package).unwrap();
        target.check_result(&b.result).unwrap();
    }
    let chunks: Vec<Vec<u8>> = chunk_map["objects"].as_array().unwrap().iter()
        .map(|o| load("receipts", o["key"].as_str().unwrap())).collect();
    let decoded = index.verify_objects(&chunks);
    assert_eq!(decoded, index.count());
    let pk = slop_ninja_protocol::fixtures::package_key();
    fn signed<R: Record + Clone>(record: &R, key: &SigningKey) -> Vec<u8> {
        let bytes = super::super::package::signed_bytes(record.clone(), key).unwrap();
        let parsed = Signed::<R>::from_bytes(&bytes).unwrap();
        let verified = parsed.verify(&slop_ninja_protocol::fixtures::trusted_keys()).unwrap();
        assert_eq!(verified.to_bytes().unwrap(), record.to_bytes().unwrap());
        let mut corrupt = bytes.clone();
        *corrupt.last_mut().unwrap() ^= 1;
        assert!(Signed::<R>::from_bytes(&corrupt).and_then(|s| s.verify(&slop_ninja_protocol::fixtures::trusted_keys()).map(|_| ())).is_err());
        bytes
    }
    let mut output: BTreeMap<String, Vec<u8>> = BTreeMap::from([
        ("receipt-index.signed".into(), index.signed.clone()),
        ("package.signed".into(), signed(&b.package, &pk)),
        ("result.signed".into(), signed(&b.result, &pk)),
        ("chunk-map.json".into(), serde_json::to_vec_pretty(&chunk_map).unwrap()),
        ("components.json".into(), serde_json::to_vec_pretty(&b.components).unwrap()),
    ]);
    if cohort.plan.receipt_index_format.is_some() {
        let (limits, _) = v3::limits_record(&cohort.plan).unwrap();
        output.insert(crate::eval::receipt_format::OBJECT.into(), serde_json::to_vec(&limits).unwrap());
    }
    if let Some(target) = &b.target { output.insert("target.signed".into(), signed(target, &pk)); }
    if let Some(localization) = &b.localization { output.insert("localization.signed".into(), signed(localization, &pk)); }
    if let Some(feedback) = &b.feedback { output.insert("gate-feedback.signed".into(), signed(feedback, &pk)); }
    if let Some(report) = &b.live_privacy { output.insert(slop_ninja_protocol::live_privacy::OBJECT.into(), report.to_bytes()); }
    output.insert("keys.json".into(), serde_json::to_vec_pretty(&json!({
        "instance": engine::INSTANCE_MAIN, "package_public_key": hex::encode(pk.verifying_key().to_bytes()),
        "receipt_public_key": hex::encode(slop_ninja_protocol::fixtures::receipt_key().verifying_key().to_bytes()), "package_digest": b.package.digest().unwrap().to_string(),
        "result_digest": b.package.result.to_string(), "target_digest": b.target.as_ref().map(|t| t.digest().unwrap().to_string()),
    })).unwrap());
    let puts = (output.len() + b.vectors.len() + index.objects.len()) as u64;
    let prefix = engine::receipt_prefix(&cohort.label);
    let mut output: BTreeMap<String, Vec<u8>> = output.into_iter().map(|(k, v)| (format!("{prefix}/{k}"), v)).collect();
    output.extend(index.objects.iter().map(|(k, v)| (k.clone(), v.clone())));
    let mut expected = export_objects(&output, root, "expected-receipts");
    expected.extend(export_objects(&b.vectors.iter().cloned().collect(), root, "expected-eval"));
    tx(db, |s| v3::commit_package(s, seq, &b, puts, now)).unwrap();
    assert_eq!(engine::cohort(db, seq).unwrap().phase, "transition");
    let expected = json!({"objects": expected, "puts": puts, "phase": "transition", "native_package_verified": true,
        "package_digest": b.package.digest().unwrap().to_string(), "result_digest": b.result.digest().unwrap().to_string(),
        "target_digest": b.target.as_ref().map(|t| t.digest().unwrap().to_string()),
        "receipt_root": index.digest().to_string(),
        "receipts_verified": decoded, "entrants": b.package.entrants.len(),
        "all_entrants_gated": true, "independent_heads": b.package.entrants.len(),
        "cluster_pairs_required": b.package.entrants.len() * b.package.entrants.len().saturating_sub(1) / 2,
        "target_present": b.target.is_some(), "package_public_key": hex::encode(pk.verifying_key().to_bytes()),
        "receipt_public_key": hex::encode(slop_ninja_protocol::fixtures::receipt_key().verifying_key().to_bytes()),
        "limit": "Native expected bytes from shared production protocol. No Worker execution or deployed timing; optional records only covered when present."});
    package_fixture_import::verify_package_commit(db, seq, &expected).unwrap();
    for field in ["package_digest", "result_digest", "target_digest", "receipt_root", "receipts_verified"] {
        let mut changed = expected.clone();
        changed[field] = json!("wrong");
        assert!(package_fixture_import::verify_package_commit(db, seq, &changed).is_err(), "{field}");
    }
    db.exec("UPDATE cohorts SET phase = 'packaging' WHERE seq = ?", &[seq.into()]).unwrap();
    assert!(package_fixture_import::verify_package_commit(db, seq, &expected).is_err());
    db.exec("UPDATE cohorts SET phase = 'transition' WHERE seq = ?", &[seq.into()]).unwrap();
    // Identical readback is safe after a lost reconciliation response.
    for _ in 0..2 { package_fixture_import::verify_package_commit(db, seq, &expected).unwrap(); }
    expected
}

/// Explicit invocation only. The full fixture needs substantial disk/RAM and
/// does not belong in the normal test sweep. The caller owns resource limits.
#[test]
#[ignore]
fn export_synthetic_package_fixture() {
    let out = std::env::var("SLOP_NINJA_PACKAGE_FIXTURE_OUT").expect("explicit output directory");
    let root = Path::new(&out);
    assert!(!root.exists(), "never overwrite a fixture");
    let full = match std::env::var("SLOP_NINJA_PACKAGE_FIXTURE_SIZE").as_deref() {
        Ok("small") => false,
        Ok("full") => true,
        _ => panic!("choose small or full explicitly"),
    };
    let optional = match std::env::var("SLOP_NINJA_PACKAGE_FIXTURE_OPTIONS").as_deref() {
        Err(std::env::VarError::NotPresent) | Ok("base") => false,
        Ok("span-feedback") => true,
        _ => panic!("choose base or span-feedback explicitly"),
    };
    let (entrants, n, probe_n, audits) = if full { (256_u16, 4000, 1000, 800) } else { (4, 40, 20, 8) };
    let db = Sqlite::open();
    sql::migrate(&db, &EVAL_COHORT_SCHEMA).unwrap();
    let rules = crate::config::RecordRules::parse(Some("reward-v3,quant-v2")).unwrap();
    let config = EvalConfig { ceilings: Ceilings { roster_cap: Some(usize::from(entrants)), accept_reward_v0: false },
        authority: crate::config::Authority::default().with_rules(rules), ..cfg() };
    let mut sim = Sim::new(&db, config, 2_000_000);
    (sim.pool, sim.texts) = pool_of(if full { 16000 } else { 160 });
    let reference = fixture_artifact(&mut sim, 0);
    let predecessor = fixture_artifact(&mut sim, 1);
    let mut probe = plan_v3(1, "testnet-probe-v1", probe_n, reference.clone());
    probe.previous_reference = Some(predecessor.clone());
    probe.schema = super::super::plan::PROBE_PLAN_SCHEMA.into();
    probe.reward_v1.as_mut().unwrap().probe_version = Some(1);
    probe.reward_v1.as_mut().unwrap().canary.draw_humans = 0;
    plan_record_rules(&mut probe, rules);
    sim.predict_doc = Some(Box::new(prediction));
    let pseq = open_cohort_with_config(&db, &sim.cfg, &probe, 10, sim.now);
    assert_eq!(sim.run(pseq, 20_000), "closed");
    let record = sim.eval["probes/v1/record.json"].clone();
    let mut plan = plan_v3(2, "synthetic-package-fixture", n, reference);
    match std::env::var("SLOP_NINJA_PACKAGE_FIXTURE_RECEIPTS").as_deref() {
        Ok("chunked-v2") => {
            plan.receipt_index_format = Some(crate::eval::receipt_format::ReceiptIndexFormat::ChunkedV2);
            let score = &mut plan.fanout.as_mut().unwrap().score;
            score.artifact_max_mib = None;
            score.artifact_spec = Some("slop_ninja_a_artifact_spec_v3".into());
            score.artifact_spec_sha256 = Some(slop_ninja_artifact_check::spec::Version::V3.spec_sha256());
            score.load_ms_per_mib = Some(100);
        }
        Err(std::env::VarError::NotPresent) | Ok("legacy-v1") => {},
        _ => panic!("choose legacy-v1 or chunked-v2 receipts explicitly"),
    }
    plan.previous_reference = Some(predecessor);
    plan.reward_v1.as_mut().unwrap().probes = vec![super::super::plan::ProbeRef {
        version: 1, size: probe_n, key: "probes/v1/record.json".into(), sha256: lane_logic::sha_hex(&record),
    }];
    plan.reward_v1.as_mut().unwrap().canary.draw_humans = audits;
    plan_record_rules(&mut plan, rules);
    if full {
        let mainnet = slop_ninja_protocol::fixtures_quant2::MAINNET_QUANT2;
        let quant = plan.reward_v1.as_mut().unwrap().quant.as_mut().unwrap();
        quant.identify_bits = mainnet.identify_bits;
        quant.budget_factor = mainnet.budget_factor;
        quant.min_success_max_failed = mainnet.min_success_max_failed;
        quant.headroom = Some(super::super::plan::HeadroomPlan {
            source: super::super::plan::HEADROOM_FIXED.into(), initial_ppb: mainnet.headroom_pub_ppb,
        });
        quant.headroom_basis = Some("synthetic full-size qualification: golden mainnet headroom".into());
    }
    plan.scorer.chunk_docs = 32;
    plan.fanout.as_mut().unwrap().score.lane_docs = 128;
    plan.entrants = (2..entrants + 2).map(|i| fixture_artifact(&mut sim, i)).collect();
    plan.allow_list = plan.entrants.iter().map(|a| a.owner.clone()).collect();
    if optional {
        let anchor = fixture_artifact(&mut sim, entrants + 2);
        let v = plan.reward_v1.as_mut().unwrap();
        v.gates.share_mae_max_ppb = 300_000_000;
        v.localization = Some(span_block(anchor));
        v.feedback = Some(super::super::plan::FeedbackPlan { rule: slop_ninja_protocol::gate_feedback::FEEDBACK_RULE.into() });
        sim.span_facts = Some(Box::new(|_| Some(facts(true, Some(24)))));
        sim.spans = Some(Box::new(|_, kind, text, gold| localizing(1, kind, text, gold)));
    }
    if full {
        // Admission prices every scored document and planned cold lane. The
        // simulator's small-fixture allowance does not cover this roster.
        let score_docs = u32::try_from(plan.scored_docs()).unwrap();
        let fanout = plan.fanout().unwrap();
        let lanes = super::super::cost::plan_score_lanes(
            u32::try_from(plan.jobs()).unwrap(), plan.n, plan.scorer.chunk_docs,
            fanout.score.lane_docs, fanout.score.max_lanes.min(sim.cfg.max_score_lanes),
        );
        let upper = super::super::cost::scoring_upper_bound_nanousd(
            1, score_docs, u32::try_from(lanes.len()).unwrap(), &fanout.cost,
        );
        let budget = super::super::cost::nano_to_micro_ceil(upper);
        plan.fanout.as_mut().unwrap().score.scoring_budget_microusd = budget;
    }
    let seq = open_cohort_with_config(&db, &sim.cfg, &plan, 20, sim.now);
    stop_before_package(&mut sim, seq);
    let jobs = db.exec("SELECT job, role, tasks, summary_count, summary_key FROM jobs WHERE seq = ? ORDER BY job", &[seq.into()]).unwrap();
    assert_eq!(jobs.len(), usize::from(entrants) + 2 + usize::from(optional));
    for job in &jobs { assert_eq!(sql::int(job, 2).unwrap(), sql::int(job, 3).unwrap()); }
    let (index, chunk_map) = fixture_index(&db, seq);
    assert!(index.count() >= u64::from(n) * (u64::from(entrants) + 2));
    // Exercise implicit row-ID gaps. The source-group table's logical keys
    // and values are unchanged; a restore that renumbers it must fail.
    db.exec("UPDATE pool_groups_used SET rowid = rowid + 10000000", &[]).unwrap();
    assert!(sql::int(&db.exec("SELECT MIN(rowid) FROM pool_groups_used", &[]).unwrap()[0], 0).unwrap() > 10000000);
    fs::create_dir(root).unwrap();
    let pristine = Sqlite::open();
    sql::migrate(&pristine, &EVAL_COHORT_SCHEMA).unwrap();
    let baseline = export_sql(&pristine, &root.join("sql-baseline"));
    let tables = export_sql(&db, &root.join("sql"));
    let mut objects = export_objects(&sim.eval, root, "eval");
    objects.extend(export_objects(&sim.rcpt, root, "receipts"));
    let restored = restore_sql(root, &tables, &baseline);
    let restored_tables = export_sql(&restored, &root.join("sql-restored"));
    assert_eq!(tables, restored_tables, "every restored SQL value and type must match");
    fs::remove_dir_all(root.join("sql-restored")).unwrap();
    let (restored_index, restored_chunks) = fixture_index(&restored, seq);
    assert_eq!(index.record_bytes(), restored_index.record_bytes());
    assert_eq!(chunk_map, restored_chunks);
    assert_eq!(serde_json::to_value(engine::cohort(&db, seq).unwrap().plan).unwrap(),
        serde_json::to_value(engine::cohort(&restored, seq).unwrap().plan).unwrap());
    for object in &objects {
        assert_eq!(hash_file(&root.join(object["file"].as_str().unwrap())), object["sha256"].as_str().unwrap());
    }
    let expected = expected_package(&restored, &sim.cfg, seq, sim.now, root, &objects);
    if optional {
        let expected_objects = expected["objects"].as_array().unwrap();
        for suffix in ["/localization.signed", "/gate-feedback.signed"] {
            assert!(expected_objects.iter().any(|o| o["key"].as_str().unwrap().ends_with(suffix)), "missing {suffix}");
        }
        let component = expected_objects.iter().find(|o| o["key"].as_str().unwrap().ends_with("/components.json")).unwrap();
        let components: Value = serde_json::from_slice(&fs::read(root.join(component["file"].as_str().unwrap())).unwrap()).unwrap();
        assert_eq!(components["localization"]["state"], "applied");
        assert_eq!(components["localization"]["waiver"], Value::Null);
        assert!(components["feedback"]["bits"].as_u64().unwrap() > 0);
    }
    let manifest = json!({
        "schema": if plan.receipt_index_format.is_some() { "slop_ninja_synthetic_package_fixture_v6" } else { "slop_ninja_synthetic_package_fixture_v5" }, "synthetic_only": true,
        "seq": seq, "now": sim.now, "plan": plan, "entrants": entrants, "batch": n,
        "options": if optional { "span-feedback" } else { "base" },
        "probe_size": probe_n, "audit_a": audits, "audit_b": plan.audit_b_humans(),
        "receipt_count": index.count(), "receipt_root": index.digest().to_string(),
        "jobs": jobs.iter().map(|r| r.iter().map(sql_value).collect::<Vec<_>>()).collect::<Vec<_>>(),
        "tables": tables, "baseline_tables": baseline, "objects": objects, "expected": expected, "snapshot_roundtrip_verified": true,
        "rowids_preserved": true,
        "implicit_rowid_gap_control": true,
        "chunk_import_replay_verified": true,
        "configuration": {"private_seed": hex::encode(sim.cfg.private_seed), "instance": sim.cfg.instance,
            "chain": {"genesis": sim.cfg.chain.genesis.to_string(), "netuid": sim.cfg.chain.netuid, "mechanism": sim.cfg.chain.mechanism},
            "authority": sim.cfg.authority.name(), "rules": sim.cfg.authority.rules().name(),
            "max_score_lanes": sim.cfg.max_score_lanes, "roster_cap": sim.cfg.ceilings.roster_cap,
            "accept_reward_v0": sim.cfg.ceilings.accept_reward_v0, "exposed_pools": sim.cfg.exposed_pools,
            "scorer_binary": sim.cfg.scorer_binary},
        "worker_package_executed": false, "full_allowance_qualified": false,
        "limit": "Probe/audit sizes are explicit fixture parameters; compare with the approved qualification workload before full timing. No deployment or performance claim."
    });
    fs::write(root.join("FIXTURE.json"), serde_json::to_vec_pretty(&manifest).unwrap()).unwrap();
    export_deployment_manifest(root, manifest);
}

fn export_deployment_manifest(root: &Path, mut manifest: Value) {
    use super::super::package_fixture_metadata;
    fs::create_dir(root.join("metadata")).unwrap();
    let mut save = |key: &str, bytes: &[u8]| {
        let path = root.join(key);
        if path.exists() {
            assert_eq!(fs::read(path).unwrap(), bytes);
        } else {
            fs::write(path, bytes).unwrap();
        }
    };
    for table in manifest["tables"].as_array_mut().unwrap() {
        let chunks = table["chunks"].as_array_mut().unwrap();
        let list = package_fixture_metadata::write(chunks, &mut save);
        chunks.clear();
        table["chunk_index"] = json!(list);
    }
    for pointer in ["/objects", "/expected/objects"] {
        let values = manifest.pointer_mut(pointer).unwrap();
        *values = json!(package_fixture_metadata::write(values.as_array().unwrap(), &mut save));
    }
    manifest["receipt_format"] = json!(manifest["plan"]["receipt_index_format"].as_str().unwrap_or("legacy-v1"));
    manifest["plan_sha256"] = json!(lane_logic::sha_hex(&serde_json::to_vec(&manifest["plan"]).unwrap()));
    let fields = manifest.as_object_mut().unwrap();
    fields.remove("plan");
    fields.remove("jobs");
    fields.insert("schema".into(), json!("slop_ninja_synthetic_package_fixture_v7"));
    let bytes = serde_json::to_vec(&manifest).unwrap();
    assert!(bytes.len() <= 1024 * 1024, "deployment fixture manifest exceeds importer bound");
    fs::write(root.join("DEPLOY.json"), bytes).unwrap();
}
