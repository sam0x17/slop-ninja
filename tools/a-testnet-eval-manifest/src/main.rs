//! slop_ninja_a_testnet_eval_manifest build --repo ROOT --seed-hex HEX64 --cohort N \
//!     --per-stratum K [--max-per-source-group M] [--allowlist FILE] --out data/.../manifest.json
//!
//! slop_ninja_a_testnet_eval_manifest pool --repo ROOT --pool-id ID --out-dir data/.../DIR \
//!     [--shard-rows N] [--allowlist FILE]
//!
//! slop_ninja_a_testnet_eval_manifest stock-v2 --repo ROOT --pool-id ID --out-dir data/.../DIR \
//!     --n N --mix H/M/S --reuse ledger:K:G|engine-v1 --probe-docs P --horizon C \
//!     --report-horizons C1,C2,... --engine-mix H/M/S --roster R --cohorts-per-day D \
//!     --probe-versions once|per-reweight [--canary-report DOCS:AUDITS:E1,E2,...] \
//!     [--plan-canary DOCS:AUDITS:EVERY] [--allowlist FILE]
//!
//! slop_ninja_a_testnet_eval_manifest replay-v1 --repo ROOT [--pool-dir data/.../DIR] [--record FILE]
//!
//! slop_ninja_a_testnet_eval_manifest carve-v3 --repo ROOT --pool-dir data/.../POOL --key-file data/.../K_test2 \
//!     --key-commitment HEX --freeze-log data/.../FROZEN-A2.json --protocol-sums data/.../splits-v4/SHA256SUMS:HEX \
//!     --out-dir data/.../DIR [--allowlist FILE]
//!
//! slop_ninja_a_testnet_eval_manifest carve-v3-apply --repo ROOT --draft data/.../CARVE-A2.draft.json \
//!     --draft-sha256 HEX --freeze-log data/.../FROZEN-A2.json --allowlist-out data/.../FILE \
//!     --pool-out data/.../DIR --record-out data/.../CARVE-A2.json [--pool-id ID]
//!
//! slop_ninja_a_testnet_eval_manifest carve-v3-release --repo ROOT --record data/.../CARVE-A2.json \
//!     --record-sha256 HEX --allowlist data/.../allowlist-v3.json:HEX --pool-dir data/.../POOL \
//!     --test2-t data/.../FILE:HEX --mock-used data/.../FILE:HEX --freeze-log data/.../FROZEN-A2.json \
//!     --out-dir data/.../DIR --pool-out data/.../DIR [--pool-id ID]
//!
//! slop_ninja_a_testnet_eval_manifest carve-v3-log-check --repo ROOT --freeze-log data/.../FROZEN-A2.json
//!
//! Testnet-only. Without `--allowlist` or `--record`, the tracked files are used.
//! Prints counts only: no text, ids or seed.
use anyhow::{Context, Result, bail, ensure};
use slop_ninja_a_testnet_eval_manifest::capacity::{Canary, Params, Reuse};
use slop_ninja_a_testnet_eval_manifest::pool_v2::{self, BuildParams, CanaryReport};
use slop_ninja_a_testnet_eval_manifest::replay::{self, Record, TRACKED_RECORD};
use slop_ninja_a_testnet_eval_manifest::stock::TRACKED_STOCK_ALLOWLIST;
use slop_ninja_a_testnet_eval_manifest::{Selection, TRACKED_ALLOWLIST, build, export_human_pool, write_manifest};
use std::collections::BTreeMap;
use std::path::PathBuf;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("carve-v3") => return carve_v3(&args[1..]),
        Some("carve-v3-apply") => return carve_v3_apply(&args[1..]),
        Some("carve-v3-release") => return carve_v3_release(&args[1..]),
        Some("carve-v3-log-check") => return carve_v3_log_check(&args[1..]),
        Some("pool") => return pool(&args[1..]),
        Some("stock-v2") => return stock_v2(&args[1..]),
        Some("replay-v1") => return replay_v1(&args[1..]),
        Some("pool-public-candidate") => return pool_pc(&args[1..]),
        _ => {}
    }
    ensure!(args.first().map(String::as_str) == Some("build"), "usage: see the crate README");
    let (mut repo, mut seed, mut cohort, mut per, mut group, mut allow, mut out) = (None, None, None, None, 1, None, None);
    let mut it = args[1..].iter();
    while let Some(a) = it.next() {
        let v = it.next().with_context(|| format!("{a} needs a value"))?;
        match a.as_str() {
            "--repo" => repo = Some(PathBuf::from(v)),
            "--seed-hex" => {
                let b = hex::decode(v).context("--seed-hex")?;
                seed = Some(<[u8; 32]>::try_from(b.as_slice()).ok().context("--seed-hex needs 32 bytes")?);
            }
            "--cohort" => cohort = Some(v.parse()?),
            "--per-stratum" => per = Some(v.parse()?),
            "--max-per-source-group" => group = v.parse()?,
            "--allowlist" => allow = Some(PathBuf::from(v)),
            "--out" => out = Some(PathBuf::from(v)),
            _ => bail!("unknown argument {a}"),
        }
    }
    let repo = repo.context("--repo")?;
    let sel = Selection {
        seed: seed.context("--seed-hex")?,
        cohort: cohort.context("--cohort")?,
        per_stratum: per.context("--per-stratum")?,
        max_per_source_group: group,
    };
    let allow = match allow {
        Some(p) => std::fs::read(p).context("--allowlist")?,
        None => TRACKED_ALLOWLIST.as_bytes().to_vec(),
    };
    let m = build(&repo, &allow, &sel)?;
    let path = write_manifest(&repo, &out.context("--out")?, &m)?;
    println!("wrote {}", path.display());
    println!("selected {} rows: {}", m["counts"]["selected"], m["counts"]["by_origin"]);
    for s in m["strata"].as_array().into_iter().flatten() {
        println!("  {} {}: {} of {} (shortfall {})", s["domain"], s["origin"], s["selected"], s["available"], s["shortfall"]);
    }
    println!("rows_sha256 {}", m["rows_sha256"]);
    Ok(())
}

/// `pool-public-candidate --texts T --records R --splits S --pool-id ID --out-dir DIR
/// [--shard-rows N] [--max-tokens N]`: the decoded public-candidate tables.
fn pool_pc(args: &[String]) -> Result<()> {
    use slop_ninja_a_testnet_eval_manifest::public_candidate;
    let (mut t, mut r, mut s, mut id, mut out, mut shard, mut max) = (None, None, None, None, None, 64usize, 1000u64);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let v = it.next().with_context(|| format!("{a} needs a value"))?;
        match a.as_str() {
            "--texts" => t = Some(PathBuf::from(v)),
            "--records" => r = Some(PathBuf::from(v)),
            "--splits" => s = Some(PathBuf::from(v)),
            "--pool-id" => id = Some(v.clone()),
            "--out-dir" => out = Some(PathBuf::from(v)),
            "--shard-rows" => shard = v.parse()?,
            "--max-tokens" => max = v.parse()?,
            _ => bail!("unknown argument {a}"),
        }
    }
    let (t, r, s) = (t.context("--texts")?, r.context("--records")?, s.context("--splits")?);
    let digest = |p: &PathBuf| -> Result<String> {
        Ok(slop_ninja_a_testnet_eval_manifest::sha256_hex(&std::fs::read(p)?))
    };
    let inputs = serde_json::json!({"texts_sha256": digest(&t)?, "records_sha256": digest(&r)?, "splits_sha256": digest(&s)?});
    let (rows, excluded) = public_candidate::select(&t, &r, &s, max)?;
    let m = public_candidate::export(&rows, &excluded, &id.context("--pool-id")?, shard, max, inputs, &out.context("--out-dir")?)?;
    println!("pool {}: {} rows in {} shards, {} source groups", m["pool_id"], m["counts"]["rows"],
        m["shards"].as_array().map_or(0, Vec::len), m["counts"]["source_groups"]);
    println!("by domain {}", m["counts"]["by_domain"]);
    println!("excluded {}", m["selection"]["excluded"]);
    Ok(())
}

fn pool(args: &[String]) -> Result<()> {
    let (mut repo, mut id, mut out, mut shard, mut allow) = (None, None, None, 64usize, None);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let v = it.next().with_context(|| format!("{a} needs a value"))?;
        match a.as_str() {
            "--repo" => repo = Some(PathBuf::from(v)),
            "--pool-id" => id = Some(v.clone()),
            "--out-dir" => out = Some(PathBuf::from(v)),
            "--shard-rows" => shard = v.parse()?,
            "--allowlist" => allow = Some(PathBuf::from(v)),
            _ => bail!("unknown argument {a}"),
        }
    }
    let allow = match allow {
        Some(p) => std::fs::read(p).context("--allowlist")?,
        None => TRACKED_ALLOWLIST.as_bytes().to_vec(),
    };
    let m = export_human_pool(&repo.context("--repo")?, &allow, &id.context("--pool-id")?, shard, &out.context("--out-dir")?)?;
    println!("pool {}: {} rows in {} shards, {} source groups", m["pool_id"], m["counts"]["rows"],
        m["shards"].as_array().map_or(0, Vec::len), m["counts"]["source_groups"]);
    println!("by domain {}", m["counts"]["by_domain"]);
    Ok(())
}

fn parse_mix(v: &str) -> Result<[u32; 3]> {
    let parts: Vec<u32> = v.split('/').map(str::parse).collect::<Result<_, _>>().context("mix H/M/S")?;
    <[u32; 3]>::try_from(parts.as_slice()).ok().context("mix needs three parts H/M/S")
}

fn parse_reuse(v: &str) -> Result<Reuse> {
    if v == "engine-v1" {
        return Ok(Reuse::EngineV1);
    }
    let rest = v.strip_prefix("ledger:").context("--reuse ledger:K:G or engine-v1")?;
    let (k, g) = rest.split_once(':').context("--reuse ledger:K:G")?;
    Ok(Reuse::Ledger { max_uses: k.parse()?, min_gap: g.parse()? })
}

/// `DOCS:AUDITS:E1,E2,...`
fn parse_canary(v: &str) -> Result<(u32, u32, Vec<u32>)> {
    let mut it = v.splitn(3, ':');
    let docs = it.next().context("canary DOCS")?.parse().context("canary DOCS")?;
    let audits = it.next().context("canary AUDITS")?.parse().context("canary AUDITS")?;
    let every: Vec<u32> = it.next().context("canary intervals")?.split(',').map(str::parse).collect::<Result<_, _>>().context("canary intervals")?;
    ensure!(!every.is_empty(), "canary intervals");
    Ok((docs, audits, every))
}

fn stock_v2(args: &[String]) -> Result<()> {
    let mut kv: BTreeMap<String, String> = BTreeMap::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let v = it.next().with_context(|| format!("{a} needs a value"))?;
        ensure!(
            matches!(a.as_str(), "--repo" | "--pool-id" | "--out-dir" | "--n" | "--mix" | "--reuse" | "--probe-docs"
                | "--horizon" | "--report-horizons" | "--engine-mix" | "--roster" | "--cohorts-per-day" | "--allowlist"
                | "--probe-versions" | "--canary-report" | "--plan-canary"),
            "unknown argument {a}"
        );
        ensure!(kv.insert(a.clone(), v.clone()).is_none(), "{a} given twice");
    }
    let get = |k: &str| kv.get(k).with_context(|| format!("{k} is required"));
    let horizons: Vec<u32> =
        get("--report-horizons")?.split(',').map(str::parse).collect::<Result<_, _>>().context("--report-horizons")?;
    let mut plan = Params::new(get("--n")?.parse()?, parse_mix(get("--mix")?)?, parse_reuse(get("--reuse")?)?, get("--probe-docs")?.parse()?);
    plan.probe_redraw_on_reweight = match get("--probe-versions")?.as_str() {
        "once" => false,
        "per-reweight" => true,
        v => bail!("--probe-versions once|per-reweight, not {v}"),
    };
    if let Some(v) = kv.get("--plan-canary") {
        let (docs, audits, every) = parse_canary(v)?;
        ensure!(every.len() == 1, "--plan-canary takes one interval");
        plan.canary = Some(Canary { docs, audits, every: every[0] });
    }
    let canary_report = match kv.get("--canary-report") {
        Some(v) => {
            let (docs, flagged_audits, every) = parse_canary(v)?;
            Some(CanaryReport { docs, flagged_audits, every })
        }
        None => None,
    };
    let bp = BuildParams {
        pool_id: get("--pool-id")?.clone(),
        horizon: get("--horizon")?.parse()?,
        report_horizons: horizons,
        plan,
        engine_mix: parse_mix(get("--engine-mix")?)?,
        roster: get("--roster")?.parse()?,
        cohorts_per_day: get("--cohorts-per-day")?.parse()?,
        canary_report,
    };
    let allow = match kv.get("--allowlist") {
        Some(p) => std::fs::read(p).context("--allowlist")?,
        None => TRACKED_STOCK_ALLOWLIST.as_bytes().to_vec(),
    };
    let repo = PathBuf::from(get("--repo")?);
    let m = pool_v2::build(&repo, &allow, &bp, &PathBuf::from(get("--out-dir")?))?;
    println!("pool {} ({}): {} groups, {} excluded by the gate (verdict {})", m["pool_id"], m["stage"],
        m["groups"]["rows"], m["exclusions"]["rows"], m["gate"]["verdict"]);
    println!("capacity documents {} (lower bound {}, expected {})", m["capacity"]["capacity_documents"],
        m["capacity"]["capacity_lower_documents"], m["capacity"]["expected_documents"]);
    println!("manifest weights (horizon {} cohorts): days before the first bind {} (at the lower yield {}), with reweighting {}",
        m["domain_weights"]["horizon_cohorts"], m["capacity"]["days_before_first_bind"],
        m["capacity"]["days_before_first_bind_lower_yield"], m["capacity"]["days_with_reweighting"]);
    println!("estimand change (TV, ppm) {}; retired {}; reduced {}", m["domain_weights"]["estimand_change_tv_ppm"],
        m["domain_weights"]["retired"], m["domain_weights"]["reduced"]);
    Ok(())
}

fn replay_v1(args: &[String]) -> Result<()> {
    let (mut repo, mut dir, mut record) = (None, None, None);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let v = it.next().with_context(|| format!("{a} needs a value"))?;
        match a.as_str() {
            "--repo" => repo = Some(PathBuf::from(v)),
            "--pool-dir" => dir = Some(v.clone()),
            "--record" => record = Some(PathBuf::from(v)),
            _ => bail!("unknown argument {a}"),
        }
    }
    let bytes = match record {
        Some(p) => std::fs::read(p).context("--record")?,
        None => TRACKED_RECORD.as_bytes().to_vec(),
    };
    let r = Record::parse(&bytes)?;
    let counts = replay::replay_counts(&r)?;
    println!("engine: the frozen copies are the files of coordinator commit {}", r.engine.commit);
    println!("count replay: {} cohorts match; {} rows used, {} remain", r.cohorts.len(), counts["used_rows"], counts["remaining_rows"]);
    if let Some(dir) = dir {
        let p = replay::replay_pool(&repo.context("--repo")?, &dir, &r)?;
        println!("engine replay: {} cohorts match on the pool manifest; {} rows used; {} of {} rows can parent",
            r.cohorts.len(), p["used_rows"], p["rows_with_two_sentences"], r.pool.rows);
        println!("{}", serde_json::to_string_pretty(&p["cohorts"])?);
    }
    Ok(())
}

/// `--flag value` pairs, each at most once, from an allowed set.
fn flags(args: &[String], allowed: &[&str]) -> Result<BTreeMap<String, String>> {
    let mut kv = BTreeMap::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let v = it.next().with_context(|| format!("{a} needs a value"))?;
        ensure!(allowed.contains(&a.as_str()), "unknown argument {a}");
        ensure!(kv.insert(a.clone(), v.clone()).is_none(), "{a} given twice");
    }
    Ok(kv)
}

/// `PATH:SHA256`.
fn file_ref(v: &str) -> Result<slop_ninja_a_testnet_eval_manifest::stock::FileRef> {
    let (path, sha256) = v.rsplit_once(':').context("expected PATH:SHA256")?;
    Ok(slop_ninja_a_testnet_eval_manifest::stock::FileRef { path: path.into(), sha256: sha256.into() })
}

fn carve_v3(args: &[String]) -> Result<()> {
    use slop_ninja_a_testnet_eval_manifest::carve_v3::{self, CarveParams, Sources};
    let kv = flags(args, &["--repo", "--pool-dir", "--key-file", "--key-commitment", "--freeze-log", "--protocol-sums", "--out-dir", "--allowlist"])?;
    let get = |k: &str| kv.get(k).with_context(|| format!("{k} is required"));
    let src = Sources {
        pool_dir: get("--pool-dir")?.clone(),
        allowlist_v2: kv.get("--allowlist").cloned().unwrap_or_else(|| "tracked".into()),
        key_file: get("--key-file")?.clone(),
        key_commitment: get("--key-commitment")?.clone(),
        protocol_sums: file_ref(get("--protocol-sums")?)?,
    };
    let (d, sha) = carve_v3::draft(&PathBuf::from(get("--repo")?), &src, &CarveParams::protocol_a2(), get("--freeze-log")?, &PathBuf::from(get("--out-dir")?))?;
    let r = &d["result"];
    println!("{} draft {sha}: verdict pass {}; {}", carve_v3::CARVE_RULE, r["verdict"]["pass"], r["verdict"]["reasons"]);
    println!("carve groups by domain and role {}", r["carve_groups"]["by_domain_role"]);
    println!("AR-8: pass {}, days before the first bind at the lower yield {}, newly retired {}",
        r["ar8"]["pass"], r["ar8"]["days_before_first_bind_lower_yield"], r["ar8"]["newly_retired"]);
    println!("TV from the manifest (ppm): {}", r["tv"]["full_carve_and_reserve"]);
    if !r["failure"].is_null() {
        println!("largest B_res' with AR-8 passing: {}; with every A8 criterion passing: {}",
            r["failure"]["largest_b_res_prime_with_ar8_passing"], r["failure"]["largest_b_res_prime_passing_a8"]);
    }
    Ok(())
}

fn carve_v3_apply(args: &[String]) -> Result<()> {
    use slop_ninja_a_testnet_eval_manifest::carve_v3::{self, ApplyArgs, CarveParams};
    let kv = flags(args, &["--repo", "--draft", "--draft-sha256", "--freeze-log", "--allowlist-out", "--pool-out", "--pool-id", "--record-out"])?;
    let get = |k: &str| kv.get(k).with_context(|| format!("{k} is required"));
    let (al, po, ro) = (PathBuf::from(get("--allowlist-out")?), PathBuf::from(get("--pool-out")?), PathBuf::from(get("--record-out")?));
    let a = ApplyArgs {
        draft: get("--draft")?,
        draft_sha256: get("--draft-sha256")?,
        freeze_log: get("--freeze-log")?,
        allowlist_out: &al,
        pool_out: &po,
        pool_id: kv.get("--pool-id").map(String::as_str),
        record_out: &ro,
    };
    let r = carve_v3::apply(&PathBuf::from(get("--repo")?), &a, &CarveParams::protocol_a2())?;
    println!("applied: record {} {}", r["written"]["path"], r["written"]["sha256"]);
    println!("pool {} rebuilt at W_A8: {} groups", r["application"]["pool"]["dir"], r["application"]["pool"]["groups"]["rows"]);
    println!("allow-list v3 {} {}", r["application"]["allowlist_v3"]["path"], r["application"]["allowlist_v3"]["sha256"]);
    Ok(())
}

fn carve_v3_release(args: &[String]) -> Result<()> {
    use slop_ninja_a_testnet_eval_manifest::carve_v3::{self, CarveParams, ReleaseArgs};
    let kv = flags(args, &["--repo", "--record", "--record-sha256", "--allowlist", "--pool-dir", "--test2-t", "--mock-used", "--freeze-log", "--out-dir", "--pool-out", "--pool-id"])?;
    let get = |k: &str| kv.get(k).with_context(|| format!("{k} is required"));
    let (allowlist, test2_t, used) = (file_ref(get("--allowlist")?)?, file_ref(get("--test2-t")?)?, file_ref(get("--mock-used")?)?);
    let (out, pool_out) = (PathBuf::from(get("--out-dir")?), PathBuf::from(get("--pool-out")?));
    let a = ReleaseArgs {
        record: get("--record")?,
        record_sha256: get("--record-sha256")?,
        allowlist: &allowlist,
        pool_dir: get("--pool-dir")?,
        test2_t: &test2_t,
        mock_used: &used,
        freeze_log: get("--freeze-log")?,
        out: &out,
        pool_out: &pool_out,
        pool_id: kv.get("--pool-id").map(String::as_str),
    };
    let r = carve_v3::release(&PathBuf::from(get("--repo")?), &a, &CarveParams::protocol_a2())?;
    println!("released {} reserve groups by domain {}; release record {}", r["record"]["result"]["released"]["rows"],
        r["record"]["result"]["released"]["by_domain"], r["release_sha256"]);
    println!("TV from W_A8 (ppm) {}; successor allow-list v3 {}; pool {} rebuilt: {} groups", r["record"]["result"]["tv_from_w_a8_ppm"],
        r["allowlist_v3_sha256"], r["pool"]["dir"], r["pool"]["groups"]["rows"]);
    Ok(())
}

fn carve_v3_log_check(args: &[String]) -> Result<()> {
    let kv = flags(args, &["--repo", "--freeze-log"])?;
    let get = |k: &str| kv.get(k).with_context(|| format!("{k} is required"));
    let r = slop_ninja_a_testnet_eval_manifest::carve_v3::log_check(&PathBuf::from(get("--repo")?), get("--freeze-log")?)?;
    println!("freeze log: {} entries, chained from entry {}, chain {}; sha256 {}", r["entries"], r["chained_from"], r["chain"], r["sha256"]);
    println!("A6 entry utc {}; user decisions {}; carve draft entries {}", r["a6_entry_utc"], r["user_decisions"], r["carve_draft_entries"]);
    if !r["a6_error"].is_null() {
        println!("A6: {}", r["a6_error"]);
    }
    Ok(())
}
