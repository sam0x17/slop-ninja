//! Focused tests of B10's second part, on synthetic fixtures (no corpus text,
//! no model or detector call). Each rule is pinned against a value computed
//! here from the fixture files, independently of the evaluator's own outputs.

use super::part2::*;
use super::tests::*;
use super::*;
use crate::comp_eval::{bootstrap_draws, group_values};
use crate::pangram_subsample::bootstrap_interval;

/// A labelled edit of a JSON record, and a synthetic involvement per (seed, document index).
type Edit = Box<dyn Fn(&mut Value)>;
type InvFn<'a> = &'a dyn Fn(u64, usize) -> f64;

/// Per-row involvement from a predictions file by the bound rule (worst case 1 − y).
fn invs(path: &Path, rows: &[Value]) -> Vec<f64> {
    let p = pred_map(path);
    rows.iter()
        .map(|r| {
            let y = if r["origin"] == "human_only" { 0.0 } else { 1.0 };
            match p.get(r["id"].as_str().unwrap()) {
                Some(v) if v.get("status").is_none() => (f(&v["p_model_only"]) + f(&v["p_mixed"])).min(1.0),
                _ => 1.0 - y,
            }
        })
        .collect()
}

/// Per-row failure (failed or missing) from a predictions file.
fn fails(path: &Path, rows: &[Value]) -> Vec<bool> {
    let p = pred_map(path);
    rows.iter()
        .map(|r| p.get(r["id"].as_str().unwrap()).is_none_or(|v| v.get("status").is_some()))
        .collect()
}

fn pred_map(path: &Path) -> BTreeMap<String, Value> {
    let mut p: BTreeMap<String, Value> = BTreeMap::new();
    for l in std::fs::read_to_string(path).unwrap().lines() {
        let v: Value = serde_json::from_str(l).unwrap();
        p.insert(v["id"].as_str().unwrap().to_string(), v);
    }
    p
}

fn read_rows(p: &Path) -> Vec<Value> {
    std::fs::read_to_string(p).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect()
}

fn pred_for(system: &str, seed: u64, r: &Value) -> Option<Value> {
    // A twin scores its original's prediction moved by a fixed shift per group and system.
    if let Some(of) = r["probe_of"].as_str() {
        let mut o = r.clone();
        o["id"] = json!(of);
        let base = pred_line(system, seed, &o)?;
        if base.get("status").is_some() {
            return Some(json!({"id": r["id"], "status": "failed", "failure": "timeout"}));
        }
        let shift = match (r["form_inject"]["group"].as_str(), system) {
            (Some("h_inj_c"), "Cand.u") => 0.004,
            (Some("h_inj_l"), "Cand.u") => -0.06 * unif(&format!("{system}:{seed}:{of}")),
            _ => 0.001,
        };
        let inv = (1.0 - f(&base["p_human"]) + shift).clamp(0.0, 1.0);
        let pm = inv * 0.6;
        return Some(json!({"id": r["id"], "p_human": 1.0 - inv, "p_model_only": pm, "p_mixed": inv - pm, "share": pm}));
    }
    pred_line(system, seed, r)
}

fn write_preds(d: &Path, tag: &str, name: &str, seed: u64, rows: &[Value]) -> PathBuf {
    let p = d.join(format!("pred-{tag}-{name}-{seed}.jsonl"));
    let lines: Vec<Value> = rows.iter().filter_map(|r| pred_for(name, seed, r)).collect();
    write_lines(&p, &lines);
    p
}

#[allow(clippy::too_many_arguments)]
fn add_system(cfg: &mut Value, d: &Path, pop: &str, tag: &str, name: &str, seeds: &[u64], rows: &[Value], ops: bool) {
    for &s in seeds {
        let pp = write_preds(d, tag, name, s, rows);
        let mut v = json!({"name": name, "seed": s, "population": pop, "predictions": pp});
        if ops {
            v["ops"] = json!(d.join(format!("ops-{name}-{s}.json")));
        }
        cfg["systems"].as_array_mut().unwrap().push(v);
    }
}

/// B3's R-form rows of an N-form population: humans unchanged, raw twins
/// `rform:<id>` with `twin_of` (one positive, `skip`, left without a twin),
/// and one screen-fail document.
fn r_rows_of(n: &[Value], skip: &str, split: &str, group: &str, block: &str) -> Vec<Value> {
    let mut v = vec![];
    for r in n {
        let id = r["id"].as_str().unwrap().to_string();
        if r["origin"] == "human_only" {
            v.push(r.clone());
            continue;
        }
        if id == skip {
            continue;
        }
        let mut t = r.clone();
        t["id"] = json!(format!("rform:{id}"));
        t["twin_of"] = json!(id);
        t["stratum"] = json!("raw_twin");
        v.push(t);
    }
    let mut sf = row(&format!("rform_screen_fail_v1:item-{block}"), split, "mixed", group, n[0]["domain"].as_str().unwrap(), "primary");
    sf["block"] = json!(block);
    sf["stratum"] = json!("rform_screen_fail_v1");
    v.push(sf);
    v
}

fn r_rows_b3() -> Vec<Value> {
    r_rows_of(&t_rows("N"), "tN-5-0", T_SPLIT, "tg2", "T")
}

/// T.N's rows with an MWL field on humans.
fn n_rows_mwl() -> Vec<Value> {
    let mut v = t_rows("N");
    for r in &mut v {
        if r["origin"] == "human_only" {
            let id = r["id"].as_str().unwrap();
            let dom = if r["domain"] == "wikipedia" { 4.6 } else { 3.9 };
            r["mwl"] = json!(dom + unif(&format!("mwl:{id}")));
        }
    }
    v
}

/// C.N's rows with a crawl year on humans.
fn c_rows_year() -> Vec<Value> {
    let mut v = c_rows();
    for r in &mut v {
        if r["origin"] == "human_only" {
            let id = r["id"].as_str().unwrap();
            r["crawl_year"] = json!(2013 + (unif(&format!("y:{id}")) * 10.0) as u64);
        }
    }
    v
}

/// B14's twins of T.N's humans for one group, with their manifest.
fn twins(d: &Path, kind: &str, split: &str) -> (Vec<Value>, PathBuf) {
    let mut rows = vec![];
    let mut man = vec![];
    for r in t_rows("N").into_iter().filter(|r| r["origin"] == "human_only") {
        let id = r["id"].as_str().unwrap().to_string();
        let mut t = r.clone();
        t["id"] = json!(format!("probe:{kind}:{id}"));
        t["probe_of"] = json!(id);
        t["split"] = json!(split);
        let lf = unif(&format!("lf:{id}")) < 0.4;
        t["form_inject"] = json!({"schema": "slop_ninja_a_form_inject_v1", "group": kind,
            "layout": if kind == "h_inj_l" { json!({"line_form": lf}) } else { Value::Null }});
        man.push(json!({"schema": PROBE_SCHEMA, "probe_id": t["id"], "base_id": id, "split": split, "kind": kind, "origin": "human_only"}));
        rows.push(t);
    }
    let mp = d.join(format!("manifest-{kind}.jsonl"));
    write_lines(&mp, &man);
    (rows, mp)
}

/// The base fixture with part 2's populations, systems, margins and gates.
fn fixture2() -> Fixture {
    let fx = fixture();
    let d = fx.dir.path().to_path_buf();
    let mut cfg = fx.cfg.clone();
    write_lines(&d.join("tn.jsonl"), &n_rows_mwl());
    let rr = r_rows_b3();
    write_lines(&d.join("tr.jsonl"), &rr);
    write_lines(&d.join("cn.jsonl"), &c_rows_year());
    // T.R's predictions follow its new rows.
    for s in cfg["systems"].as_array().unwrap().clone() {
        if s["population"] == "T.R" {
            let lines: Vec<Value> = rr
                .iter()
                .filter_map(|r| pred_for(s["name"].as_str().unwrap(), s["seed"].as_u64().unwrap(), r))
                .collect();
            write_lines(Path::new(s["predictions"].as_str().unwrap()), &lines);
        }
    }
    add_system(&mut cfg, &d, "T.R", "tr", "Cand.u", &RECIPE_SEEDS, &rr, false);
    add_system(&mut cfg, &d, "T.R", "tr", "R0.u", &[17], &rr, false);
    for (pid, kind, tag) in [("T.HC", "h_inj_c", "hc"), ("T.HL", "h_inj_l", "hl")] {
        let (rows, mp) = twins(&d, kind, T_SPLIT);
        let p = d.join(format!("{tag}.jsonl"));
        write_lines(&p, &rows);
        cfg["populations"].as_array_mut().unwrap().push(json!({"id": pid, "block": "T", "split": T_SPLIT, "rows": p,
            "weights": {"rule": "balanced"}, "probe": {"kind": kind, "of": "T.N", "manifest": mp}}));
        add_system(&mut cfg, &d, pid, tag, "Cand.u", &RECIPE_SEEDS, &rows, false);
        add_system(&mut cfg, &d, pid, tag, "R0.u", &[17], &rows, false);
        add_system(&mut cfg, &d, pid, tag, "Cand.p", &RECIPE_SEEDS, &rows, true);
    }
    let m = cfg["margins"].as_array_mut().unwrap();
    m.push(json!({"id": "m_slope", "kind": "reseed_sd", "statistic": "mwl_slope", "field": "mwl", "population": "T.N", "system": "Faux.u"}));
    m.push(json!({"id": "m_era", "kind": "reseed_sd", "statistic": "era_abs_slope", "field": "crawl_year", "population": "C.N", "system": "Recal.m"}));
    let g = cfg["gates"].as_array_mut().unwrap();
    for x in [
        json!({"kind": "shift", "id": "T3f.C", "population": "T.HC", "system": "Cand.u", "levels": ["artifact", "recipe"], "margin": {"margin": "m_H.T"}}),
        json!({"kind": "shift", "id": "T3f.L", "population": "T.HL", "system": "Cand.u", "levels": ["artifact", "recipe"], "margin": {"margin": "m_H.T"}, "two_sided": true}),
        json!({"kind": "shift", "id": "T3f.R0", "population": "T.HC", "system": "R0.u", "levels": ["artifact"]}),
        json!({"kind": "cp_flags", "id": "T3g.C", "population": "T.HC", "system": "Cand.p", "point": "0.01", "levels": ["artifact", "recipe"]}),
        json!({"kind": "fs", "id": "T5b.T", "pairs": [{"n_population": "T.N", "r_population": "T.R"}], "candidate": "Cand.u", "comparator": "R0.u",
               "levels": ["artifact", "recipe"], "margin": {"value": 0.0}, "strict": true}),
        json!({"kind": "fri", "id": "FRI.T", "n_population": "T.N", "r_population": "T.R", "system": "Cand.p"}),
        json!({"kind": "slope", "id": "T3e.T", "population": "T.N", "field": "mwl", "candidate": "Cand.u", "comparator": "R0.u",
               "comparator_recipe": "Faux.u", "levels": ["artifact", "recipe"], "margin": {"margin": "m_slope"}}),
        json!({"kind": "era_slope", "id": "T3h", "population": "C.N", "field": "crawl_year", "candidate": "Cand.m", "comparator": "Recal.m",
               "levels": ["artifact"], "margin": {"margin": "m_era"}}),
        json!({"kind": "t8b", "id": "T8b", "population": "mock", "candidate": "Cand.t", "comparator": "R0.t", "levels": ["artifact"], "by": ["operation"]}),
    ] {
        g.push(x);
    }
    cfg["measures"] = json!([
        {"kind": "t8a_s", "id": "S3-1", "population": "mock", "candidate": "Cand.t", "comparator": "R0.t"},
        {"kind": "slice_sd", "id": "S3-4", "population": "T.N", "candidate": "Cand.p", "comparator": "R0.p"}
    ]);
    Fixture { dir: fx.dir, cfg }
}

fn sys_path(cfg: &Value, pop: &str, name: &str, seed: u64) -> PathBuf {
    let s = cfg["systems"].as_array().unwrap().iter().find(|s| s["population"] == pop && s["name"] == name && s["seed"] == seed).unwrap();
    PathBuf::from(s["predictions"].as_str().unwrap())
}

fn loss_vec(path: &Path, rows: &[Value]) -> Vec<f64> {
    rows.iter()
        .zip(invs(path, rows))
        .map(|(r, i)| {
            let y = if r["origin"] == "human_only" { 0.0 } else { 1.0 };
            (i - y) * (i - y)
        })
        .collect()
}

fn parsed(rows: &[Value]) -> Vec<Row> {
    parse_rows(&rows.iter().map(|r| r.to_string()).collect::<Vec<_>>().join("\n")).unwrap()
}

/// v1.2 balanced weights written out here: each source group equal mass,
/// each (group, cell) within it equal mass, each document within a cell equal.
fn balanced_here(keys: &[(String, String)]) -> Vec<f64> {
    let mut cells: BTreeMap<(&str, &str), usize> = BTreeMap::new();
    for (g, c) in keys {
        *cells.entry((g.as_str(), c.as_str())).or_default() += 1;
    }
    let mut per: BTreeMap<&str, usize> = BTreeMap::new();
    for (g, _) in cells.keys() {
        *per.entry(g).or_default() += 1;
    }
    let groups = per.len() as f64;
    keys.iter().map(|(g, c)| 1.0 / (groups * per[g.as_str()] as f64 * cells[&(g.as_str(), c.as_str())] as f64)).collect()
}

fn cell_key(r: &Value) -> String {
    let o = r["origin"].as_str().unwrap();
    if o == "human_only" { o.into() } else { format!("{o}|{}", r["share"]) }
}

#[test]
fn r2_eval2_end_to_end_part2() {
    let fx = fixture2();
    let out = run_value(&fx, &fx.cfg, "p2").unwrap();
    assert_eq!(out["per_seed_equality"]["pass"], true, "{}", out["per_seed_equality"]);
    for id in ["T3f.C", "T3f.L", "T3g.C", "T5b.T", "T3e.T", "T3h"] {
        let s = &gate(&out, id)["status"];
        assert!(s == "pass" || s == "fail", "{id}: {}", gate(&out, id));
    }
    for id in ["T3f.R0", "FRI.T", "T8b"] {
        assert_eq!(gate(&out, id)["status"], "reported", "{id}");
    }
    assert_eq!(out["populations"]["T.HC"]["probe"]["manifest_lines"], 48);
    assert_eq!(out["measures"].as_array().unwrap().len(), 2);
    assert_eq!(out["readings"].as_array().unwrap().len(), READINGS.len());
    assert_eq!(out["pending"].as_array().unwrap().len(), PENDING.len());
    // Margins are written before any candidate output is read, and the new ones come from comparators only.
    let m: Value = serde_json::from_slice(&std::fs::read(fx.dir.path().join("p2-margins.json")).unwrap()).unwrap();
    assert!(m["margins"]["m_slope"]["fault"].is_null() && m["margins"]["m_era"]["fault"].is_null());
    assert_eq!(m["margins"]["m_slope"]["field"], "mwl");
}

#[test]
fn r2_eval2_shift_equals_s2_h1_pairing() {
    // T3f: twin − original human-only Brier with the original's S2 H1 weight and group, computed here.
    let fx = fixture2();
    let out = run_value(&fx, &fx.cfg, "sh").unwrap();
    let d = fx.dir.path();
    let n = read_rows(&d.join("tn.jsonl"));
    let rows = parsed(&n);
    let humans: Vec<usize> = (0..rows.len()).filter(|&i| rows[i].primary && rows[i].is_human()).collect();
    let w = balanced_weights(&rows, &humans);
    let hrows: Vec<Value> = humans.iter().map(|&i| n[i].clone()).collect();
    for (gid, pid) in [("T3f.C", "T.HC"), ("T3f.L", "T.HL")] {
        let tw = read_rows(&d.join(if pid == "T.HC" { "hc.jsonl" } else { "hl.jsonl" }));
        let lt = loss_vec(&sys_path(&fx.cfg, pid, "Cand.u", 17), &tw);
        let lo = loss_vec(&sys_path(&fx.cfg, "T.N", "Cand.u", 17), &hrows);
        let dl: Vec<f64> = lt.iter().zip(&lo).map(|(a, b)| a - b).collect();
        let v = group_values(&rows, &humans, &humans, &w, &dl);
        let (lo95, hi95) = bootstrap_interval(&v).unwrap();
        let g = &gate(&out, gid)["levels"]["artifact"];
        assert_eq!(f(&g["point"]).to_bits(), (v.iter().sum::<f64>() / v.len() as f64).to_bits(), "{gid}");
        assert_eq!(f(&g["lower"]).to_bits(), lo95.to_bits(), "{gid}");
        assert_eq!(f(&g["upper"]).to_bits(), hi95.to_bits(), "{gid}");
        // The margin is m_H at the recipe level and sqrt(2) m_H at the artifact level.
        let r = &gate(&out, gid)["levels"];
        assert_eq!(f(&r["artifact"]["margin"]).to_bits(), (std::f64::consts::SQRT_2 * f(&r["recipe"]["margin"])).to_bits());
        let m = f(&g["margin"]);
        let two = gid == "T3f.L";
        assert_eq!(g["pass"], json!(hi95 <= m && (!two || lo95 >= -m)), "{gid}");
    }
    // The L twins' line-form strata, computed here: each over its twins, with balanced weights
    // recomputed over their originals.
    let tw = read_rows(&d.join("hl.jsonl"));
    let lt = loss_vec(&sys_path(&fx.cfg, "T.HL", "Cand.u", 17), &tw);
    let lo = loss_vec(&sys_path(&fx.cfg, "T.N", "Cand.u", 17), &hrows);
    let st = &gate(&out, "T3f.L")["levels"]["artifact"]["strata"];
    let mut total = 0;
    for (label, want) in [("line_form", true), ("not_line_form", false)] {
        let ks: Vec<usize> = (0..tw.len()).filter(|&k| tw[k]["form_inject"]["layout"]["line_form"] == want).collect();
        let idx: Vec<usize> = ks.iter().map(|&k| humans[k]).collect();
        let wl = balanced_weights(&rows, &idx);
        let dl: Vec<f64> = ks.iter().map(|&k| lt[k] - lo[k]).collect();
        let v = group_values(&rows, &idx, &idx, &wl, &dl);
        assert_eq!(st[label]["twins"], ks.len(), "{label}");
        assert!((f(&st[label]["point"]) - v.iter().sum::<f64>() / v.len() as f64).abs() < 1e-15, "{label}");
        total += ks.len();
    }
    assert_eq!(total, 48);
    assert!(gate(&out, "T3f.C")["levels"]["artifact"]["strata"].is_null());
    // The two-sided rule.
    assert!(shift_pass(-0.01, 0.01, 0.01, true));
    assert!(!shift_pass(-0.0101, 0.0, 0.01, true));
    assert!(shift_pass(-0.0101, 0.0, 0.01, false));
    assert!(!shift_pass(0.0, 0.0101, 0.01, false));
}

#[test]
fn r2_eval2_twin_population_refusals() {
    let fx = fixture2();
    let d = fx.dir.path();
    let (rows, mp) = twins(d, "h_inj_c", T_SPLIT);
    // A manifest missing one twin.
    let man: Vec<Value> = read_rows(&mp);
    write_lines(&mp, &man[1..]);
    assert!(err(run_value(&fx, &fx.cfg, "m1")).contains("manifest's (probe_id, base_id) pairs"));
    write_lines(&mp, &man);
    // A twin of a human the original population does not hold.
    let mut r2 = rows.clone();
    r2[0]["probe_of"] = json!("th-99-0");
    r2[0]["id"] = json!("probe:h_inj_c:th-99-0");
    let mut man2 = man.clone();
    man2[0]["probe_id"] = r2[0]["id"].clone();
    man2[0]["base_id"] = json!("th-99-0");
    write_lines(&d.join("hc.jsonl"), &r2);
    write_lines(&mp, &man2);
    // (Refused before any prediction is read, so the stale twin predictions are never reached.)
    let e = err(run_value(&fx, &fx.cfg, "m2"));
    assert!(e.contains("primary humans, one each"), "{e}");
    // A twin id that is not B14's.
    let mut r3 = rows.clone();
    r3[1]["id"] = json!("probe:h_inj_l:x");
    write_lines(&d.join("hc.jsonl"), &r3);
    write_lines(&mp, &man);
    assert!(err(run_value(&fx, &fx.cfg, "m3")).contains("is not probe:h_inj_c:<probe_of>"));
    // A twin whose form_inject record is of the other group.
    let mut r5 = rows.clone();
    r5[3]["form_inject"]["group"] = json!("h_inj_l");
    write_lines(&d.join("hc.jsonl"), &r5);
    write_lines(&mp, &man);
    assert!(err(run_value(&fx, &fx.cfg, "m5")).contains("form_inject group is not h_inj_c"));
    // A manifest line of another split is refused from its bytes.
    write_lines(&d.join("hc.jsonl"), &rows);
    let mut man4 = man.clone();
    man4[2]["split"] = json!("dev_select");
    write_lines(&mp, &man4);
    assert!(err(run_value(&fx, &fx.cfg, "m4")).contains("refused split (dev_select)"));
    // Twins of the same block in another split (an open split that holds T rows) are refused.
    let (rows6, mp6) = twins(d, "h_inj_c", "span_select2");
    write_lines(&d.join("hc.jsonl"), &rows6);
    let mut cfg = fx.cfg.clone();
    let k = cfg["populations"].as_array().unwrap().iter().position(|p| p["id"] == "T.HC").unwrap();
    cfg["populations"][k]["split"] = json!("span_select2");
    cfg["populations"][k]["probe"]["manifest"] = json!(mp6);
    let e = err(run_value(&fx, &cfg, "m6"));
    assert!(e.contains("block and split must be T.N's"), "{e}");
}

/// The FS pairs of fixture2's T.N/T.R, computed here: (N row, R row).
fn fs_pairs_here(n: &[Value], r: &[Value], family: Option<&str>) -> Vec<(usize, usize)> {
    let rid: BTreeMap<String, usize> = r.iter().enumerate().map(|(i, x)| (x["id"].as_str().unwrap().to_string(), i)).collect();
    (0..n.len())
        .filter(|&i| n[i]["origin"] != "human_only")
        .filter(|&i| family.is_none_or(|fam| n[i]["generator_label"] == fam))
        .filter_map(|i| rid.get(&format!("rform:{}", n[i]["id"].as_str().unwrap())).map(|&j| (i, j)))
        .collect()
}

#[test]
fn r2_eval2_fs_pairs_and_statistic() {
    let fx = fixture2();
    let out = run_value(&fx, &fx.cfg, "fs").unwrap();
    let d = fx.dir.path();
    let n = read_rows(&d.join("tn.jsonl"));
    let r = read_rows(&d.join("tr.jsonl"));
    let rows = parsed(&n);
    let pairs = fs_pairs_here(&n, &r, None);
    let g = gate(&out, "T5b.T");
    assert_eq!(g["pairs"][0]["counts"]["pairs"], pairs.len());
    assert_eq!(g["pairs"][0]["counts"]["screen_fail"], 1);
    assert_eq!(g["pairs"][0]["counts"]["n_form_positives_unpaired"], 1);
    let idx: Vec<usize> = pairs.iter().map(|p| p.0).collect();
    let w = balanced_weights(&rows, &idx);
    let fsd = |name: &str, seed: u64| -> Vec<f64> {
        let a = invs(&sys_path(&fx.cfg, "T.N", name, seed), &n);
        let b = invs(&sys_path(&fx.cfg, "T.R", name, seed), &r);
        pairs.iter().map(|&(i, j)| (b[j] - a[i]).abs()).collect()
    };
    let (c, z) = (fsd("Cand.u", 17), fsd("R0.u", 17));
    let dl: Vec<f64> = c.iter().zip(&z).map(|(a, b)| a - b).collect();
    let v = group_values(&rows, &idx, &idx, &w, &dl);
    let (lo, hi) = bootstrap_interval(&v).unwrap();
    let a = &g["levels"]["artifact"];
    assert_eq!(f(&a["point"]).to_bits(), (v.iter().sum::<f64>() / v.len() as f64).to_bits());
    assert_eq!(f(&a["lower"]).to_bits(), lo.to_bits());
    assert_eq!(f(&a["upper"]).to_bits(), hi.to_bits());
    assert!((f(&a["terms"][0]["fs_candidate"]) - wmean(&w, &c)).abs() < 1e-15);
    assert_eq!((a["terms"][0]["failed_pairs_candidate"].as_u64(), a["terms"][0]["failed_pairs_comparator"].as_u64()), (Some(0), Some(0)));
    assert_eq!(a["pass"], json!(hi < 0.0));
    // FRI: N-form minus R-form Brier at the gate weights, per seed.
    let fri = gate(&out, "FRI.T");
    for s in RECIPE_SEEDS {
        let k = s.to_string();
        let want = f(&out["systems"]["T.N"]["Cand.p"][&k]["gate_weight_brier"]) - f(&out["systems"]["T.R"]["Cand.p"][&k]["gate_weight_brier"]);
        assert_eq!(f(&fri["seeds"][&k]["fri"]).to_bits(), want.to_bits());
    }
    // A failed or missing prediction's involvement is the bound rule's worst case 1 - y.
    let pr = parsed(&n);
    let hum = pr.iter().find(|r| r.is_human()).unwrap();
    let pos = pr.iter().find(|r| !r.is_human()).unwrap();
    assert_eq!((involvement_doc(hum, None), involvement_doc(pos, None)), (1.0, 0.0));
    assert_eq!(involvement_doc(pos, Some(&Obs::Failed("timeout".into()))), 0.0);
}

#[test]
fn r2_eval2_fs_failures_are_worst_case() {
    // A pair with a failed or missing prediction on either form takes |p_R - p_N| = 1, for the
    // candidate and the comparator alike (§11; reading R6).
    let fx = fixture2();
    let d = fx.dir.path();
    let n = read_rows(&d.join("tn.jsonl"));
    let r = read_rows(&d.join("tr.jsonl"));
    let pairs = fs_pairs_here(&n, &r, None);
    let fail_ids = |which: &[(usize, usize)], on_r: bool, on_n: bool| -> BTreeSet<String> {
        let mut s = BTreeSet::new();
        for &(i, j) in which {
            if on_n {
                s.insert(n[i]["id"].as_str().unwrap().to_string());
            }
            if on_r {
                s.insert(r[j]["id"].as_str().unwrap().to_string());
            }
        }
        s
    };
    let fail_in = |cfg: &Value, sys: &str, pop: &str, seeds: &[u64], ids: &BTreeSet<String>| {
        for &s in seeds {
            let p = sys_path(cfg, pop, sys, s);
            let lines: Vec<Value> = read_rows(&p)
                .into_iter()
                .map(|l| if ids.contains(l["id"].as_str().unwrap()) { json!({"id": l["id"], "status": "failed", "failure": "timeout"}) } else { l })
                .collect();
            write_lines(&p, &lines);
        }
    };
    // (a) The candidate fails on both forms of every paired positive: FS = 1 and T5b fails.
    let ids = fail_ids(&pairs, true, true);
    fail_in(&fx.cfg, "Cand.u", "T.N", &RECIPE_SEEDS, &ids);
    fail_in(&fx.cfg, "Cand.u", "T.R", &RECIPE_SEEDS, &ids);
    let out = run_value(&fx, &fx.cfg, "fsf1").unwrap();
    let g = gate(&out, "T5b.T");
    for l in ["artifact", "recipe"] {
        let t = &g["levels"][l]["terms"][0];
        assert!((f(&t["fs_candidate"]) - 1.0).abs() < 1e-12, "{l}");
        assert_eq!(t["failed_pairs_candidate"], pairs.len(), "{l}");
        assert_eq!(g["levels"][l]["pass"], false, "{l}");
    }
    assert_eq!(g["status"], "fail");
    // (b) One pair fails on one form at seed 29 only: at the artifact level (seed 17) it is scored
    // as usual; at the recipe level it takes 1. The comparator's failures count the same way.
    let fx = fixture2();
    let (i0, j0) = pairs[3];
    fail_in(&fx.cfg, "Cand.u", "T.R", &[29], &fail_ids(&[(i0, j0)], true, false));
    fail_in(&fx.cfg, "R0.u", "T.N", &[17], &fail_ids(&[pairs[5]], false, true));
    let out = run_value(&fx, &fx.cfg, "fsf2").unwrap();
    let g = gate(&out, "T5b.T");
    let rows = parsed(&n);
    let idx: Vec<usize> = pairs.iter().map(|p| p.0).collect();
    let w = balanced_weights(&rows, &idx);
    let fsd = |name: &str, seeds: &[u64]| -> Vec<f64> {
        let mean = |pop: &str, rws: &[Value]| -> (Vec<f64>, Vec<bool>) {
            let v: Vec<Vec<f64>> = seeds.iter().map(|&s| invs(&sys_path(&fx.cfg, pop, name, s), rws)).collect();
            let fl: Vec<Vec<bool>> = seeds.iter().map(|&s| fails(&sys_path(&fx.cfg, pop, name, s), rws)).collect();
            ((0..rws.len()).map(|k| v.iter().map(|x| x[k]).sum::<f64>() / v.len() as f64).collect(),
             (0..rws.len()).map(|k| fl.iter().any(|x| x[k])).collect())
        };
        let ((a, fa), (b, fb)) = (mean("T.N", &n), mean("T.R", &r));
        pairs.iter().map(|&(i, j)| if fa[i] || fb[j] { 1.0 } else { (b[j] - a[i]).abs() }).collect()
    };
    let art = &g["levels"]["artifact"]["terms"][0];
    assert_eq!((art["failed_pairs_candidate"].as_u64(), art["failed_pairs_comparator"].as_u64()), (Some(0), Some(1)));
    assert!((f(&art["fs_candidate"]) - wmean(&w, &fsd("Cand.u", &[17]))).abs() < 1e-15);
    assert!((f(&art["fs_comparator"]) - wmean(&w, &fsd("R0.u", &[17]))).abs() < 1e-15);
    let rec = &g["levels"]["recipe"]["terms"][0];
    assert_eq!(rec["failed_pairs_candidate"], 1);
    let want = fsd("Cand.u", &RECIPE_SEEDS);
    assert_eq!(want[3], 1.0);
    assert!((f(&rec["fs_candidate"]) - wmean(&w, &want)).abs() < 1e-15);
}

#[test]
fn r2_eval2_fs_pooled_held_out_families() {
    // T5e: both blocks' held-out-family paired positives pooled, with balanced weights recomputed
    // over their union (reading R15); the family list keeps some families and drops others.
    let fx = fixture2();
    let d = fx.dir.path().to_path_buf();
    let mut cfg = fx.cfg.clone();
    let cn = read_rows(&d.join("cn.jsonl"));
    let cr = r_rows_of(&cn, "cm-4", C_SPLIT, "cg0.example", "C");
    write_lines(&d.join("cr.jsonl"), &cr);
    cfg["populations"].as_array_mut().unwrap().push(json!({"id": "C.R", "block": "C", "form": "R", "split": C_SPLIT, "rows": d.join("cr.jsonl"),
        "weights": {"rule": "cohort", "shares": d.join("shares.json")}}));
    add_system(&mut cfg, &d, "C.N", "cn", "Cand.u", &RECIPE_SEEDS, &cn, false);
    add_system(&mut cfg, &d, "C.R", "cr", "Cand.u", &RECIPE_SEEDS, &cr, false);
    add_system(&mut cfg, &d, "C.N", "cn", "R0.u", &[17], &cn, false);
    add_system(&mut cfg, &d, "C.R", "cr", "R0.u", &[17], &cr, false);
    // C positives get a second family on every third group.
    let mut cn2 = cn.clone();
    for r in &mut cn2 {
        if r["origin"] != "human_only" && unif(&format!("fam:{}", r["id"])) < 0.35 {
            r["generator_label"] = json!("gen-b");
        }
    }
    write_lines(&d.join("cn.jsonl"), &cn2);
    let mut cr2 = cr.clone();
    for r in &mut cr2 {
        if let Some(of) = r["twin_of"].as_str() {
            r["generator_label"] = cn2.iter().find(|x| x["id"] == of).unwrap()["generator_label"].clone();
        }
    }
    write_lines(&d.join("cr.jsonl"), &cr2);
    cfg["gates"].as_array_mut().unwrap().push(json!({"kind": "fs", "id": "T5e", "pairs": [{"n_population": "T.N", "r_population": "T.R"},
        {"n_population": "C.N", "r_population": "C.R"}], "candidate": "Cand.u", "comparator": "R0.u", "levels": ["artifact"],
        "families": ["gen-a"], "margin": {"value": 0.0}, "strict": true}));
    let out = run_value(&fx, &cfg, "t5e").unwrap();
    let g = gate(&out, "T5e");
    assert_eq!(g["pooled"], true);
    // Computed here: the union's pairs, balanced weights over the union (groups keyed by block).
    let tn = read_rows(&d.join("tn.jsonl"));
    let tr = read_rows(&d.join("tr.jsonl"));
    let (pt, pc) = (fs_pairs_here(&tn, &tr, Some("gen-a")), fs_pairs_here(&cn2, &cr2, Some("gen-a")));
    assert!(!pt.is_empty() && !pc.is_empty());
    assert!(pt.len() < fs_pairs_here(&tn, &tr, None).len() && pc.len() < fs_pairs_here(&cn2, &cr2, None).len());
    assert_eq!((g["pairs"][0]["counts"]["pairs"].as_u64(), g["pairs"][1]["counts"]["pairs"].as_u64()), (Some(pt.len() as u64), Some(pc.len() as u64)));
    let keys: Vec<(String, String)> = pt
        .iter()
        .map(|&(i, _)| (format!("T:{}", tn[i]["source_group"].as_str().unwrap()), cell_key(&tn[i])))
        .chain(pc.iter().map(|&(i, _)| (format!("C:{}", cn2[i]["source_group"].as_str().unwrap()), cell_key(&cn2[i]))))
        .collect();
    let w = balanced_here(&keys);
    let diffs = |name: &str, n: &[Value], r: &[Value], pp: &[(usize, usize)], np: &str, rp: &str| -> Vec<f64> {
        let a = invs(&sys_path(&cfg, np, name, 17), n);
        let b = invs(&sys_path(&cfg, rp, name, 17), r);
        pp.iter().map(|&(i, j)| (b[j] - a[i]).abs()).collect()
    };
    let fc: Vec<f64> = diffs("Cand.u", &tn, &tr, &pt, "T.N", "T.R").into_iter().chain(diffs("Cand.u", &cn2, &cr2, &pc, "C.N", "C.R")).collect();
    let fb: Vec<f64> = diffs("R0.u", &tn, &tr, &pt, "T.N", "T.R").into_iter().chain(diffs("R0.u", &cn2, &cr2, &pc, "C.N", "C.R")).collect();
    let a = &g["levels"]["artifact"];
    let dot = |x: &[f64]| w.iter().zip(x).map(|(p, q)| p * q).sum::<f64>();
    assert!((w.iter().sum::<f64>() - 1.0).abs() < 1e-12);
    assert!((f(&a["fs_candidate"]) - dot(&fc)).abs() < 1e-12);
    assert!((f(&a["fs_comparator"]) - dot(&fb)).abs() < 1e-12);
    assert!((f(&a["point"]) - (dot(&fc) - dot(&fb))).abs() < 1e-12);
    let groups = |pp: &[(usize, usize)], rows: &[Value]| pp.iter().map(|&(i, _)| rows[i]["source_group"].as_str().unwrap()).collect::<BTreeSet<_>>().len();
    assert_eq!(a["units"], json!({"T": groups(&pt, &tn), "C": groups(&pc, &cn2)}));
    // A family list that keeps nothing leaves no pair, which is refused.
    let mut c2 = cfg.clone();
    set_gate(&mut c2, "T5e", |g| g["families"] = json!(["none"]));
    assert!(err(run_value(&fx, &c2, "t5e2")).contains("no paired positives"));
    // Two pairs of one block, or three pairs, are refused.
    let mut c3 = cfg.clone();
    set_gate(&mut c3, "T5e", |g| g["pairs"] = json!([{"n_population": "T.N", "r_population": "T.R"}, {"n_population": "T.N", "r_population": "T.R"}]));
    assert!(err(run_value(&fx, &c3, "t5e3")).contains("one per block pooled"));
}

#[test]
fn r2_eval2_fs_refusals() {
    let fx = fixture2();
    let p = fx.dir.path().join("tr.jsonl");
    let base = r_rows_b3();
    let k = base.iter().position(|r| r["stratum"] == "raw_twin").unwrap();
    for (tag, edit) in [
        ("id", Box::new(|r: &mut Value| r["id"] = json!("rform:other")) as Box<dyn Fn(&mut Value)>),
        ("stratum", Box::new(|r: &mut Value| r["stratum"] = json!("other"))),
        ("twin_of", Box::new(|r: &mut Value| {
            r.as_object_mut().unwrap().remove("twin_of");
        })),
        ("group", Box::new(|r: &mut Value| r["source_group"] = json!("tg-elsewhere"))),
    ] {
        let mut rows = base.clone();
        edit(&mut rows[k]);
        write_lines(&p, &rows);
        let mut cfg = fx.cfg.clone();
        // Only the FS gate reads these fields; regenerate T.R predictions for the edited ids.
        for s in cfg["systems"].as_array_mut().unwrap() {
            if s["population"] == "T.R" {
                let lines: Vec<Value> = rows.iter().filter_map(|r| pred_for(s["name"].as_str().unwrap(), s["seed"].as_u64().unwrap(), r)).collect();
                write_lines(Path::new(s["predictions"].as_str().unwrap()), &lines);
            }
        }
        cfg["gates"].as_array_mut().unwrap().retain(|g| g["id"] != "FRI.T");
        let e = err(run_value(&fx, &cfg, &format!("fsr-{tag}")));
        assert!(e.contains("gate T5b.T"), "{tag}: {e}");
    }
}

/// Weighted within-domain standardisation (ddof 0), written out here.
fn z_here(rows: &[Value], w: &[f64]) -> Vec<f64> {
    let mut dom: BTreeMap<String, (f64, f64)> = BTreeMap::new();
    for (k, r) in rows.iter().enumerate() {
        let e = dom.entry(r["domain"].as_str().unwrap().into()).or_default();
        e.0 += w[k];
        e.1 += w[k] * f(&r["mwl"]);
    }
    let mut var: BTreeMap<String, f64> = BTreeMap::new();
    for (k, r) in rows.iter().enumerate() {
        let dname = r["domain"].as_str().unwrap();
        let (sw, sx) = dom[dname];
        *var.entry(dname.into()).or_default() += w[k] * (f(&r["mwl"]) - sx / sw).powi(2);
    }
    rows.iter()
        .map(|r| {
            let dname = r["domain"].as_str().unwrap();
            let (sw, sx) = dom[dname];
            (f(&r["mwl"]) - sx / sw) / (var[dname] / sw).sqrt()
        })
        .collect()
}

/// Weighted least-squares slope over a multiset of positions, written out here.
fn wls_here(ks: &[usize], w: &[f64], z: &[f64], y: &[f64]) -> f64 {
    let sw: f64 = ks.iter().map(|&k| w[k]).sum();
    let zb = ks.iter().map(|&k| w[k] * z[k]).sum::<f64>() / sw;
    let yb = ks.iter().map(|&k| w[k] * y[k]).sum::<f64>() / sw;
    let num: f64 = ks.iter().map(|&k| w[k] * (z[k] - zb) * (y[k] - yb)).sum();
    let den: f64 = ks.iter().map(|&k| w[k] * (z[k] - zb).powi(2)).sum();
    num / den
}

fn slope_here(rows: &[Value], w: &[f64], y: &[f64]) -> f64 {
    let all: Vec<usize> = (0..rows.len()).collect();
    wls_here(&all, w, &z_here(rows, w), y)
}

/// Positions per source group (key order), from rows.
fn groups_here(rows: &[Value]) -> Vec<Vec<usize>> {
    let mut g: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (k, r) in rows.iter().enumerate() {
        g.entry(r["source_group"].as_str().unwrap()).or_default().push(k);
    }
    g.into_values().collect()
}

fn logit_here(p: f64) -> f64 {
    assert!(p > 0.0 && p < 1.0);
    (p / (1.0 - p)).ln()
}

#[test]
fn r2_eval2_mwl_slope() {
    let fx = fixture2();
    let out = run_value(&fx, &fx.cfg, "sl").unwrap();
    let d = fx.dir.path();
    let n = read_rows(&d.join("tn.jsonl"));
    let rows = parsed(&n);
    let humans: Vec<usize> = (0..rows.len()).filter(|&i| rows[i].primary && rows[i].is_human()).collect();
    let w = balanced_weights(&rows, &humans);
    let hrows: Vec<Value> = humans.iter().map(|&i| n[i].clone()).collect();
    let lg = |name: &str, seed: u64| -> Vec<f64> { invs(&sys_path(&fx.cfg, "T.N", name, seed), &hrows).into_iter().map(logit_here).collect() };
    let g = &gate(&out, "T3e.T")["levels"];
    let (yc, yr) = (lg("Cand.u", 17), lg("R0.u", 17));
    let (sc, sr) = (slope_here(&hrows, &w, &yc), slope_here(&hrows, &w, &yr));
    assert!((f(&g["artifact"]["slope_candidate"]) - sc).abs() < 1e-9, "{} {sc}", g["artifact"]);
    assert!((f(&g["artifact"]["point"]) - (sc - sr)).abs() < 1e-9);
    // The bootstrap, recomputed here: source groups resampled, both slopes recomputed on the
    // full-sample z (reading R3).
    let z = z_here(&hrows, &w);
    let gl = groups_here(&hrows);
    let draws = bootstrap_draws(gl.len(), |ix| {
        let ks: Vec<usize> = ix.iter().flat_map(|&j| gl[j].iter().copied()).collect();
        wls_here(&ks, &w, &z, &yc) - wls_here(&ks, &w, &z, &yr)
    })
    .unwrap();
    assert!((f(&g["artifact"]["lower"]) - draws[24]).abs() < 1e-9, "{} {}", g["artifact"]["lower"], draws[24]);
    assert!((f(&g["artifact"]["upper"]) - draws[974]).abs() < 1e-9, "{} {}", g["artifact"]["upper"], draws[974]);
    // The recipe level averages the per-document logits over the seeds, against Faux.u's three seeds.
    let mean3 = |name: &str| -> Vec<f64> {
        let v: Vec<Vec<f64>> = RECIPE_SEEDS.iter().map(|&s| lg(name, s)).collect();
        (0..hrows.len()).map(|k| v.iter().map(|x| x[k]).sum::<f64>() / 3.0).collect()
    };
    let want = slope_here(&hrows, &w, &mean3("Cand.u")) - slope_here(&hrows, &w, &mean3("Faux.u"));
    assert!((f(&g["recipe"]["point"]) - want).abs() < 1e-9);
    assert_eq!(g["recipe"]["comparator"], "Faux.u");
    // The margin: the reseed SD of Faux.u's slope over its three seeds.
    let per: Vec<f64> = RECIPE_SEEDS.iter().map(|&s| slope_here(&hrows, &w, &lg("Faux.u", s))).collect();
    assert!((f(&g["recipe"]["margin"]) - sample_sd(&per).unwrap()).abs() < 1e-9);
    assert_eq!(g["recipe"]["pass"], json!(f(&g["recipe"]["upper"]) <= f(&g["recipe"]["margin"])));
    // A human without the field is refused.
    let mut bad = n.clone();
    let k = bad.iter().position(|r| r["origin"] == "human_only").unwrap();
    bad[k].as_object_mut().unwrap().remove("mwl");
    write_lines(&d.join("tn.jsonl"), &bad);
    assert!(err(run_value(&fx, &fx.cfg, "sl2")).contains("no numeric field mwl"));
    // A slope statistic in a paired gate, or a margin field on a Brier margin, is refused.
    write_lines(&d.join("tn.jsonl"), &n);
    let mut cfg = fx.cfg.clone();
    cfg["margins"].as_array_mut().unwrap()[0]["field"] = json!("mwl");
    assert!(err(run_value(&fx, &cfg, "sl3")).contains("a field is named exactly for a slope statistic"));
    let mut cfg = fx.cfg.clone();
    set_gate(&mut cfg, "T1a", |g| g["statistic"] = json!("mwl_slope"));
    assert!(err(run_value(&fx, &cfg, "sl4")).contains("is a slope"));
}

#[test]
fn r2_eval2_logit_is_exact_inside_and_derived_at_the_ends() {
    // Exact for 0 < p < 1, including values the old 1e-6 clamp moved.
    for p in [0.25, 1e-12, 1.0 - 1e-12, 0.5, f64::from_bits(2)] {
        assert_eq!(logit(p).to_bits(), (p / (1.0 - p)).ln().to_bits(), "{p}");
    }
    // p = 1 takes the largest f64 below 1, 1 - 2^-53; p = 0 the smallest positive f64, 2^-1074.
    assert_eq!(LOGIT_HIGH, f64::from_bits(1.0f64.to_bits() - 1));
    assert_eq!(LOGIT_LOW.to_bits(), 1);
    assert!((logit(1.0) - ((2f64.powi(53)) - 1.0).ln()).abs() < 1e-12);
    assert!((logit(0.0) + 1074.0 * 2f64.ln()).abs() < 1e-9);
    // The order is kept at both ends.
    assert!(logit(0.0) < logit(f64::from_bits(2)) && logit(f64::from_bits(2)) < logit(1e-300));
    assert!(logit(1.0 - f64::EPSILON) < logit(1.0));
}

#[test]
fn r2_eval2_within_slope_is_cc_human_measures() {
    let ours = include_str!("part2.rs");
    let theirs = include_str!("../../../grammar-cc-human/src/measure.rs");
    let body = |s: &str| {
        let i = s.find("pub fn within_slope").unwrap();
        s[i..i + s[i..].find("\n}\n").unwrap() + 3].to_string()
    };
    assert_eq!(body(ours), body(theirs));
    assert!((within_slope(&[(0, 2013.0, 0.1), (0, 2015.0, 0.3), (1, 2014.0, 0.5), (1, 2016.0, 0.5)]) - 0.05).abs() < 1e-15);
    assert!(within_slope(&[(0, 2013.0, 0.1), (1, 2014.0, 0.2)]).is_nan());
}

#[test]
fn r2_eval2_era_slope() {
    let fx = fixture2();
    let mut cfg = fx.cfg.clone();
    cfg["gates"].as_array_mut().unwrap().push(json!({"kind": "era_slope", "id": "T3h.logit", "population": "C.N", "field": "crawl_year",
        "candidate": "Cand.m", "comparator": "Recal.m", "levels": ["artifact"], "logit": true}));
    let out = run_value(&fx, &cfg, "era").unwrap();
    let d = fx.dir.path();
    let c = read_rows(&d.join("cn.jsonl"));
    let hum: Vec<Value> = c.iter().filter(|r| r["origin"] == "human_only").cloned().collect();
    let doms: BTreeSet<String> = hum.iter().map(|r| r["domain"].as_str().unwrap().to_string()).collect();
    let di: BTreeMap<&str, usize> = doms.iter().enumerate().map(|(i, x)| (x.as_str(), i)).collect();
    let vals = |name: &str, seed: u64, lg: bool| -> Vec<f64> {
        invs(&sys_path(&cfg, "C.N", name, seed), &hum).into_iter().map(|p| if lg { logit_here(p) } else { p }).collect()
    };
    let beta_on = |ks: &[usize], v: &[f64]| -> f64 {
        let rows: Vec<(usize, f64, f64)> = ks.iter().map(|&k| (di[hum[k]["domain"].as_str().unwrap()], f(&hum[k]["crawl_year"]), v[k])).collect();
        within_slope(&rows)
    };
    let all: Vec<usize> = (0..hum.len()).collect();
    let g = &gate(&out, "T3h")["levels"]["artifact"];
    let (vc, vr) = (vals("Cand.m", 17, false), vals("Recal.m", 17, false));
    let (bc, br) = (beta_on(&all, &vc), beta_on(&all, &vr));
    assert_eq!(f(&g["beta_candidate"]).to_bits(), bc.to_bits());
    assert_eq!(f(&g["point"]).to_bits(), (bc.abs() - br.abs()).to_bits());
    // The bootstrap, recomputed here: source groups resampled, both |beta| recomputed.
    let gl = groups_here(&hum);
    let draws = bootstrap_draws(gl.len(), |ix| {
        let ks: Vec<usize> = ix.iter().flat_map(|&j| gl[j].iter().copied()).collect();
        beta_on(&ks, &vc).abs() - beta_on(&ks, &vr).abs()
    })
    .unwrap();
    assert_eq!((f(&g["lower"]).to_bits(), f(&g["upper"]).to_bits()), (draws[24].to_bits(), draws[974].to_bits()));
    let per: Vec<f64> = RECIPE_SEEDS.iter().map(|&s| beta_on(&all, &vals("Recal.m", s, false)).abs()).collect();
    assert!((f(&g["margin"]) - std::f64::consts::SQRT_2 * sample_sd(&per).unwrap()).abs() < 1e-15);
    assert_eq!(g["pass"], json!(f(&g["upper"]) <= f(&g["margin"])));
    // The logit-scale slope is reported only, from the involvement logits.
    let gl2 = gate(&out, "T3h.logit");
    assert_eq!((gl2["status"].as_str(), gl2["scale"].as_str()), (Some("reported"), Some("uncalibrated_involvement_logit")));
    let bl = beta_on(&all, &vals("Cand.m", 17, true));
    assert_eq!(f(&gl2["levels"]["artifact"]["beta_candidate"]).to_bits(), bl.to_bits());
    let mut c2 = cfg.clone();
    set_gate(&mut c2, "T3h.logit", |g| g["margin"] = json!({"margin": "m_era"}));
    assert!(err(run_value(&fx, &c2, "era3")).contains("reported only"));
    // A human without an integer year is refused.
    let mut bad = c.clone();
    let k = bad.iter().position(|r| r["origin"] == "human_only").unwrap();
    bad[k]["crawl_year"] = json!(2013.5);
    write_lines(&d.join("cn.jsonl"), &bad);
    assert!(err(run_value(&fx, &fx.cfg, "era2")).contains("no integer year field crawl_year"));
}

#[test]
fn r2_eval2_t8b_all_equals_t8a_and_by_field() {
    let fx = fixture2();
    let out = run_value(&fx, &fx.cfg, "t8b").unwrap();
    let a = &gate(&out, "T8a")["levels"]["artifact"];
    let b = &gate(&out, "T8b")["levels"]["artifact"];
    assert_eq!(f(&b["all"]["point"]).to_bits(), f(&a["point"]).to_bits());
    assert_eq!(f(&b["all"]["upper"]).to_bits(), f(&a["upper"]).to_bits());
    assert_eq!(f(&b["all"]["lower"]).to_bits(), f(&a["lower"]).to_bits());
    // operation is absent on humans: the subset is the mirror positives plus every human.
    let mock = read_rows(&fx.dir.path().join("mock.jsonl"));
    assert_eq!(b["by"]["operation"]["mirror"]["documents"], mock.len());
    // The text field is never read.
    let mut cfg = fx.cfg.clone();
    set_gate(&mut cfg, "T8b", |g| g["by"] = json!(["text"]));
    assert!(err(run_value(&fx, &cfg, "t8b-text")).contains("the text field is never read"));
}

#[test]
fn r2_eval2_s3_1_and_s3_4() {
    let fx = fixture2();
    let out = run_value(&fx, &fx.cfg, "ms").unwrap();
    let ms = out["measures"].as_array().unwrap();
    let s1 = ms.iter().find(|m| m["id"] == "S3-1").unwrap();
    // s = SD of T8a's ratio draws x sqrt(Kish ESS / 1,000), computed here from the mock rows.
    let mock = read_rows(&fx.dir.path().join("mock.jsonl"));
    let raw: Vec<f64> = mock.iter().map(|r| f(&r["w_a8"])).collect();
    let tot: f64 = raw.iter().sum();
    let w: Vec<f64> = raw.iter().map(|x| x / tot).collect();
    let ess = w.iter().sum::<f64>().powi(2) / w.iter().map(|x| x * x).sum::<f64>();
    let lc = loss_vec(&sys_path(&fx.cfg, "mock", "Cand.t", 17), &mock);
    let lr = loss_vec(&sys_path(&fx.cfg, "mock", "R0.t", 17), &mock);
    let mut u: BTreeMap<String, (f64, f64)> = BTreeMap::new();
    for i in 0..mock.len() {
        let e = u.entry(mock[i]["ledger_unit"].as_str().unwrap().into()).or_default();
        e.0 += w[i] * (lc[i] - lr[i]);
        e.1 += w[i] * lr[i];
    }
    let (dd, rr): (Vec<f64>, Vec<f64>) = u.into_values().unzip();
    let draws = bootstrap_draws(dd.len(), |ix| ix.iter().map(|&j| dd[j]).sum::<f64>() / ix.iter().map(|&j| rr[j]).sum::<f64>()).unwrap();
    let se = sample_sd(&draws).unwrap();
    assert!((f(&s1["bootstrap_se"]) - se).abs() < 1e-12);
    assert!((f(&s1["kish_ess"]) - ess).abs() < 1e-9);
    let s = f(&s1["s"]);
    assert!((s - se * (ess / 1000.0).sqrt()).abs() < 1e-12);
    let (br, bm, sf) = t8a_branch(s).unwrap();
    assert_eq!((s1["t8a_branch"]["branch"].as_str().unwrap(), s1["t8a_branch"]["b_m"].as_u64().unwrap(), s1["t8a_branch"]["s_above_0_19_shortfall"].as_bool().unwrap()),
        (br.name(), bm, sf));
    // S3-4: pooled within-domain SD of group values, computed here with the bound group values.
    let s4 = ms.iter().find(|m| m["id"] == "S3-4").unwrap();
    let n = read_rows(&fx.dir.path().join("tn.jsonl"));
    let rows = parsed(&n);
    let prim: Vec<usize> = (0..rows.len()).filter(|&i| rows[i].primary).collect();
    let bw = balanced_weights(&rows, &prim);
    let a = loss_vec(&sys_path(&fx.cfg, "T.N", "Cand.p", 17), &n);
    let b = loss_vec(&sys_path(&fx.cfg, "T.N", "R0.p", 17), &n);
    let dl: Vec<f64> = prim.iter().map(|&i| a[i] - b[i]).collect();
    let gv = group_values(&rows, &prim, &prim, &bw, &dl);
    let groups: BTreeSet<&str> = prim.iter().map(|&i| rows[i].group.as_str()).collect();
    let mut by: BTreeMap<&str, Vec<f64>> = BTreeMap::new();
    for (g, v) in groups.iter().zip(&gv) {
        let dom = &rows[prim.iter().copied().find(|&i| rows[i].group == *g).unwrap()].domain;
        by.entry(dom.as_str()).or_default().push(*v);
    }
    let (mut ss, mut df) = (0.0, 0);
    for v in by.values() {
        let m = v.iter().sum::<f64>() / v.len() as f64;
        ss += v.iter().map(|x| (x - m).powi(2)).sum::<f64>();
        df += v.len() - 1;
    }
    assert!((f(&s4["sd"]) - (ss / df as f64).sqrt()).abs() < 1e-12);
    assert_eq!(s4["source_groups"], 24);
    // S3-4 refuses a group spanning domains.
    let mut bad = n.clone();
    let k = bad.iter().position(|r| r["source_group"] == "tg0" && r["origin"] != "human_only").unwrap();
    bad[k]["domain"] = json!("reddit");
    write_lines(&fx.dir.path().join("tn.jsonl"), &bad);
    assert!(err(run_value(&fx, &fx.cfg, "ms2")).contains("spans domains"));
}

#[test]
fn r2_eval2_measures_candidates_are_not_comparators() {
    let fx = fixture2();
    let mut cfg = fx.cfg.clone();
    cfg["measures"][0]["candidate"] = json!("R0.p");
    assert!(err(run_value(&fx, &cfg, "mc")).contains("is a gate candidate and is listed as a comparator"));
}

// ------------------------------------------------------------------ T2

/// A synthetic B10 evaluation record holding S3-4's measure on a Dev-iter-2 T population.
fn s34_record(d: &Path, tag: &str, sd: Value, split: &str) -> (PathBuf, String) {
    let rec = json!({"schema": EVAL_SCHEMA, "synthetic": true, "populations": {"T.D": {"split": split}},
        "measures": [{"id": "S3-4", "kind": "slice_sd", "population": "T.D", "candidate": "Cand.p", "comparator": "R0.p", "sd": sd}]});
    let p = d.join(format!("s34-{tag}.json"));
    std::fs::write(&p, rec.to_string()).unwrap();
    let h = sha256_hex(&std::fs::read(&p).unwrap());
    (p, h)
}

/// A T population of 48 groups (wikipedia 20 and reddit 10 groups of 8
/// humans, plos 18 groups of 4 humans, each group with a gen-a or gen-h
/// positive), and a C population of 20 essays and 10 news groups (8 humans
/// each); `frontier` positives (T, C) are spread over the groups.
fn slice_fixture_with(frontier: (usize, usize)) -> (tempfile::TempDir, Value, Value) {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let mut t = vec![];
    for g in 0..48usize {
        let dom = if g < 20 { "wikipedia" } else if g < 30 { "reddit" } else { "plos" };
        for k in 0..(if g < 30 { 8 } else { 4 }) {
            t.push(row(&format!("sh-{g}-{k}"), T_SPLIT, "human_only", &format!("sg{g}"), dom, "primary"));
        }
        let mut p = row(&format!("sp-{g}"), T_SPLIT, "mixed", &format!("sg{g}"), dom, "primary");
        p["generator_label"] = json!(if g % 2 == 0 { "gen-h" } else { "gen-a" });
        t.push(p);
    }
    for k in 0..frontier.0 {
        let g = k % 48;
        let dom = if g < 20 { "wikipedia" } else if g < 30 { "reddit" } else { "plos" };
        t.push(row(&format!("sf-{k}"), T_SPLIT, "model_only", &format!("sg{g}"), dom, "frontier"));
    }
    let mut c = vec![];
    for g in 0..30usize {
        let dom = if g < 20 { "essays" } else { "news" };
        for k in 0..8 {
            c.push(row(&format!("ch2-{g}-{k}"), C_SPLIT, "human_only", &format!("c{g}.example"), dom, "primary"));
        }
        let mut p = row(&format!("cp2-{g}"), C_SPLIT, "model_only", &format!("c{g}.example"), dom, "primary");
        p["generator_label"] = json!(if g % 3 == 0 { "gen-h" } else { "gen-a" });
        c.push(p);
    }
    for k in 0..frontier.1 {
        let g = k % 30;
        c.push(row(&format!("cf-{k}"), C_SPLIT, "model_only", &format!("c{g}.example"), if g < 20 { "essays" } else { "news" }, "frontier"));
    }
    for r in &mut t {
        r["block"] = json!("T");
    }
    for r in &mut c {
        r["block"] = json!("C");
    }
    write_lines(&d.join("st.jsonl"), &t);
    write_lines(&d.join("sc.jsonl"), &c);
    std::fs::write(d.join("shares.json"), SHARES).unwrap();
    let pops = json!([
        {"id": "T.N", "block": "T", "form": "N", "split": T_SPLIT, "rows": d.join("st.jsonl"), "weights": {"rule": "balanced"}},
        {"id": "C.N", "block": "C", "form": "N", "split": C_SPLIT, "rows": d.join("sc.jsonl"), "weights": {"rule": "cohort", "shares": d.join("shares.json")}}
    ]);
    let mut systems = vec![];
    for (pid, tag, rows) in [("T.N", "st", &t), ("C.N", "sc", &c)] {
        for (name, seeds) in [("Cand.p", &RECIPE_SEEDS[..]), ("R0.p", &[17u64][..]), ("Same.p", &[17u64][..])] {
            for &s in seeds {
                let pp = d.join(format!("pred-{tag}-{name}-{s}.jsonl"));
                let src = if name == "Same.p" { "R0.p" } else { name };
                write_lines(&pp, &rows.iter().filter_map(|r| pred_line(src, s, r)).collect::<Vec<_>>());
                systems.push(json!({"name": name, "seed": s, "population": pid, "predictions": pp}));
            }
        }
    }
    let (sp, sh) = s34_record(d, "plan", json!(0.04), "deviter2_t");
    let plan = json!({"schema": SLICE_PLAN_CONFIG_SCHEMA, "populations": pops, "sd": {"record": sp, "sha256": sh, "measure": "S3-4"},
        "held_out_generators": ["gen-h"]});
    let cfg = json!({"schema": CONFIG_SCHEMA, "populations": pops, "systems": systems, "comparators": ["R0.p"], "gates": []});
    (dir, plan, cfg)
}

fn slice_fixture() -> (tempfile::TempDir, Value, Value) {
    slice_fixture_with((0, 0))
}

fn plan_of(plan: &Value) -> Result<Value> {
    slice_plan(&parse_slice_plan_config(&serde_json::to_vec(plan).unwrap())?)
}

/// Writes a plan record and a T2 gate configuration over it.
fn t2_cfg(d: &Path, cfg: &Value, rec: &Value, tag: &str, candidate: &str, levels: Value) -> Value {
    let pp = d.join(format!("plan-{tag}.json"));
    write_new(&pp, &pretty(rec).unwrap()).unwrap();
    let h = sha256_hex(&std::fs::read(&pp).unwrap());
    let mut c = cfg.clone();
    c["gates"] = json!([{"kind": "slices", "id": "T2", "plan": pp, "plan_sha256": h, "candidate": candidate, "comparator": "R0.p", "levels": levels}]);
    c
}

fn run_cfg(d: &Path, cfg: &Value, tag: &str) -> Result<Value> {
    let c = parse_config(&serde_json::to_vec(cfg).unwrap())?;
    run(&c, &d.join(format!("{tag}-m.json")), &d.join(format!("{tag}-o.json")))
}

#[test]
fn r2_eval2_slice_plan_and_t2() {
    let (dir, plan, cfg) = slice_fixture();
    let d = dir.path();
    let rec = plan_of(&plan).unwrap();
    let sl = rec["slices"].as_array().unwrap();
    // §11.4's family, derived from the rows: the T domains, essays and news, and the two pooled slices.
    let ids: Vec<&str> = sl.iter().map(|s| s["id"].as_str().unwrap()).collect();
    assert_eq!(ids, ["domain:plos", "domain:reddit", "domain:wikipedia", "stratum:essays", "stratum:news", "held_out_generator", "frontier"]);
    let by = |id: &str| sl.iter().find(|s| s["id"] == id).unwrap();
    // Counts from rows only: wikipedia 20 groups / 160 humans; reddit 10 groups (short); plos 18 groups but
    // 72 humans (short); essays 20 / 160; news 10 groups (short); the pooled held-out slice holds every T and
    // C group (all humans) and the gen-h positives; frontier has no frontier positive.
    assert_eq!(by("domain:wikipedia")["counts"]["source_groups"], 20);
    assert_eq!(by("domain:wikipedia")["counts"]["human_documents"], 160);
    assert_eq!(by("domain:reddit")["candidate"], false);
    assert_eq!(by("domain:reddit")["shortfall"][0], "source_groups 10 < 15");
    assert_eq!(by("domain:plos")["counts"]["source_groups"], 18);
    assert_eq!((by("domain:plos")["candidate"].clone(), by("domain:plos")["shortfall"].clone()), (json!(false), json!(["human_documents 72 < 150"])));
    assert_eq!(by("stratum:news")["shortfall"][0], "source_groups 10 < 15");
    assert_eq!(by("held_out_generator")["counts"]["source_groups"], 78);
    assert_eq!(by("held_out_generator")["counts"]["human_documents"], 552);
    assert_eq!(by("held_out_generator")["terms"], json!([{"population": "T.N", "dimension": "held_out_generator", "value": "all", "coef": 0.5},
        {"population": "C.N", "dimension": "held_out_generator", "value": "all", "coef": 0.5}]));
    assert_eq!(by("frontier")["candidate"], false);
    assert!(by("frontier")["shortfall"].as_array().unwrap().contains(&json!("frontier_positives 0 < 150")));
    // S3-4's SD is read from its record, by digest.
    assert_eq!(rec["sd"]["value"], 0.04);
    assert_eq!(rec["sd"]["measure"], "S3-4");
    // SE_planned = SD / sqrt(groups); K and the gated set by r2_slice_gate_v2.
    for id in ["domain:wikipedia", "stratum:essays"] {
        assert_eq!(f(&by(id)["se_planned"]).to_bits(), (0.04 / 20f64.sqrt()).to_bits());
    }
    let se = [0.04 / 20f64.sqrt(), 0.04 / 20f64.sqrt(), 0.04 / 78f64.sqrt()];
    let (k, z, w) = slice_gate_k(&se);
    assert_eq!(rec["k"], k);
    assert_eq!(f(&rec["z_k"]).to_bits(), z.unwrap().to_bits());
    assert_eq!(by("domain:wikipedia")["gated"], w[0]);
    assert_eq!(by("held_out_generator")["gated"], w[2]);
    assert_eq!(by("domain:reddit")["gated"], false);
    // The plan file and the T2 gate.
    let c1 = t2_cfg(d, &cfg, &rec, "p1", "Cand.p", json!(["artifact", "recipe"]));
    let out = run_cfg(d, &c1, "t2").unwrap();
    let g = &out["gates"][0];
    let s = g["slices"].as_array().unwrap().iter().find(|s| s["slice"] == "domain:wikipedia").unwrap();
    // The wikipedia slice computed here: balanced weights over its documents, the bound group values,
    // the protocol bootstrap, SE = SD of the draws.
    let t = read_rows(&d.join("st.jsonl"));
    let rows = parsed(&t);
    let idx: Vec<usize> = (0..rows.len()).filter(|&i| rows[i].domain == "wikipedia").collect();
    let bw = balanced_weights(&rows, &idx);
    let a = loss_vec(&d.join("pred-st-Cand.p-17.jsonl"), &t);
    let b = loss_vec(&d.join("pred-st-R0.p-17.jsonl"), &t);
    let dl: Vec<f64> = idx.iter().map(|&i| a[i] - b[i]).collect();
    let v = group_values(&rows, &idx, &idx, &bw, &dl);
    let draws = bootstrap_draws(v.len(), |ix| ix.iter().map(|&j| v[j]).sum::<f64>() / v.len() as f64).unwrap();
    let se = sample_sd(&draws).unwrap();
    let art = &s["levels"]["artifact"];
    assert_eq!(f(&art["point"]).to_bits(), (v.iter().sum::<f64>() / v.len() as f64).to_bits());
    assert_eq!(f(&art["se_realised"]).to_bits(), se.to_bits());
    assert_eq!(f(&art["statistic"]).to_bits(), (f(&art["point"]) / se).to_bits());
    if s["gated"] == true {
        assert_eq!(art["pass"], json!(f(&art["statistic"]) <= z.unwrap()));
    }
    // The pooled slice resamples T then C, each term at half weight.
    let hs = g["slices"].as_array().unwrap().iter().find(|s| s["slice"] == "held_out_generator").unwrap();
    assert_eq!(hs["levels"]["artifact"]["units"], json!({"T": 48, "C": 30}));
    // A reported slice carries no pass.
    let rd = g["slices"].as_array().unwrap().iter().find(|s| s["slice"] == "domain:reddit").unwrap();
    assert!(rd["levels"]["artifact"].get("pass").is_none());
    // A plan edited and re-hashed is refused unless it is what the rows, §11.4's family and S3-4 give:
    // a flipped gated flag; a short slice (reddit, 10 groups) made a candidate with a small planned SE and
    // K, z_K and the gated set made consistent (the review's probe); a slice left out.
    let mut bad1 = rec.clone();
    let k1 = bad1["slices"].as_array().unwrap().iter().position(|s| s["gated"] == true).unwrap();
    bad1["slices"][k1]["gated"] = json!(false);
    let mut bad2 = rec.clone();
    let k2 = bad2["slices"].as_array().unwrap().iter().position(|s| s["id"] == "domain:reddit").unwrap();
    bad2["slices"][k2]["candidate"] = json!(true);
    bad2["slices"][k2]["shortfall"] = json!([]);
    bad2["slices"][k2]["se_planned"] = json!(0.001);
    let se2: Vec<f64> = bad2["slices"].as_array().unwrap().iter().filter_map(|s| s["se_planned"].as_f64()).collect();
    let (kk, zz, worthy) = slice_gate_k(&se2);
    let mut wi = worthy.into_iter();
    for s in bad2["slices"].as_array_mut().unwrap() {
        let gw = s["candidate"] == true && wi.next().unwrap_or(false);
        s["gate_worthy"] = json!(gw);
        s["gated"] = json!(gw);
    }
    bad2["k"] = json!(kk);
    bad2["z_k"] = json!(zz);
    let mut bad3 = rec.clone();
    bad3["slices"].as_array_mut().unwrap().retain(|s| s["id"] != "stratum:essays");
    for (tag, bad) in [("b1", bad1), ("b2", bad2), ("b3", bad3)] {
        let cb = t2_cfg(d, &cfg, &bad, tag, "Cand.p", json!(["artifact"]));
        let e = format!("{:#}", run_cfg(d, &cb, tag).unwrap_err());
        assert!(e.contains("does not follow from the rows"), "{tag}: {e}");
    }
    // An SD other than S3-4's record's, its record changed, or bytes that are not the named digest, are refused.
    let mut bad4 = rec.clone();
    bad4["sd"]["value"] = json!(0.02);
    let e = format!("{:#}", run_cfg(d, &t2_cfg(d, &cfg, &bad4, "b4", "Cand.p", json!(["artifact"])), "b4").unwrap_err());
    assert!(e.contains("is not S3-4's record's"), "{e}");
    let mut c2 = c1.clone();
    c2["gates"][0]["plan_sha256"] = json!("0".repeat(64));
    assert!(format!("{:#}", run_cfg(d, &c2, "t2b").unwrap_err()).contains("hashes to"));
    // The rows changed after the plan: refused.
    let mut t2 = t.clone();
    t2.retain(|r| r["id"] != "sh-0-0");
    write_lines(&d.join("st.jsonl"), &t2);
    for nm in ["Cand.p-17", "Cand.p-29", "Cand.p-41", "R0.p-17", "Same.p-17"] {
        let p = d.join(format!("pred-st-{nm}.jsonl"));
        let keep: Vec<Value> = read_rows(&p).into_iter().filter(|r| r["id"] != "sh-0-0").collect();
        write_lines(&p, &keep);
    }
    let e = format!("{:#}", run_cfg(d, &c1, "t2c").unwrap_err());
    assert!(e.contains("not the rows the slice plan counted"), "{e}");
    // S3-4's record changed after the plan: refused.
    write_lines(&d.join("st.jsonl"), &t);
    let sp = PathBuf::from(rec["sd"]["record"].as_str().unwrap());
    let mut sv: Value = serde_json::from_slice(&std::fs::read(&sp).unwrap()).unwrap();
    sv["measures"][0]["sd"] = json!(0.05);
    std::fs::write(&sp, sv.to_string()).unwrap();
    let e = format!("{:#}", run_cfg(d, &c1, "t2d").unwrap_err());
    assert!(e.contains("S3-4's record") && e.contains("hashes to"), "{e}");
}

#[test]
fn r2_eval2_t2_gated_slice_with_zero_se_is_not_evaluated() {
    // A candidate equal to R0 everywhere: every gated slice has SE_realised 0 (reading R2).
    let (dir, plan, cfg) = slice_fixture();
    let d = dir.path();
    let rec = plan_of(&plan).unwrap();
    assert!(rec["k"].as_u64().unwrap() > 0);
    let out = run_cfg(d, &t2_cfg(d, &cfg, &rec, "z", "Same.p", json!(["artifact"])), "z").unwrap();
    let g = &out["gates"][0];
    assert_eq!(g["status"], "not_evaluated");
    assert!(g["reasons"][0].as_str().unwrap().contains("SE_realised is 0"));
    // The pass rule at the boundary: point / SE equal to z_K passes.
    assert!(slice_pass(2.0, 2.0) && !slice_pass(2.0 + 1e-12, 2.0));
}

#[test]
fn r2_eval2_slice_frontier_minimum() {
    // frontier (T and C pooled) needs 150 frontier positives besides the group and human minimums.
    for ((ft, fc), want) in [((100, 49), false), ((100, 50), true)] {
        let (_dir, plan, _) = slice_fixture_with((ft, fc));
        let rec = plan_of(&plan).unwrap();
        let fr = rec["slices"].as_array().unwrap().iter().find(|s| s["id"] == "frontier").unwrap().clone();
        assert_eq!(fr["counts"]["frontier_positives"], ft + fc);
        assert_eq!(fr["counts"]["source_groups"], 78);
        assert_eq!(fr["candidate"], want, "{fr}");
        if !want {
            assert_eq!(fr["shortfall"], json!(["frontier_positives 149 < 150"]));
        }
        // Frontier positives are never counted in another slice's frontier count.
        let wk = rec["slices"].as_array().unwrap().iter().find(|s| s["id"] == "domain:wikipedia").unwrap().clone();
        let t = read_rows(&PathBuf::from(plan["populations"][0]["rows"].as_str().unwrap()));
        let n = t.iter().filter(|r| r["domain"] == "wikipedia" && r["population"] == "frontier").count();
        assert_eq!(wk["counts"]["frontier_positives"], n);
    }
}

#[test]
fn r2_eval2_slice_plan_refusals_and_no_candidates() {
    // A small T and C population: no slice reaches the minimum, so K = 0 and T2 gates nothing.
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let mut t = vec![];
    let mut c = vec![];
    for g in 0..6 {
        t.push(row(&format!("h{g}"), T_SPLIT, "human_only", &format!("g{g}"), "wikipedia", "primary"));
        t.push(row(&format!("p{g}"), T_SPLIT, "mixed", &format!("g{g}"), "wikipedia", "primary"));
        c.push(row(&format!("ch{g}"), C_SPLIT, "human_only", &format!("c{g}.example"), "news", "primary"));
        c.push(row(&format!("cp{g}"), C_SPLIT, "mixed", &format!("c{g}.example"), "news", "primary"));
    }
    for r in &mut t {
        r["block"] = json!("T");
    }
    for r in &mut c {
        r["block"] = json!("C");
    }
    write_lines(&d.join("t.jsonl"), &t);
    write_lines(&d.join("c.jsonl"), &c);
    std::fs::write(d.join("shares.json"), SHARES).unwrap();
    let pops = json!([
        {"id": "T.N", "block": "T", "form": "N", "split": T_SPLIT, "rows": d.join("t.jsonl"), "weights": {"rule": "balanced"}},
        {"id": "C.N", "block": "C", "form": "N", "split": C_SPLIT, "rows": d.join("c.jsonl"), "weights": {"rule": "cohort", "shares": d.join("shares.json")}}
    ]);
    let (sp, sh) = s34_record(d, "a", json!(0.04), "deviter2_t");
    let plan = json!({"schema": SLICE_PLAN_CONFIG_SCHEMA, "populations": pops, "sd": {"record": sp, "sha256": sh, "measure": "S3-4"}});
    let rec = plan_of(&plan).unwrap();
    assert_eq!(rec["k"], 0);
    assert!(rec["z_k"].is_null());
    assert!(rec["slices"].as_array().unwrap().iter().all(|s| s["gated"] == false));
    // Two T populations, a mock, an R-form population, or no C population are refused.
    let mut p2 = plan.clone();
    p2["populations"][1] = json!({"id": "T.N2", "block": "T", "form": "N", "split": T_SPLIT, "rows": d.join("t.jsonl"), "weights": {"rule": "balanced"}});
    assert!(format!("{:#}", plan_of(&p2).unwrap_err()).contains("exactly one block-T population"));
    let mut p3 = plan.clone();
    p3["populations"][0]["form"] = json!("R");
    assert!(format!("{:#}", plan_of(&p3).unwrap_err()).contains("is not N-form"));
    let mut p4 = plan.clone();
    p4["populations"].as_array_mut().unwrap().pop();
    assert!(plan_of(&p4).is_err());
    // S3-4's record: a bad SD, another split, another measure kind, or other bytes are refused.
    for (tag, sd, split, want) in [("z", json!(0.0), "deviter2_t", "finite and positive"), ("s", json!(0.04), "deviter2_c", "Dev-iter-2 T")] {
        let (sp, sh) = s34_record(d, tag, sd, split);
        let mut p = plan.clone();
        p["sd"] = json!({"record": sp, "sha256": sh, "measure": "S3-4"});
        let e = format!("{:#}", plan_of(&p).unwrap_err());
        assert!(e.contains(want), "{tag}: {e}");
    }
    let mut p5 = plan.clone();
    p5["sd"]["measure"] = json!("S3-1");
    assert!(format!("{:#}", plan_of(&p5).unwrap_err()).contains("no measure S3-1"));
    let mut p6 = plan.clone();
    p6["sd"]["sha256"] = json!("0".repeat(64));
    assert!(format!("{:#}", plan_of(&p6).unwrap_err()).contains("hashes to"));
    // The slice list is derived, never configured.
    let mut p7 = plan.clone();
    p7["slices"] = json!([]);
    assert!(plan_of(&p7).is_err());
}

// ------------------------------------------------------------------ selection, decision, null check, reports

fn selection_cfg(fx: &Fixture, n1_fs_margin: f64, recal_hinj_margin: f64, tag: &str) -> Value {
    let mut cfg = fx.cfg.clone();
    let g = cfg["gates"].as_array_mut().unwrap();
    for x in [
        json!({"kind": "paired", "id": "E.H.N1", "statistic": "human_brier_balanced", "terms": [{"population": "T.N"}], "candidate": "Cand.u", "comparator": "Faux.u", "levels": ["recipe"], "margin": {"value": 1.0}}),
        json!({"kind": "paired", "id": "E.R.N1", "statistic": "brier", "terms": [{"population": "T.R"}], "candidate": "Cand.p", "comparator": "R0.p", "levels": ["recipe"], "margin": {"value": 1.0}}),
        json!({"kind": "shift", "id": "E.I.N1", "population": "T.HC", "system": "Cand.u", "levels": ["recipe"], "margin": {"value": 1.0}}),
        json!({"kind": "fs", "id": "E.F.N1", "pairs": [{"n_population": "T.N", "r_population": "T.R"}], "candidate": "Cand.u", "comparator": "R0.u", "levels": ["recipe"], "margin": {"value": n1_fs_margin}}),
        json!({"kind": "paired", "id": "E.H.RC", "statistic": "human_brier_balanced", "terms": [{"population": "T.N"}], "candidate": "Recal2.u", "comparator": "Faux.u", "levels": ["recipe"], "margin": {"value": 1.0}}),
        json!({"kind": "paired", "id": "E.R.RC", "statistic": "brier", "terms": [{"population": "T.R"}], "candidate": "Recal2.p", "comparator": "R0.p", "levels": ["recipe"], "margin": {"value": 1.0}}),
        json!({"kind": "shift", "id": "E.I.RC", "population": "T.HC", "system": "Recal2.u", "levels": ["recipe"], "margin": {"value": recal_hinj_margin}}),
    ] {
        g.push(x);
    }
    cfg["selection"] = json!({"record": fx.dir.path().join(format!("{tag}-selection-record-r2.json")), "order": ["N1", "R0-recal"], "candidates": [
        {"name": "N1", "kind": "retrained", "blend": "Cand", "conditions": [
            {"condition": "human_only", "gate": "E.H.N1"}, {"condition": "r_form", "gate": "E.R.N1"},
            {"condition": "h_inj", "gate": "E.I.N1"}, {"condition": "fs", "gate": "E.F.N1"}]},
        {"name": "R0-recal", "kind": "calibration_only", "blend": "Recal", "conditions": [
            {"condition": "human_only", "gate": "E.H.RC"}, {"condition": "r_form", "gate": "E.R.RC"}, {"condition": "h_inj", "gate": "E.I.RC"}]}
    ]});
    cfg
}

/// Recal2.u / Recal2.p: copies of Recal.p's predictions under candidate names (R0-recal as a candidate).
fn add_recal2(fx: &Fixture, cfg: &mut Value) {
    let d = fx.dir.path().to_path_buf();
    for (pid, tag) in [("T.N", "tn"), ("T.R", "tr"), ("T.HC", "hc")] {
        let rows = read_rows(&d.join(format!("{tag}.jsonl")));
        for name in ["Recal2.u", "Recal2.p"] {
            for s in RECIPE_SEEDS {
                let pp = d.join(format!("pred-{tag}-{name}-{s}.jsonl"));
                let lines: Vec<Value> = rows.iter().filter_map(|r| pred_for("Recal.p", s, r)).collect();
                write_lines(&pp, &lines);
                cfg["systems"].as_array_mut().unwrap().push(json!({"name": name, "seed": s, "population": pid, "predictions": pp}));
            }
        }
    }
}

fn read_json(p: &Path) -> Value {
    serde_json::from_slice(&std::fs::read(p).unwrap()).unwrap()
}

#[test]
fn r2_eval2_selection_u7_and_tie() {
    let fx = fixture2();
    let mut cfg = selection_cfg(&fx, 1.0, 1.0, "s1");
    add_recal2(&fx, &mut cfg);
    let out = run_value(&fx, &cfg, "s1").unwrap();
    let sel = &out["selection"];
    let rec = read_json(Path::new(sel["record"].as_str().unwrap()));
    assert_eq!(sel["sha256"], sha256_hex(&std::fs::read(sel["record"].as_str().unwrap()).unwrap()).as_str());
    assert_eq!(rec["schema"], SELECTION_SCHEMA);
    assert_eq!(rec["candidates"][0]["eligible"], true);
    assert_eq!(rec["candidates"][1]["eligible"], true);
    // Every candidate's Dev-select-2 values: its blend's per-seed values and each condition gate's levels.
    assert_eq!(rec["candidates"][0]["blend_values"], out["blends"]["Cand"]);
    assert_eq!(rec["candidates"][1]["conditions"][2]["levels"], gate(&out, "E.I.RC")["levels"]);
    // U7: lowest blend first; the tie interval is part 1's "tie" composite (Cand − Recal at the recipe level).
    let (bc, br) = (f(&out["blends"]["Cand"]["seed_mean"]), f(&out["blends"]["Recal"]["seed_mean"]));
    let first = if bc <= br { "N1" } else { "R0-recal" };
    assert_eq!(rec["eligible_ranking"][0], first);
    let tie = &gate(&out, "tie")["levels"]["recipe"];
    let ti = &rec["tie"]["interval"];
    let sign = if first == "N1" { 1.0 } else { -1.0 };
    assert!((f(&ti["point"]) - sign * f(&tie["point"])).abs() < 1e-15);
    if first == "N1" {
        assert_eq!(f(&ti["lower"]).to_bits(), f(&tie["lower"]).to_bits());
        assert_eq!(f(&ti["upper"]).to_bits(), f(&tie["upper"]).to_bits());
    }
    let want = if ti["includes_zero"] == true { "N1" } else { first };
    assert_eq!(rec["winner"], want);
    // A second run into the same record is refused before any read.
    assert!(err(run_value(&fx, &cfg, "s1b")).contains("a new selection record is required"));
}

#[test]
fn r2_eval2_selection_eligibility_and_exemptions() {
    let fx = fixture2();
    // N1 fails its FS condition (margin -1); R0-recal fails H-inj, from which it is exempt.
    let mut cfg = selection_cfg(&fx, -1.0, -1.0, "s2");
    add_recal2(&fx, &mut cfg);
    let out = run_value(&fx, &cfg, "s2").unwrap();
    let rec = read_json(Path::new(out["selection"]["record"].as_str().unwrap()));
    assert_eq!(rec["candidates"][0]["eligible"], false);
    assert_eq!(rec["candidates"][1]["eligible"], true);
    assert_eq!(rec["candidates"][1]["conditions"][2]["status"], "fail");
    assert!(rec["candidates"][1]["conditions"][2]["role"].as_str().unwrap().starts_with("reported"));
    assert_eq!(rec["winner"], "R0-recal");
    assert!(rec["tie"].is_null());
    // Nobody eligible: R0 stays.
    let mut cfg = selection_cfg(&fx, -1.0, -1.0, "s3");
    add_recal2(&fx, &mut cfg);
    set_gate(&mut cfg, "E.H.RC", |g| g["margin"] = json!({"value": -1.0}));
    let out = run_value(&fx, &cfg, "s3").unwrap();
    let rec = read_json(Path::new(out["selection"]["record"].as_str().unwrap()));
    assert!(rec["winner"].is_null());
    assert!(rec["outcome"].as_str().unwrap().contains("R0 stays"));
    // Structure: a retrained candidate without FS, a candidate without its R-form condition, a wrong gate
    // kind, an ungated condition gate, a bad order.
    let mut c = selection_cfg(&fx, 1.0, 1.0, "s4");
    c["selection"]["candidates"][0]["conditions"].as_array_mut().unwrap().pop();
    assert!(err(run_value(&fx, &c, "s4")).contains("H-inj and FS"));
    for k in [0, 1] {
        let mut c = selection_cfg(&fx, 1.0, 1.0, "s4r");
        c["selection"]["candidates"][k]["conditions"].as_array_mut().unwrap().retain(|x| x["condition"] != "r_form");
        assert!(err(run_value(&fx, &c, "s4r")).contains("needs the human-only and R-form conditions"), "{k}");
    }
    let mut c = selection_cfg(&fx, 1.0, 1.0, "s5");
    c["selection"]["candidates"][0]["conditions"][0]["gate"] = json!("E.R.N1");
    assert!(err(run_value(&fx, &c, "s5")).contains("is not a HumanOnly condition"));
    let mut c = selection_cfg(&fx, 1.0, 1.0, "s5b");
    set_gate(&mut c, "E.I.N1", |g| {
        g.as_object_mut().unwrap().remove("margin");
    });
    assert!(err(run_value(&fx, &c, "s5b")).contains("is not a HInj condition"));
    let mut c = selection_cfg(&fx, 1.0, 1.0, "s6");
    c["selection"]["order"] = json!(["N1"]);
    assert!(err(run_value(&fx, &c, "s6")).contains("order"));
}

#[test]
fn r2_eval2_selection_record_is_written_last() {
    // A report set naming a gated gate is refused by the structural checks, before anything is read
    // or written (the review's probe).
    let fx = fixture2();
    let mut cfg = selection_cfg(&fx, 1.0, 1.0, "o1");
    add_recal2(&fx, &mut cfg);
    cfg["report_sets"] = json!([{"id": "x", "label": "x", "gates": ["T3f.C"]}]);
    let e = err(run_value(&fx, &cfg, "o1"));
    assert!(e.contains("report set x: gate T3f.C has a pass rule"), "{e}");
    assert!(!fx.dir.path().join("o1-selection-record-r2.json").exists());
    assert!(!fx.dir.path().join("o1-margins.json").exists());
    // A failure after the selection is computed (an external decision record whose bytes are not its
    // digest) leaves neither the selection record nor the evaluation.
    let mut cfg = selection_cfg(&fx, 1.0, 1.0, "o2");
    add_recal2(&fx, &mut cfg);
    let p = fx.dir.path().join("t7a.json");
    std::fs::write(&p, json!({"schema": EXTERNAL_SCHEMA, "protocol": "T7a", "candidate": "N1", "status": "pass"}).to_string()).unwrap();
    cfg["decision"] = json!({"candidate": "N1", "kind": "retrained", "gates": [], "external": [{"protocol": "T7a", "record": p, "sha256": "0".repeat(64)}]});
    let e = err(run_value(&fx, &cfg, "o2"));
    assert!(e.contains("external record"), "{e}");
    assert!(!fx.dir.path().join("o2-selection-record-r2.json").exists());
    assert!(!fx.dir.path().join("o2-out.json").exists());
}

#[test]
fn r2_eval2_selection_tie_prefers_the_order() {
    // N1's blend is R0-recal's with one T.N positive scored worse: R0-recal ranks first, but the
    // paired interval includes zero, so N1 (earlier in the order) wins.
    let fx = fixture2();
    let d = fx.dir.path().to_path_buf();
    let mut cfg = selection_cfg(&fx, 1.0, 1.0, "tp");
    add_recal2(&fx, &mut cfg);
    for s in RECIPE_SEEDS {
        let p = d.join(format!("pred-tn-Recal2.p-{s}.jsonl"));
        let mut lines = read_rows(&p);
        let k = lines.iter().position(|l| l["id"] == "tN-2-0").unwrap();
        let inv = 1.0 - f(&lines[k]["p_human"]) - 0.05;
        lines[k] = json!({"id": "tN-2-0", "p_human": 1.0 - inv, "p_model_only": inv * 0.6, "p_mixed": inv * 0.4, "share": inv * 0.6});
        write_lines(&p, &lines);
    }
    cfg["blends"].as_array_mut().unwrap().push(json!({"id": "Near", "t_n": {"population": "T.N", "system": "Recal2.p"},
        "t_r": {"population": "T.R", "system": "Recal2.p"}, "c_n": {"population": "C.N", "system": "Recal.m"}}));
    cfg["selection"]["candidates"][0]["blend"] = json!("Near");
    let out = run_value(&fx, &cfg, "tp").unwrap();
    assert!(f(&out["blends"]["Near"]["seed_mean"]) > f(&out["blends"]["Recal"]["seed_mean"]));
    let rec = read_json(Path::new(out["selection"]["record"].as_str().unwrap()));
    assert_eq!(rec["eligible_ranking"], json!(["R0-recal", "N1"]));
    let ti = &rec["tie"]["interval"];
    // R0-recal − N1: every draw is <= 0 and many are exactly 0, so the upper bound is 0.
    assert_eq!((f(&ti["upper"]), ti["includes_zero"].clone()), (0.0, json!(true)));
    assert!(f(&ti["point"]) < 0.0);
    assert_eq!(rec["winner"], "N1");
}

/// A synthetic gate record evaluated at both levels.
fn g(id: &str, status: &str, point: Option<f64>, reasons: &[&str]) -> Value {
    json!({"id": id, "status": status, "reasons": reasons, "levels": {"artifact": {"point": point, "pass": status == "pass"},
        "recipe": {"point": point, "pass": status == "pass"}}})
}

fn ext_record(d: &Path, protocol: &str, candidate: &str, status: &str) -> Value {
    let p = d.join(format!("ext-{protocol}-{status}.json"));
    let body = json!({"schema": EXTERNAL_SCHEMA, "protocol": protocol, "candidate": candidate, "status": status});
    std::fs::write(&p, body.to_string()).unwrap();
    json!({"protocol": protocol, "record": p, "sha256": sha256_hex(body.to_string().as_bytes())})
}

#[test]
fn r2_eval2_decision_record() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let all: Vec<Value> = DECISION_GATES.iter().map(|p| g(&format!("g.{p}"), "pass", None, &[])).collect();
    let spec_with = |kind: &str, ext_status: &str| -> DecisionSpec {
        serde_json::from_value(json!({"candidate": "N1", "kind": kind,
            "gates": DECISION_GATES.iter().filter(|p| !EXTERNAL_GATES.contains(p)).map(|p| json!({"protocol": p, "gate": format!("g.{p}")})).collect::<Vec<_>>(),
            "external": EXTERNAL_GATES.iter().map(|p| ext_record(d, p, "N1", if *p == "T7a" { ext_status } else { "pass" })).collect::<Vec<_>>()})).unwrap()
    };
    let spec = |kind: &str| spec_with(kind, "pass");
    assert_eq!(decision_record(&spec("retrained"), &all).unwrap()["outcome"], "pass");
    // One required gate fails: fail; for a calibration-only candidate T5b is reported.
    let mut one = all.clone();
    one[DECISION_GATES.iter().position(|p| *p == "T5b").unwrap()]["status"] = json!("fail");
    let r = decision_record(&spec("retrained"), &one).unwrap();
    assert_eq!((r["outcome"].as_str(), r["failed"].clone()), (Some("fail"), json!(["T5b"])));
    assert_eq!(decision_record(&spec("calibration_only"), &one).unwrap()["outcome"], "pass");
    // A gate not evaluated never passes: the record is incomplete and names it apart.
    let mut ne = all.clone();
    ne[DECISION_GATES.iter().position(|p| *p == "T2").unwrap()]["status"] = json!("not_evaluated");
    let r = decision_record(&spec("retrained"), &ne).unwrap();
    assert_eq!((r["outcome"].as_str(), r["not_evaluated"].clone(), r["failed"].clone()), (Some("incomplete"), json!(["T2"]), json!([])));
    assert_eq!(r["gates"].as_array().unwrap().iter().find(|x| x["protocol"] == "T2").unwrap()["status"], "not_evaluated");
    // An external gate's status is read from its record: a record saying fail fails the decision.
    let r = decision_record(&spec_with("retrained", "fail"), &all).unwrap();
    assert_eq!((r["outcome"].as_str(), r["failed"].clone()), (Some("fail"), json!(["T7a"])));
    let r = decision_record(&spec_with("retrained", "not_evaluated"), &all).unwrap();
    assert_eq!((r["outcome"].as_str(), r["not_evaluated"].clone()), (Some("incomplete"), json!(["T7a"])));
    // A protocol gate mapped to two configuration gates passes only if both pass.
    let mut s2 = spec("retrained");
    s2.gates.push(serde_json::from_value(json!({"protocol": "T3a", "gate": "g.T3a.C", "block": "C"})).unwrap());
    let mut two = all.clone();
    two.push(g("g.T3a.C", "fail", None, &[]));
    assert_eq!(decision_record(&s2, &two).unwrap()["failed"], json!(["T3a"]));
    // A missing gate leaves the record incomplete; a reported gate cannot be required.
    let mut s3 = spec("retrained");
    s3.gates.retain(|e| e.protocol != "T1b");
    assert_eq!(decision_record(&s3, &all).unwrap()["missing"], json!(["T1b"]));
    assert_eq!(decision_record(&s3, &all).unwrap()["outcome"], "incomplete");
    let mut rep = all.clone();
    rep[0]["status"] = json!("reported");
    assert!(decision_record(&spec("retrained"), &rep).is_err());
    // A gate record without a required level is refused.
    let mut lv = all.clone();
    lv[0]["levels"].as_object_mut().unwrap().remove("recipe");
    assert!(format!("{:#}", decision_record(&spec("retrained"), &lv).unwrap_err()).contains("has no recipe level"));
    // External records: bound by digest, of this candidate and gate, with a known status.
    let mut s4 = spec("retrained");
    s4.external[0].sha256 = "0".repeat(64);
    assert!(decision_record(&s4, &all).is_err());
    for (cand, st) in [("N2", "pass"), ("N1", "passed")] {
        let mut s5 = spec("retrained");
        s5.external[0] = serde_json::from_value(ext_record(d, EXTERNAL_GATES[0], cand, st)).unwrap();
        assert!(decision_record(&s5, &all).is_err(), "{cand} {st}");
    }
    assert_eq!(required_levels("T3e"), &[Level::Recipe]);
    assert_eq!(required_levels("T3h"), &[Level::Artifact]);
    assert_eq!(required_levels("T5d"), &[Level::Artifact, Level::Recipe]);
}

#[test]
fn r2_eval2_decision_structure_is_checked_before_any_read() {
    let fx = fixture2();
    let dec = |gates: Value, external: Value| json!({"candidate": "N1", "kind": "retrained", "gates": gates, "external": external});
    let d = fx.dir.path();
    for (tag, gates, external, want) in [
        // T1b is evaluated at the artifact level only; §11.10 requires both levels.
        ("lv", json!([{"protocol": "T1b", "gate": "T1b"}]), json!([]), "exactly the levels"),
        // T3e is a recipe gate: a gate evaluated at both levels is refused.
        ("t3e", json!([{"protocol": "T3e", "gate": "T3e.T"}]), json!([]), "exactly the levels"),
        // A reported gate (no margin) cannot be required.
        ("rep", json!([{"protocol": "T3f", "gate": "T3f.R0"}]), json!([]), "exactly the levels"),
        ("t1d", json!([{"protocol": "T7a", "gate": "T1d"}]), json!([]), "enters as an external record"),
        ("ext", json!([]), json!([ext_record(d, "T1a", "N1", "pass")]), "is not one of the external gates"),
        ("unk", json!([{"protocol": "T9", "gate": "T1a"}]), json!([]), "is not a §11.10 gate"),
    ] {
        let mut cfg = fx.cfg.clone();
        cfg["decision"] = dec(gates, external);
        let e = err(run_value(&fx, &cfg, &format!("dc-{tag}")));
        assert!(e.contains(want), "{tag}: {e}");
        assert!(!d.join(format!("dc-{tag}-margins.json")).exists(), "{tag}");
    }
    // A reported gate at the required levels is refused as required, and accepted as a calibration-only
    // candidate's reported gate.
    let mut cfg = fx.cfg.clone();
    cfg["gates"].as_array_mut().unwrap().push(json!({"kind": "shift", "id": "T3f.R0b", "population": "T.HC", "system": "R0.u", "levels": ["artifact", "recipe"]}));
    cfg["decision"] = dec(json!([{"protocol": "T3f", "gate": "T3f.R0b"}]), json!([]));
    assert!(err(run_value(&fx, &cfg, "dc-r")).contains("is reported only and cannot be required"));
    cfg["decision"]["kind"] = json!("calibration_only");
    let out = run_value(&fx, &cfg, "dc-r2").unwrap();
    let row = out["decision"]["gates"].as_array().unwrap().iter().find(|x| x["protocol"] == "T3f").unwrap().clone();
    assert_eq!((row["role"].as_str(), row["status"].as_str()), (Some("reported"), Some("reported")));
    assert_eq!(out["decision"]["outcome"], "incomplete");
}

#[test]
fn r2_eval2_null_check_record_faults() {
    let dir = tempfile::tempdir().unwrap();
    let planned = json!({"schema": PLANNED_SCHEMA, "joint_pass_probability_at_zero": 0.41, "gates": {
        "T1a.T": {"pass_probability_at_zero": 0.62, "planned_se": 0.001},
        "T1c.T": {"pass_probability_at_zero": 0.99, "planned_se": 0.002},
        "T3c.T": {"pass_probability_at_zero": 0.9, "planned_se": 0.001}}});
    let pp = dir.path().join("planned.json");
    std::fs::write(&pp, planned.to_string()).unwrap();
    let gates = vec![
        g("T1a", "pass", Some(0.0009), &[]),
        g("T1c.T", "fail", Some(0.001), &[]),
        g("T3c.T", "pass", Some(-0.0030), &[]),
        g("T1e", "not_evaluated", None, &["margin wT2.T1e: the margin is 0 or not finite (item 1.10 (a))"]),
        g("T1b", "fail", Some(-0.01), &[]),
    ];
    let np: NullPlan = serde_json::from_value(json!({"gates": [
        {"protocol": "T1a", "gate": "T1a", "block": "T"}, {"protocol": "T1c", "gate": "T1c.T", "block": "T"}, {"protocol": "T3c", "gate": "T3c.T", "block": "T"},
        {"protocol": "T1e", "gate": "T1e", "block": "T"}, {"protocol": "T1b", "gate": "T1b"}],
        "planned": pp, "planned_sha256": sha256_hex(planned.to_string().as_bytes())})).unwrap();
    let r = null_check_record(&np, &gates).unwrap();
    let by = |p: &str| r["gates"].as_array().unwrap().iter().find(|x| x["protocol"] == p).unwrap().clone();
    assert!(by("T1a")["faults"].as_array().unwrap().is_empty()); // 0.0009 within 2.576 x 0.001
    assert!(by("T1c")["faults"][0].as_str().unwrap().starts_with("(b)"));
    assert!(by("T3c")["faults"][0].as_str().unwrap().starts_with("(c)")); // |-0.003| > 0.002576
    assert!(by("T1e")["faults"][0].as_str().unwrap().starts_with("(a)"));
    assert_eq!(by("T1b")["role"], "reported");
    assert!(by("T1b")["faults"].as_array().unwrap().is_empty());
    assert_eq!((r["subset_evaluated"].as_u64(), r["subset_passed"].as_u64(), r["subset_not_evaluated"].as_u64()), (Some(3), Some(2), Some(1)));
    assert_eq!(r["joint_pass_probability_at_zero"], 0.41);
    assert_eq!(r["amendment_required"], true);
    assert_eq!(r["subset_missing"], json!(["T1c.C", "T3a.T", "T3a.C", "T3b.T", "T3c.C", "T3e.T", "T3e.C", "T5d.C"]));
    assert_eq!(r["planned_missing"], json!(["T1e.T"]));
    // Boundaries: exactly 2.576 SEs and exactly 0.95 are as written.
    let gates = vec![g("T1a", "pass", Some(NULL_SE_MULTIPLE * 0.001), &[]), g("T1c.T", "fail", Some(0.0), &[])];
    let planned2 = json!({"schema": PLANNED_SCHEMA, "joint_pass_probability_at_zero": 0.5, "gates": {"T1a.T": {"pass_probability_at_zero": 0.5, "planned_se": 0.001}, "T1c.T": {"pass_probability_at_zero": 0.95, "planned_se": 1.0}}});
    std::fs::write(&pp, planned2.to_string()).unwrap();
    let np2: NullPlan = serde_json::from_value(json!({"gates": [{"protocol": "T1a", "gate": "T1a", "block": "T"}, {"protocol": "T1c", "gate": "T1c.T", "block": "T"}],
        "planned": pp, "planned_sha256": sha256_hex(planned2.to_string().as_bytes())})).unwrap();
    let r = null_check_record(&np2, &gates).unwrap();
    assert!(r["gates"][0]["faults"].as_array().unwrap().is_empty());
    assert!(r["gates"][1]["faults"][0].as_str().unwrap().starts_with("(b)"));
    // Each block's gate takes its own planned values (the review's probe): T1c.C's point is within its own
    // planned SE and outside T1c.T's.
    let planned3 = json!({"schema": PLANNED_SCHEMA, "joint_pass_probability_at_zero": 0.5, "gates": {
        "T1c.T": {"pass_probability_at_zero": 0.99, "planned_se": 0.001}, "T1c.C": {"pass_probability_at_zero": 0.5, "planned_se": 0.002}}});
    std::fs::write(&pp, planned3.to_string()).unwrap();
    let np3: NullPlan = serde_json::from_value(json!({"gates": [{"protocol": "T1c", "gate": "T1c.T", "block": "T"}, {"protocol": "T1c", "gate": "T1c.C", "block": "C"}],
        "planned": pp, "planned_sha256": sha256_hex(planned3.to_string().as_bytes())})).unwrap();
    let r = null_check_record(&np3, &[g("T1c.T", "pass", Some(0.0009), &[]), g("T1c.C", "fail", Some(0.004), &[])]).unwrap();
    assert!(r["gates"][0]["faults"].as_array().unwrap().is_empty());
    assert!(r["gates"][1]["faults"].as_array().unwrap().is_empty(), "{}", r["gates"][1]);
    assert_eq!(r["gates"][1]["key"], "T1c.C");
    // A planned file without B13's joint pass probability, or a tampered one, is refused.
    let planned4 = json!({"schema": PLANNED_SCHEMA, "gates": {}});
    std::fs::write(&pp, planned4.to_string()).unwrap();
    let np4: NullPlan = serde_json::from_value(json!({"gates": [], "planned": pp, "planned_sha256": sha256_hex(planned4.to_string().as_bytes())})).unwrap();
    assert!(format!("{:#}", null_check_record(&np4, &[]).unwrap_err()).contains("joint_pass_probability_at_zero"));
    let mut np5 = np3;
    np5.planned_sha256 = Some("0".repeat(64));
    assert!(null_check_record(&np5, &[]).is_err());
}

#[test]
fn r2_eval2_null_check_end_to_end_and_report_sets() {
    let fx = fixture();
    let mut cfg = fx.cfg.clone();
    cfg["null_check"] = json!(true);
    cfg["released_seed"] = json!(29);
    cfg["blends"] = json!([]);
    cfg["gates"] = json!([
        {"kind": "paired", "id": "T3c.null", "statistic": "human_brier_balanced", "terms": [{"population": "T.N"}],
         "candidate": "Faux.u", "comparator": "R0.u", "levels": ["artifact"], "margin": {"margin": "m_H.T"}},
        {"kind": "paired", "id": "T3c.ungated", "statistic": "human_brier_balanced", "terms": [{"population": "T.N"}],
         "candidate": "Faux.u", "comparator": "R0.u", "levels": ["artifact"]},
        {"kind": "paired", "id": "T1e.recal", "statistic": "brier", "terms": [{"population": "T.R"}],
         "candidate": "Cand.p", "comparator": "Recal.p", "levels": ["artifact"]}]);
    cfg["null_check_plan"] = json!({"gates": [{"protocol": "T3c", "gate": "T3c.null", "block": "T"}]});
    cfg["report_sets"] = json!([{"id": "R0-recal companions", "label": "T1e against R0-recal (item 1.8)", "gates": ["T1e.recal"]}]);
    let out = run_value(&fx, &cfg, "nce").unwrap();
    let nc = &out["null_check"];
    assert_eq!(nc["gates"][0]["status"], gate(&out, "T3c.null")["status"]);
    assert_eq!(nc["subset_evaluated"], 1);
    assert_eq!(nc["planned_missing"], json!(["T3c.T"]));
    assert!(nc["joint_pass_probability_at_zero"].is_null());
    assert_eq!(out["report_sets"]["R0-recal companions"]["gates"][0]["gate"], "T1e.recal");
    // Structural refusals: a gate with a pass rule in a report set; a subset entry without its block, on a
    // block its gate does not read or that the protocol gate has not, on an ungated gate, or twice.
    for (tag, edit, want) in [
        ("rs", Box::new(|c: &mut Value| c["report_sets"][0]["gates"] = json!(["T3c.null"])) as Box<dyn Fn(&mut Value)>, "reported comparisons only"),
        ("nb", Box::new(|c: &mut Value| c["null_check_plan"]["gates"][0] = json!({"protocol": "T3c", "gate": "T3c.null"})), "names no block"),
        ("wb", Box::new(|c: &mut Value| c["null_check_plan"]["gates"][0]["block"] = json!("C")), "does not read block C only"),
        ("pb", Box::new(|c: &mut Value| c["null_check_plan"]["gates"][0] = json!({"protocol": "T1a", "gate": "T3c.null", "block": "C"})), "is not evaluated on block C"),
        ("ug", Box::new(|c: &mut Value| c["null_check_plan"]["gates"][0]["gate"] = json!("T3c.ungated")), "has no pass rule"),
        ("tw", Box::new(|c: &mut Value| c["null_check_plan"]["gates"] = json!([{"protocol": "T3c", "gate": "T3c.null", "block": "T"}, {"protocol": "T3c", "gate": "T3c.null", "block": "T"}])), "twice"),
    ] {
        let mut c = cfg.clone();
        edit(&mut c);
        let e = err(run_value(&fx, &c, &format!("nce-{tag}")));
        assert!(e.contains(want), "{tag}: {e}");
    }
}

// ------------------------------------------------------------------ T4a/T4b, gating on equality

/// B11's slack record (synthetic) for a system on a population's rows.
fn slack_record(d: &Path, tag: &str, rows_sha: &str, system: &str, export: &str, cal: &str, seeds: &[(u64, f64)]) -> (PathBuf, String) {
    let rec = json!({"schema": SLACK_SCHEMA, "population_rows_sha256": rows_sha, "system": system, "export": export,
        "cal_population": {"split": cal, "rows_sha256": "synthetic"}, "cross_fitted": true,
        "seeds": seeds.iter().map(|(s, x)| (s.to_string(), json!({"slack_share_of_b0": x}))).collect::<Map<_, _>>()});
    let p = d.join(format!("slack-{tag}.json"));
    std::fs::write(&p, rec.to_string()).unwrap();
    (p.clone(), sha256_hex(&std::fs::read(&p).unwrap()))
}

/// B12's σ record (synthetic) with one design at `n`.
fn sigma_c_record(d: &Path, tag: &str, sigma: f64, edit: impl FnOnce(&mut Value)) -> (PathBuf, String) {
    let mut rec = json!({"schema": SIGMA_SCHEMA, "bootstrap": {"draws": 2000, "seed": 20261030, "as_pinned": true},
        "inputs": {"rows_sha256": "synthetic-deviter2", "block": "C", "weight_field": {"field": "cohort_weight", "absent": "refused"}},
        "designs": [{"n": 1000, "mix": [0.2, 0.55, 0.25], "sigma": 0.9, "delta": 1.8},
                    {"n": 4000, "mix": [0.2, 0.55, 0.25], "sigma": sigma, "delta": (2.0 * sigma).max(0.01)}]});
    edit(&mut rec);
    let p = d.join(format!("sigma-c-{tag}.json"));
    std::fs::write(&p, rec.to_string()).unwrap();
    (p.clone(), sha256_hex(&std::fs::read(&p).unwrap()))
}

#[test]
fn r2_eval2_slack_record() {
    let fx = fixture();
    let d = fx.dir.path();
    let rows_sha = sha256_hex(&std::fs::read(d.join("cn.jsonl")).unwrap());
    let (p, h) = slack_record(d, "c", &rows_sha, "Cand.m", "mainnet", "cal2_c", &[(17, 0.02), (29, 0.025), (41, 0.031)]);
    // delta_C = max(0.01, 2 x 0.0125) = 0.025 from B12's n = 4,000 design (S1-4).
    let (sp, sh) = sigma_c_record(d, "ok", 0.0125, |_| {});
    let mut cfg = fx.cfg.clone();
    cfg["gates"].as_array_mut().unwrap().push(json!({"kind": "slack", "id": "T4a.C", "population": "C.N", "system": "Cand.m", "record": p,
        "record_sha256": h, "bound": {"source": "delta_c", "record": sp, "sha256": sh}, "levels": ["artifact", "recipe"]}));
    let out = run_value(&fx, &cfg, "sk").unwrap();
    let g = gate(&out, "T4a.C");
    assert_eq!(g["bound"]["value"], 0.025);
    assert_eq!(g["levels"]["artifact"]["pass"], true); // 0.02 <= 0.025
    assert_eq!(g["levels"]["recipe"]["pass"], false); // 0.031 > 0.025; 0.025 itself passes
    assert_eq!(g["levels"]["recipe"]["seeds"][1]["pass"], true);
    assert_eq!(g["status"], "fail");
    // The bound's record: other bytes, an unpinned bootstrap, another weight field or block, no n = 4,000
    // design, another mix, or a delta that is not max(0.01, 2 sigma) are refused.
    let edits: [(&str, Edit, &str); 6] = [
        ("pin", Box::new(|v: &mut Value| v["bootstrap"]["as_pinned"] = json!(false)), "not the pinned one"),
        ("wf", Box::new(|v: &mut Value| v["inputs"]["weight_field"]["field"] = json!("weight")), "weight field is not cohort_weight"),
        ("blk", Box::new(|v: &mut Value| v["inputs"]["block"] = json!("T")), "not on block C"),
        ("n", Box::new(|v: &mut Value| v["designs"][1]["n"] = json!(1000)), "designs at n = 4000"),
        ("mix", Box::new(|v: &mut Value| v["designs"][1]["mix"] = json!([0.2, 0.25, 0.55])), "mix is not 20/55/25"),
        ("dl", Box::new(|v: &mut Value| v["designs"][1]["delta"] = json!(0.03)), "not max(0.01, 2 sigma)"),
    ];
    for (tag, edit, want) in edits {
        let (sp2, sh2) = sigma_c_record(d, tag, 0.0125, edit);
        let mut c = cfg.clone();
        set_gate(&mut c, "T4a.C", |g| g["bound"] = json!({"source": "delta_c", "record": sp2, "sha256": sh2}));
        let e = err(run_value(&fx, &c, &format!("sk-{tag}")));
        assert!(e.contains(want), "{tag}: {e}");
    }
    let mut c = cfg.clone();
    set_gate(&mut c, "T4a.C", |g| g["bound"]["sha256"] = json!("0".repeat(64)));
    assert!(err(run_value(&fx, &c, "sk-h")).contains("hashes to"));
    // A block's bound is fixed: delta_m for T, delta_C for C, sigma_m for the mock.
    let mut c = cfg.clone();
    set_gate(&mut c, "T4a.C", |g| g["bound"] = json!({"source": "sigma_m"}));
    assert!(err(run_value(&fx, &c, "sk-b")).contains("takes the bound delta_c"));
    // The slack record: other bytes, other rows, or not cross-fitted on the mainnet export fitted on CAL-2 C.
    let mut c = cfg.clone();
    set_gate(&mut c, "T4a.C", |g| g["record_sha256"] = json!("0".repeat(64)));
    assert!(err(run_value(&fx, &c, "sk2")).contains("hashes to"));
    for (tag, rs, exp, cal) in [("rows", "00", "mainnet", "cal2_c"), ("exp", rows_sha.as_str(), "product", "cal2_c"), ("cal", rows_sha.as_str(), "mainnet", "cal2_t")] {
        let (p3, h3) = slack_record(d, tag, rs, "Cand.m", exp, cal, &[(17, 0.02)]);
        let mut c = cfg.clone();
        set_gate(&mut c, "T4a.C", |g| {
            g["record"] = json!(p3);
            g["record_sha256"] = json!(h3);
        });
        let e = err(run_value(&fx, &c, &format!("sk3-{tag}")));
        assert!(e.contains(if tag == "rows" { "is not Cand.m's on population C.N's rows" } else { "is not cross-fitted on the mainnet export fitted on cal2_c" }), "{tag}: {e}");
    }
}

#[test]
fn r2_eval2_slack_bounds_and_reports() {
    let fx = fixture();
    let d = fx.dir.path();
    let sha = |f: &str| sha256_hex(&std::fs::read(d.join(f)).unwrap());
    // T4a on T: delta_m = 0.070; T4b on the mock: sigma_m = 0.035; R0's slack reported (no bound), with R0
    // a comparator.
    let (pt, ht) = slack_record(d, "t", &sha("tn.jsonl"), "Cand.p", "product", "cal2_t", &[(17, 0.07), (29, 0.0701), (41, 0.01)]);
    let (pm, hm) = slack_record(d, "m", &sha("mock.jsonl"), "Cand.t", "testnet", "cal2_mock", &[(17, 0.035), (29, 0.01), (41, 0.02)]);
    let (pr, hr) = slack_record(d, "r", &sha("mock.jsonl"), "R0.t", "testnet", "cal2_mock", &[(17, 0.261)]);
    let mut cfg = fx.cfg.clone();
    let gs = cfg["gates"].as_array_mut().unwrap();
    gs.push(json!({"kind": "slack", "id": "T4a.T", "population": "T.N", "system": "Cand.p", "record": pt, "record_sha256": ht, "bound": {"source": "delta_m"}, "levels": ["artifact", "recipe"]}));
    gs.push(json!({"kind": "slack", "id": "T4b", "population": "mock", "system": "Cand.t", "record": pm, "record_sha256": hm, "bound": {"source": "sigma_m"}, "levels": ["artifact", "recipe"]}));
    gs.push(json!({"kind": "slack", "id": "T4b.R0", "population": "mock", "system": "R0.t", "record": pr, "record_sha256": hr, "levels": ["artifact"]}));
    let out = run_value(&fx, &cfg, "skb").unwrap();
    let t = gate(&out, "T4a.T");
    assert_eq!((t["bound"]["value"].as_f64(), t["levels"]["artifact"]["pass"].as_bool(), t["levels"]["recipe"]["pass"].as_bool()), (Some(DELTA_M), Some(true), Some(false)));
    let m = gate(&out, "T4b");
    assert_eq!((m["bound"]["value"].as_f64(), m["status"].as_str()), (Some(SIGMA_M), Some("pass")));
    let r = gate(&out, "T4b.R0");
    assert_eq!(r["status"], "reported");
    assert_eq!(r["levels"]["artifact"]["seeds"][0]["slack_share_of_b0"], 0.261);
    assert!(r["levels"]["artifact"].get("pass").is_none() && r["bound"].is_null());
    assert!(!out["margins"]["gate_candidates"].as_array().unwrap().contains(&json!("R0.t")));
    // A bounded slack makes its system a candidate, which cannot be a comparator.
    let mut c = cfg.clone();
    set_gate(&mut c, "T4b.R0", |g| g["bound"] = json!({"source": "sigma_m"}));
    assert!(err(run_value(&fx, &c, "skb2")).contains("is a gate candidate and is listed as a comparator"));
    // The old free-number bound is no longer accepted.
    let mut c = cfg.clone();
    set_gate(&mut c, "T4b", |g| g["bound"] = json!(0.035));
    assert!(parse_config(&serde_json::to_vec(&c).unwrap()).is_err());
}

#[test]
fn r2_eval2_part2_needs_per_seed_equality() {
    let fx = fixture2();
    let (p, mut e) = release_eval(&fx, "T.N", &[]);
    let ptr = "/systems/Cand.p/seeds/17/balanced_involvement_brier";
    let x = f(e.pointer(ptr).unwrap());
    *e.pointer_mut(ptr).unwrap() = json!(f64::from_bits(x.to_bits() + 1));
    std::fs::write(&p, serde_json::to_vec(&e).unwrap()).unwrap();
    let mut cfg = selection_cfg(&fx, 1.0, 1.0, "ne");
    add_recal2(&fx, &mut cfg);
    cfg["release_evaluations"] = json!({"T.N": p});
    let out = run_value(&fx, &cfg, "ne").unwrap();
    assert_eq!(out["per_seed_equality"]["pass"], false);
    for m in out["measures"].as_array().unwrap() {
        assert_eq!(m["status"], "not_evaluated");
    }
    assert_eq!(out["selection"]["status"], "not_evaluated");
    assert!(!fx.dir.path().join("ne-selection-record-r2.json").exists());
    for id in ["T3f.C", "T5b.T", "T3e.T", "T3h", "T8b"] {
        assert_eq!(gate(&out, id)["status"], "not_evaluated", "{id}");
    }
}

// ------------------------------------------------------------------ S6-1

/// Outputs of the manifest's `reward_v1::aligned_rss` (subnet protocol crate,
/// `subnet/crates/protocol/src/reward_v1.rs` blob abb47bc5e9540a957aac5751579258a23dc96f58 at
/// 45c27a29), recorded by running that function on these vectors (unit
/// slop-a-comp-r2-b10p2fix-probe): (x, y, RSS(x | y), RSS(y | x)). They include
/// adjacent blocks of equal mean whose divisions do not come out even, where
/// merging such blocks would change the floors.
const MANIFEST_RSS: [(&[u32], &[u32], u128, u128); 14] = [
    (&[0, 1, 1, 0], &[1, 1, 2, 2], 2, 2),
    (&[1, 2, 2, 2, 1, 2], &[3, 3, 3, 8, 8, 8], 2, 38),
    (&[1, 0, 0, 1, 1, 0], &[0, 0, 1, 1, 2, 2], 3, 4),
    (&[7, 7, 7], &[10, 20, 30], 0, 200),
    (&[3, 1, 2], &[1, 2, 3], 2, 2),
    (&[5, 5, 9], &[7, 7, 7], 11, 0),
    (&[1, 2, 3, 4, 5], &[5, 4, 3, 2, 1], 10, 10),
    (&[9, 1, 1, 4, 4, 0, 6], &[1, 2, 2, 3, 3, 4, 5], 55, 11),
    (&[1, 0, 0, 0, 0, 1], &[500_000_000, 500_000_000, 500_000_000, 600_000_000, 600_000_000, 600_000_000], 2, 15_000_000_000_000_000),
    (&[1_000_000_000, 0, 999_999_999, 1, 500_000_001, 499_999_999], &[2, 2, 1, 1, 3, 3], 999_999_998_000_000_004, 4),
    (&[366575366, 978522777, 982292506, 363298323, 694978167, 630318738, 955594924, 536927941, 132338767, 12895670, 727845874],
     &[100000000, 500000000, 300000000, 100000000, 300000000, 200000000, 100000000, 200000000, 500000000, 0, 500000000],
     717_203_757_903_046_706, 208_000_000_000_000_000),
    (&[1, 0, 3, 4, 4, 4, 1, 3, 0, 4, 2, 3, 4, 2, 2, 4, 1, 3],
     &[300000000, 0, 500000000, 500000000, 300000000, 0, 200000000, 200000000, 400000000, 300000000, 0, 100000000, 400000000, 200000000, 200000000, 200000000, 300000000, 300000000],
     33, 375_833_333_333_333_334),
    (&[522235187, 82354450, 778289528, 814237937, 84016259, 507077849, 751039397, 549803783, 705282099, 436250829, 906319329, 693776554, 522732612, 743585710, 575460109, 881534035, 231692169, 187599183, 938435306, 639588712, 638422434, 38749624, 144207204, 517420260, 581446545],
     &[100000000, 0, 0, 300000000, 400000000, 0, 300000000, 100000000, 500000000, 300000000, 400000000, 500000000, 200000000, 500000000, 100000000, 400000000, 500000000, 100000000, 300000000, 400000000, 100000000, 400000000, 200000000, 200000000, 100000000],
     1_639_797_044_816_205_373, 521_142_857_142_857_144),
    (&[3, 0, 3, 3, 0, 1, 0, 4, 2, 1, 3, 1, 0, 2, 1, 2, 0, 4, 4, 4, 1, 1, 4, 2, 2, 1, 1, 1, 0, 1, 3, 2],
     &[0, 200000000, 0, 200000000, 200000000, 200000000, 500000000, 500000000, 100000000, 100000000, 0, 400000000, 300000000, 400000000, 300000000, 0, 400000000, 200000000, 100000000, 300000000, 0, 200000000, 500000000, 400000000, 100000000, 300000000, 200000000, 500000000, 100000000, 0, 500000000, 400000000],
     56, 894_666_666_666_666_667),
];

#[test]
fn r2_eval2_aligned_rss_is_the_manifests() {
    // B10's copy reproduces the manifest function's recorded outputs in both directions (no source coupling
    // to the subnet crate; the landing record states the recorded blob).
    for (k, (x, y, xy, yx)) in MANIFEST_RSS.iter().enumerate() {
        assert_eq!(aligned_rss(x, y).unwrap(), *xy, "case {k} x|y");
        assert_eq!(aligned_rss(y, x).unwrap(), *yx, "case {k} y|x");
    }
    // Within d: min RSS <= common d^2, compared exactly; no common document is never near.
    assert!(near_within(4, 4 * 25_000_000u128 * 25_000_000, D_CHECK_D_PPB));
    assert!(!near_within(4, 4 * 25_000_000u128 * 25_000_000 + 1, D_CHECK_D_PPB));
    assert!(!near_within(0, 0, D_CHECK_D_PPB));
    let (c, m) = near_min_rss(&[Some(3), None, Some(1), Some(2)], &[Some(1), Some(9), Some(2), Some(3)]).unwrap();
    assert_eq!((c, m), (3, 2));
    // The min of the two directions: (x | y) = 11 and (y | x) = 0 on case 6.
    assert_eq!(near_min_rss(&[Some(5), Some(5), Some(9)], &[Some(7), Some(7), Some(7)]).unwrap(), (3, 0));
    // Parts per billion as the manifest's Probability::from_f64: half to even, out of range refused.
    assert_eq!((ppb(0.5).unwrap(), ppb(0.1234567894).unwrap(), ppb(1.6e-9).unwrap(), ppb(1.0).unwrap(), ppb(0.0).unwrap()), (500_000_000, 123_456_789, 2, 1_000_000_000, 0));
    assert_eq!((ppb(5e-10).unwrap(), ppb(2.5e-9).unwrap(), ppb(4.5e-9).unwrap()), (0, 2, 4));
    for bad in [1.2, -0.1, f64::NAN, f64::INFINITY] {
        assert!(ppb(bad).is_err(), "{bad}");
    }
}

/// A consumed Test-2 mock of `n` documents with systems whose involvement per
/// (seed, document index) the closure gives, and its bound release evaluation.
fn repl_fixture(n: usize, systems: &[(&str, InvFn)]) -> (Fixture, Value) {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let origins = ["human_only", "mixed", "model_only", "mixed"];
    let rows: Vec<Value> = (0..n)
        .map(|k| {
            let mut r = row(&format!("d{k}"), "test2_mock", origins[k % 4], &format!("mg{}", k % 7), "wikipedia", "primary");
            r["ledger_unit"] = json!(format!("lu{}", k % 13));
            r["w_a8"] = json!(1.0 + (k % 3) as f64 * 0.5);
            r
        })
        .collect();
    write_lines(&d.join("mock.jsonl"), &rows);
    let mut sys = vec![];
    for (name, inv) in systems {
        for s in RECIPE_SEEDS {
            let pp = d.join(format!("pred-{name}-{s}.jsonl"));
            let lines: Vec<Value> = (0..n)
                .map(|k| {
                    let v = inv(s, k);
                    json!({"id": format!("d{k}"), "p_human": 1.0 - v, "p_model_only": v * 0.5, "p_mixed": v * 0.5, "share": v * 0.5})
                })
                .collect();
            write_lines(&pp, &lines);
            sys.push(json!({"name": name, "seed": s, "population": "mock", "predictions": pp}));
        }
    }
    let mut cfg = json!({"schema": CONFIG_SCHEMA, "consume_sealed_splits": ["test2_mock"],
        "populations": [{"id": "mock", "block": "mock", "split": "test2_mock", "rows": d.join("mock.jsonl"),
            "weights": {"rule": "field", "field": "w_a8"}, "unit_field": "ledger_unit"}],
        "systems": sys, "comparators": [], "gates": []});
    let fx = Fixture { dir, cfg: cfg.clone() };
    let (p, _) = release_eval(&fx, "mock", &[]);
    cfg["release_evaluations"] = json!({"mock": p});
    (Fixture { dir: fx.dir, cfg: cfg.clone() }, cfg)
}

/// B12's σ′ record (synthetic) naming the system's three seed files.
fn sigma_prime_record(fx: &Fixture, system: &str, sigma_scaled: u128, edit: impl FnOnce(&mut Value)) -> Value {
    let d = fx.dir.path();
    let names: Vec<String> = RECIPE_SEEDS.iter().map(|s| format!("{system}:{s}")).collect();
    let preds: Map<String, Value> = RECIPE_SEEDS
        .iter()
        .zip(&names)
        .map(|(s, n)| (n.clone(), json!({"sha256": sha256_hex(&std::fs::read(sys_path(&fx.cfg, "mock", system, *s)).unwrap())})))
        .collect();
    let m = u128::from(SIGMA_M_SCALED);
    let eff = sigma_scaled.max(m);
    let mut rec = json!({"schema": SIGMA_SCHEMA, "systems": names, "bootstrap": {"draws": 2000, "seed": 20261030, "as_pinned": true},
        "inputs": {"rows_sha256": sha256_hex(&std::fs::read(d.join("mock.jsonl")).unwrap()), "block": null,
                   "weight_field": {"field": "w_a8", "absent": "refused"}, "predictions": preds},
        "designs": [{"n": 4000, "mix": [0.2, 0.55, 0.25], "sigma": 0.5, "sigma_scaled": "500000000000000000", "sigma_eff": null, "delta": 1.0},
                    {"n": 1000, "mix": [0.2, 0.55, 0.25], "sigma": sigma_scaled as f64 / 1e18, "sigma_scaled": sigma_scaled.to_string(),
                     "sigma_eff": {"sigma_m_scaled": m.to_string(), "sigma_eff_scaled": eff.to_string(), "delta_eff_scaled": (10_000_000_000_000_000u128).max(2 * eff).to_string()},
                     "delta": (2.0 * sigma_scaled as f64 / 1e18).max(0.01)}]});
    edit(&mut rec);
    rec
}

fn with_sigma(fx: &Fixture, cfg: &Value, tag: &str, rec: &Value) -> Value {
    let p = fx.dir.path().join(format!("sigma-{tag}.json"));
    std::fs::write(&p, rec.to_string()).unwrap();
    let mut c = cfg.clone();
    c["replacement"]["sigma"] = json!(p);
    c["replacement"]["sigma_sha256"] = json!(sha256_hex(rec.to_string().as_bytes()));
    c
}

#[test]
fn r2_eval2_replacement_sigma_and_fpr() {
    let inv = |s: u64, k: usize| -> f64 {
        // Every fourth document of seed 41 sits exactly at 0.5 (flag 1 - p_human = 0.5).
        if s == 41 && k.is_multiple_of(8) { 0.5 } else { unif(&format!("r:{s}:{k}")) }
    };
    let (fx, mut cfg) = repl_fixture(40, &[("R0p.t", &inv)]);
    cfg["replacement"] = json!({"population": "mock", "system": "R0p.t", "human_fpr": [{"population": "mock", "system": "R0p.t"}]});
    // sigma' below sigma_m: sigma_eff = sigma_m and delta = 2 sigma_m = 0.07 (item 1.2: the bar never falls),
    // from the n = 1,000 design, not the first.
    let c = with_sigma(&fx, &cfg, "ok", &sigma_prime_record(&fx, "R0p.t", 20_000_000_000_000_000, |_| {}));
    let out = run_value(&fx, &c, "rp").unwrap();
    assert_eq!(out["per_seed_equality"]["pass"], true, "{}", out["per_seed_equality"]);
    let s = &out["replacement"]["sigma"];
    assert_eq!((s["sigma_scaled"].as_str(), s["sigma_eff_scaled"].as_str(), s["delta_scaled"].as_str()),
        (Some("20000000000000000"), Some("35000000000000000"), Some("70000000000000000")));
    assert_eq!((f(&s["sigma_prime"]), f(&s["sigma_eff"]), f(&s["delta"])), (0.02, 0.035, 0.07));
    // sigma' above sigma_m: sigma_eff = sigma'.
    let c = with_sigma(&fx, &cfg, "hi", &sigma_prime_record(&fx, "R0p.t", 41_000_000_000_000_000, |_| {}));
    let s = run_value(&fx, &c, "rp-hi").unwrap()["replacement"]["sigma"].clone();
    assert_eq!((s["sigma_eff_scaled"].as_str(), s["delta_scaled"].as_str()), (Some("41000000000000000"), Some("82000000000000000")));
    // The record must carry sigma_eff with the manifest's sigma_m, the 20/55/25 n = 1,000 design, the pinned
    // bootstrap, the mock's W_A8 field, and R_0''s seed 17, 29 and 41 files in that order, with item 1.2's arithmetic.
    let edits: [(&str, Edit, &str); 9] = [
        ("eff", Box::new(|v: &mut Value| v["designs"][1]["sigma_eff"] = Value::Null), "has no sigma_eff"),
        ("sm", Box::new(|v: &mut Value| v["designs"][1]["sigma_eff"]["sigma_m_scaled"] = json!("30000000000000000")), "has no sigma_eff"),
        ("ar", Box::new(|v: &mut Value| v["designs"][1]["sigma_eff"]["delta_eff_scaled"] = json!("40000000000000000")), "does not follow item 1.2"),
        ("mix", Box::new(|v: &mut Value| v["designs"][1]["mix"] = json!([0.5, 0.25, 0.25])), "mix is not 20/55/25"),
        ("n", Box::new(|v: &mut Value| {
            v["designs"].as_array_mut().unwrap().remove(1);
        }), "designs at n = 1000"),
        ("pin", Box::new(|v: &mut Value| v["bootstrap"]["as_pinned"] = json!(false)), "not the pinned one"),
        ("wf", Box::new(|v: &mut Value| v["inputs"]["weight_field"] = json!({"field": "weight", "absent": "1"})), "weight field is not w_a8"),
        ("ord", Box::new(|v: &mut Value| {
            let a = v["systems"][0].clone();
            v["systems"][0] = v["systems"][1].clone();
            v["systems"][1] = a;
        }), "is not R0p.t's seed 17 predictions"),
        ("rows", Box::new(|v: &mut Value| v["inputs"]["rows_sha256"] = json!("00")), "not on the consumed Test-2 mock's rows"),
    ];
    for (tag, edit, want) in edits {
        let c = with_sigma(&fx, &cfg, tag, &sigma_prime_record(&fx, "R0p.t", 20_000_000_000_000_000, edit));
        let e = err(run_value(&fx, &c, &format!("rp-{tag}")));
        assert!(e.contains(want), "{tag}: {e}");
    }
    // The pure-human counts at 0.5 are the bound evaluator's (flag >= 0.5, the ties at exactly 0.5 included).
    let out = run_value(&fx, &cfg, "rp-fpr").unwrap();
    let r = &out["replacement"]["human_fpr_at_0_5"]["entries"][0]["seeds"];
    for s in RECIPE_SEEDS {
        let k = s.to_string();
        assert_eq!(r[&k]["flags"], out["systems"]["mock"]["R0p.t"][&k]["bound"]["human_fpr_at_0_5"]["count"], "{s}");
    }
    let want41 = (0..40).filter(|&k| k % 4 == 0 && inv(41, k) >= 0.5).count();
    assert_eq!(r["41"]["flags"], want41);
    assert!((0..40).any(|k| k % 4 == 0 && inv(41, k) == 0.5));
    // A population that is not the consumed mock, or a system without its three seeds, is refused before any read.
    let mut c = cfg.clone();
    c["replacement"]["system"] = json!("Other.t");
    assert!(err(run_value(&fx, &c, "rp-sys")).contains("needs seeds"));
    let mut c = cfg.clone();
    c["systems"].as_array_mut().unwrap().retain(|s| s["seed"] != 41);
    assert!(err(run_value(&fx, &c, "rp-s41")).contains("needs seeds"));
}

#[test]
fn r2_eval2_replacement_d_check() {
    // 1,100 documents: S = the first 1,024, R = the other 76. Batch 0 is S; every other batch is R plus 948
    // documents of S. Seed 29 is two levels on S and 0 or 1 on R; seed 41 rises over S and is 0.5 on R, so
    // RSS(29 | 41) = 0 on S (seed 29 is a non-decreasing map of seed 41) while RSS(41 | 29) is large; seed 17
    // is unrelated. Only the pair 29-41 falls within 0.025, and only on batch 0.
    const N: usize = 1100;
    let near = |s: u64, k: usize| -> f64 {
        match (s, k < 1024) {
            (29, true) => if k < 512 { 0.25 } else { 0.75 },
            (29, false) => (k % 2) as f64,
            (41, true) => (k as f64 + 0.5) / 1024.0,
            (41, false) => 0.5,
            _ => unif(&format!("s17:{k}")),
        }
    };
    let far = |s: u64, k: usize| -> f64 { if s == 41 { unif(&format!("f41:{k}")) } else { near(s, k) } };
    let (fx, cfg) = repl_fixture(N, &[("Near.t", &near), ("Far.t", &far)]);
    let d = fx.dir.path();
    let id = |k: usize| json!(format!("d{k}"));
    let mut lines = vec![json!({"batch": 0, "ids": (0..1024).map(id).collect::<Vec<_>>()})];
    for b in 1..D_CHECK_BATCHES {
        let ids: Vec<Value> = (0..948).map(|j| id((b + j) % 1024)).chain((1024..N).map(id)).collect();
        lines.push(json!({"batch": b, "ids": ids}));
    }
    let bp = d.join("batches.jsonl");
    write_lines(&bp, &lines);
    let bh = sha256_hex(&std::fs::read(&bp).unwrap());
    let dc = |system: &str| -> Value {
        let mut c = cfg.clone();
        c["replacement"] = json!({"population": "mock", "system": system, "d_check": {"batches": bp, "batches_sha256": bh}});
        c
    };
    let out = run_value(&fx, &dc("Near.t"), "dc1").unwrap();
    let r = &out["replacement"]["d_check"];
    assert_eq!(r["pairs"][0]["seeds"], json!([17, 29]));
    assert_eq!((r["pairs"][0]["batches_within"].as_u64(), r["pairs"][1]["batches_within"].as_u64(), r["pairs"][2]["batches_within"].as_u64()), (Some(0), Some(0), Some(1)));
    assert_eq!(r["pairs"][2]["min_distance_ppb"], 0);
    assert_eq!(r["d_stays"], false);
    // With seed 41 unrelated, no pair falls within 0.025 and d stays.
    let out = run_value(&fx, &dc("Far.t"), "dc2").unwrap();
    let r = &out["replacement"]["d_check"];
    assert!(r["pairs"].as_array().unwrap().iter().all(|p| p["batches_within"] == 0));
    assert_eq!(r["d_stays"], true);
    // A batch with a repeated document, a short batch, or a batches file that is not its digest is refused.
    let mut l2 = lines.clone();
    l2[5]["ids"][3] = l2[5]["ids"][4].clone();
    let bp2 = d.join("batches2.jsonl");
    write_lines(&bp2, &l2);
    let mut c = dc("Near.t");
    c["replacement"]["d_check"] = json!({"batches": bp2, "batches_sha256": sha256_hex(&std::fs::read(&bp2).unwrap())});
    assert!(err(run_value(&fx, &c, "dc3")).contains("a document twice"));
    let bp3 = d.join("batches3.jsonl");
    write_lines(&bp3, &lines[..10]);
    let mut c = dc("Near.t");
    c["replacement"]["d_check"] = json!({"batches": bp3, "batches_sha256": sha256_hex(&std::fs::read(&bp3).unwrap())});
    assert!(err(run_value(&fx, &c, "dc4")).contains("10 batches, not 2000"));
    let mut c = dc("Near.t");
    c["replacement"]["d_check"]["batches_sha256"] = json!("0".repeat(64));
    assert!(err(run_value(&fx, &c, "dc5")).contains("the batches file is not"));
}

#[test]
fn r2_eval2_config_examples_parse() {
    // Every part-2 configuration item is closed: an unknown field is refused.
    let fx = fixture2();
    assert!(parse_config(&serde_json::to_vec(&fx.cfg).unwrap()).is_ok());
    let mut cfg = fx.cfg.clone();
    set_gate(&mut cfg, "T3f.C", |g| g["unknown"] = json!(1));
    assert!(parse_config(&serde_json::to_vec(&cfg).unwrap()).is_err());
    let mut cfg = fx.cfg.clone();
    cfg["measures"][0]["unknown"] = json!(1);
    assert!(parse_config(&serde_json::to_vec(&cfg).unwrap()).is_err());
    let mut cfg = fx.cfg.clone();
    cfg["populations"][4]["probe"]["unknown"] = json!(1);
    assert!(parse_config(&serde_json::to_vec(&cfg).unwrap()).is_err());
    // FS pairs carry no free coefficient.
    let mut cfg = fx.cfg.clone();
    set_gate(&mut cfg, "T5b.T", |g| g["pairs"][0]["coef"] = json!(0.5));
    assert!(parse_config(&serde_json::to_vec(&cfg).unwrap()).is_err());
}
