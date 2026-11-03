//! Synthetic-data tests only: no corpus text, no model or detector calls.
use crate::load::*;
use crate::run::*;
use crate::space::*;
use crate::stats::*;
use crate::tasks::*;
use crate::v2::*;
use serde_json::json;
use std::collections::{BTreeMap, HashMap};

/// Deterministic pseudo-random numbers for fixtures.
struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> f64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((self.0 >> 11) as f64) / ((1u64 << 53) as f64)
    }
}

#[test]
fn chacha20_matches_rfc8439_block_vector() {
    // RFC 8439 section 2.3.2: key 00..1f, counter 1, nonce 00:00:00:09 00:00:00:4a 00:00:00:00.
    let mut key = [0u32; 8];
    for (i, k) in key.iter_mut().enumerate() {
        let b = [4 * i as u8, 4 * i as u8 + 1, 4 * i as u8 + 2, 4 * i as u8 + 3];
        *k = u32::from_le_bytes(b);
    }
    let out = chacha_block(&key, 1, 0x0900_0000, 0x4a00_0000, 0);
    assert_eq!(&out[..4], &[0xe4e7_f110, 0x1559_3bd1, 0x1fdd_0f50, 0xc471_20a3]);
    assert_eq!(out[15], 0x4e3c_50a2);
}

#[test]
fn chacha20_stream_and_uniform_draws_are_deterministic() {
    let mut a = ChaCha20::from_seed([7u8; 32]);
    let mut b = ChaCha20::from_seed([7u8; 32]);
    let xs: Vec<u32> = (0..100).map(|_| a.below(13)).collect();
    let ys: Vec<u32> = (0..100).map(|_| b.below(13)).collect();
    assert_eq!(xs, ys);
    assert!(xs.iter().all(|&x| x < 13));
    let mut counts = [0usize; 4];
    let mut c = ChaCha20::from_seed([1u8; 32]);
    for _ in 0..40_000 {
        counts[c.below(4) as usize] += 1;
    }
    assert!(counts.iter().all(|&n| (9_500..10_500).contains(&n)));
}

fn item(s: f64, pos: bool, w: f64) -> Item {
    Item { s, pos, unit: 0, w }
}

#[test]
fn weighted_auc_counts_ties_half_and_respects_weights() {
    let r = Roc::new(vec![item(0.9, true, 1.0), item(0.4, true, 1.0), item(0.5, false, 1.0), item(0.1, false, 1.0)]);
    assert!((r.eval(None).auc - 0.75).abs() < 1e-15);
    let r = Roc::new(vec![item(0.5, true, 1.0), item(0.5, false, 1.0)]);
    assert!((r.eval(None).auc - 0.5).abs() < 1e-15);
    // Upweighting the misranked positive lowers the AUC to (1*2 + 3*1)/(4*2).
    let r = Roc::new(vec![item(0.9, true, 1.0), item(0.4, true, 3.0), item(0.5, false, 1.0), item(0.1, false, 1.0)]);
    assert!((r.eval(None).auc - 5.0 / 8.0).abs() < 1e-15);
    // Multiplicities scale units.
    let mut items = vec![item(0.9, true, 1.0), item(0.1, false, 1.0)];
    items.push(Item { s: 0.2, pos: true, unit: 1, w: 1.0 });
    items.push(Item { s: 0.3, pos: false, unit: 1, w: 1.0 });
    let r = Roc::new(items);
    let m = [1.0, 0.0];
    assert!((r.eval(Some(&m)).auc - 1.0).abs() < 1e-15);
}

#[test]
fn eer_and_fpr_interpolate_the_roc() {
    let r = Roc::new(vec![item(0.9, true, 1.0), item(0.8, true, 1.0), item(0.2, false, 1.0), item(0.1, false, 1.0)]);
    let o = r.eval(None);
    assert_eq!(o.auc, 1.0);
    assert!(o.eer.abs() < 1e-15 && o.fpr90.abs() < 1e-15);
    // Fully tied scores: the ROC is the diagonal, EER 0.5, FPR at TPR 0.9 is 0.9.
    let r = Roc::new(vec![item(0.5, true, 1.0), item(0.5, false, 1.0)]);
    let o = r.eval(None);
    assert!((o.eer - 0.5).abs() < 1e-12 && (o.fpr90 - 0.9).abs() < 1e-12 && (o.fpr95 - 0.95).abs() < 1e-12);
}

#[test]
fn type7_quantile() {
    let v = [1.0, 2.0, 3.0, 4.0];
    assert_eq!(quantile(&v, 0.0), 1.0);
    assert_eq!(quantile(&v, 1.0), 4.0);
    assert!((quantile(&v, 0.5) - 2.5).abs() < 1e-15);
    assert!((quantile(&v, 0.9) - 3.7).abs() < 1e-12);
}

#[test]
fn logistic_fusion_weights_the_informative_score() {
    let mut rng = Lcg(3);
    let (mut x, mut y, mut w) = (Vec::new(), Vec::new(), Vec::new());
    for i in 0..2000 {
        let pos = i % 5 == 0;
        let z1 = rng.next() * 2.0 - 1.0 + if pos { 1.5 } else { 0.0 };
        let z2 = rng.next() * 2.0 - 1.0;
        x.push([z1, z2]);
        y.push(pos);
        w.push(1.0);
    }
    let b = logistic_fit(&x, &y, &w, 1e-3).unwrap();
    assert!(b[1] > 1.0 && b[2].abs() < 0.5 * b[1]);
}

fn test_inputs(n_ids: usize) -> Inputs {
    Inputs {
        keys: vec![KeyKind::Avail { doc_scope: false }; n_ids - 3],
        ..Default::default()
    }
}

#[test]
fn standardization_and_cosine_match_a_dense_reference() {
    let n_ids = 40usize;
    let inp = test_inputs(n_ids);
    let rep = Rep::Cfg { set: CfgSet::All, merged: false };
    let mut rng = Lcg(11);
    let ndocs = 24;
    // Unequal author sizes (3 and 4 documents) exercise the 1/n weights.
    let groups: Vec<usize> = (0..ndocs)
        .map(|i| if i < 12 { i / 3 } else { 10 + (i - 12) / 4 })
        .collect();
    let raws: Vec<Sparse> = (0..ndocs)
        .map(|_| {
            (0..n_ids as u32)
                .filter_map(|id| {
                    let p = rng.next();
                    (p < 0.3 + 0.01 * id as f64).then(|| (id, rng.next() * 2.0 + 0.01))
                })
                .collect()
        })
        .collect();
    let refs: Vec<&Sparse> = raws.iter().collect();
    let st = fit_stats(&inp, rep, &refs, &groups);
    // Dense reference.
    let dense: Vec<Vec<f64>> = raws
        .iter()
        .map(|r| {
            let mut v = vec![0.0; n_ids];
            for &(i, x) in r {
                v[i as usize] = x;
            }
            v
        })
        .collect();
    let mut sizes: HashMap<usize, f64> = HashMap::new();
    for g in &groups {
        *sizes.entry(*g).or_default() += 1.0;
    }
    let w: Vec<f64> = groups.iter().map(|g| 1.0 / sizes[g]).collect();
    let ws: f64 = w.iter().sum();
    let vocab: Vec<usize> = (0..n_ids)
        .filter(|&j| dense.iter().filter(|v| v[j] > 0.0).count() >= 2)
        .collect();
    assert_eq!(st.vocab, vocab.iter().map(|&j| j as u32).collect::<Vec<_>>());
    for (d, &j) in vocab.iter().enumerate() {
        let m: f64 = dense.iter().zip(&w).map(|(v, wi)| wi * v[j]).sum::<f64>() / ws;
        let var: f64 = dense.iter().zip(&w).map(|(v, wi)| wi * (v[j] - m).powi(2)).sum::<f64>() / ws;
        assert!((st.mean[d] - m).abs() < 1e-12);
        assert!((st.sd_total[d] - var.sqrt().max(SD_FLOOR)).abs() < 1e-12);
        let mut within = 0.0;
        for g in sizes.keys() {
            let idx: Vec<usize> = (0..ndocs).filter(|&i| groups[i] == *g).collect();
            let am = idx.iter().map(|&i| dense[i][j]).sum::<f64>() / idx.len() as f64;
            within += idx.iter().map(|&i| w[i] * (dense[i][j] - am).powi(2)).sum::<f64>();
        }
        assert!((st.sd_within[d] - (within / ws).sqrt().max(SD_FLOOR)).abs() < 1e-12);
    }
    for scorer in [Scorer::S1, Scorer::S2] {
        let model = Model::new(Config { rep, scorer }, Text::Full, &st);
        let z = |i: usize| -> Vec<f64> {
            vocab
                .iter()
                .enumerate()
                .map(|(d, &j)| (dense[i][j] - model.mean[d]) / model.scale[d])
                .collect()
        };
        let unit = |v: Vec<f64>| -> Vec<f64> {
            let n = v.iter().map(|x| x * x).sum::<f64>().sqrt();
            v.into_iter().map(|x| x / n).collect()
        };
        let preps: Vec<Prepared> = raws.iter().map(|r| model.prepare(r)).collect();
        for refs_idx in [vec![0usize], vec![0, 1, 2], vec![3, 4, 5, 6, 7]] {
            let mut pm = vec![0.0; vocab.len()];
            for &i in &refs_idx {
                for (a, b) in pm.iter_mut().zip(unit(z(i))) {
                    *a += b / refs_idx.len() as f64;
                }
            }
            let pm = unit(pm);
            let prof = profile(&model, &refs_idx.iter().map(|&i| &preps[i]).collect::<Vec<_>>());
            for q in 8..ndocs {
                let want: f64 = unit(z(q)).iter().zip(&pm).map(|(a, b)| a * b).sum();
                let got = score(&prof, &preps[q]);
                assert!((want - got).abs() < 1e-12, "{want} vs {got}");
            }
        }
    }
}

#[test]
fn cfg_projection_merges_pools_restricts_and_adds_document_flags() {
    let mut merged = HashMap::new();
    let names = [
        "availability/window/Partial",
        "availability/unresolved_token",
        "fallback_complete/leaf/sigA",
        "native_complete/leaf/sigA",
        "partial/leaf/sigA",
        "partial/production/sigB",
    ];
    let keys: Vec<KeyKind> = names.iter().map(|k| parse_key(k, &mut merged).unwrap()).collect();
    assert_eq!(merged.len(), 2);
    let mut inp = Inputs {
        keys,
        n_merged: merged.len(),
        ..Default::default()
    };
    inp.cfg = vec![Some(CfgRow {
        tokens: 10.0,
        complete: 4.0,
        projection_available: true,
        search_exhaustive: false,
        execution_failures: 0,
        keys: vec![0, 1, 2, 3, 4, 5],
        vals: vec![2.0, 1.0, 1.0, 3.0, 4.0, 5.0],
    })];
    let l = |x: f64| x.ln_1p();
    let pooled_leaf = raw(&inp, 0, Rep::Cfg { set: CfgSet::LeafGrammar, merged: false }, Text::Full).unwrap();
    assert_eq!(pooled_leaf, vec![(2, l(0.1)), (3, l(0.3)), (4, l(0.4))]);
    let merged_leaf = raw(&inp, 0, Rep::Cfg { set: CfgSet::LeafGrammar, merged: true }, Text::Full).unwrap();
    let mid = 6 + 3; // keys + fixed flags
    assert_eq!(merged_leaf, vec![(mid, l(0.8))]);
    let all = raw(&inp, 0, Rep::Cfg { set: CfgSet::All, merged: false }, Text::Full).unwrap();
    // window count is document-scope (denominator 1); unresolved tokens per token.
    assert_eq!(all[0], (0, l(2.0)));
    assert_eq!(all[1], (1, l(0.1)));
    // projection_available and has_token_opportunities flags present, search_exhaustive absent.
    assert!(all.contains(&(6, l(1.0))) && all.contains(&(8, l(1.0))) && !all.iter().any(|a| a.0 == 7));
    let structure = raw(&inp, 0, Rep::Cfg { set: CfgSet::Structure, merged: false }, Text::Full).unwrap();
    assert_eq!(structure, vec![(5, l(0.5))]);
    // Restricted: complete pools only, complete-token denominator, no availability.
    let r = raw(&inp, 0, Rep::Cfg { set: CfgSet::All, merged: false }, Text::Restricted).unwrap();
    assert_eq!(r, vec![(2, l(0.25)), (3, l(0.75))]);
    let rm = raw(&inp, 0, Rep::Cfg { set: CfgSet::All, merged: true }, Text::Restricted).unwrap();
    assert_eq!(rm, vec![(mid, l(1.0))]);
}

#[test]
fn stylometric_vocabulary_ranks_by_document_frequency_then_bytes() {
    let mut inp = Inputs {
        char3: (0..2500).map(|i| format!("{:04}", 9999 - i)).collect(),
        ..Default::default()
    };
    inp.char3.push("aaa".into());
    let raws: Vec<Sparse> = (0..3)
        .map(|d| {
            (0..2501u32)
                .filter(|g| d == 0 || g % 2 == 0 || *g == 2500)
                .map(|g| (N_FW as u32 + g, 1.0))
                .collect()
        })
        .collect();
    let refs: Vec<&Sparse> = raws.iter().collect();
    let v = fit_vocab(&inp, Rep::Stylo, &refs);
    assert_eq!(v.len(), N_FW + CHAR3_VOCAB);
    assert!(v.contains(&(N_FW as u32 + 2500)));
    // 1,251 grams appear in all three documents; the remaining 749 slots go to
    // the smallest byte strings among the singletons (largest indices).
    assert!(v.contains(&(N_FW as u32 + 2499)) && !v.contains(&(N_FW as u32 + 1)));
}

#[test]
fn t3_metrics_follow_the_negative_rules() {
    let st = T3Stratum {
        name: "s".into(),
        targets: vec![
            T3Target { writer: "x".into(), profile: vec![], has_train: true, sources: vec![100] },
            T3Target { writer: "y".into(), profile: vec![], has_train: false, sources: vec![] },
        ],
        records: vec![
            T3Rec { doc: 1, target: 0, kind: "full".into(), family: "f".into(), parent_writer: Some("p".into()), source: Some(100) },
            T3Rec { doc: 2, target: 1, kind: "mirror".into(), family: "f".into(), parent_writer: Some("q".into()), source: None },
            T3Rec { doc: 3, target: 1, kind: "mirror".into(), family: "f".into(), parent_writer: Some("x".into()), source: None },
        ],
    };
    let sc = T3Scores {
        rec: vec![vec![0.5, 0.2, 0.9], vec![0.1, 0.6, 0.7]],
        src: vec![vec![0.6], vec![]],
    };
    let m = t3_target_metrics(&st, &sc);
    // Target x: record 3 (parent by x) is excluded; negatives 0.2 and own source 0.6.
    assert!((m["auc"][0] - 0.5).abs() < 1e-15);
    assert!((m["auc_other_targets_only"][0] - 1.0).abs() < 1e-15);
    assert_eq!(m["own_source_win_share"][0], 0.0);
    // Target y: positives 0.6, 0.7 against x's record 0.1.
    assert!((m["auc"][1] - 1.0).abs() < 1e-15);
    assert!(m["own_source_win_share"][1].is_nan());
    assert!(m["auc_targets_with_train_split:true"][1].is_nan());
}

// ------------------------------------------------------------ protocol v2

fn fusion_fixture(seed: u64, n: usize, shift: [f64; 3]) -> (Vec<[f64; 3]>, Vec<bool>, Vec<f64>) {
    let mut rng = Lcg(seed);
    let (mut x, mut y, mut w) = (Vec::new(), Vec::new(), Vec::new());
    for i in 0..n {
        let pos = i % 5 == 0;
        let mut v = [0.0; 3];
        for (j, s) in shift.iter().enumerate() {
            v[j] = rng.next() * 2.0 - 1.0 + if pos { *s } else { 0.0 };
        }
        x.push(v);
        y.push(pos);
        w.push(0.5 + rng.next());
    }
    (x, y, w)
}

#[test]
fn three_slope_fit_equals_the_two_slope_fit_on_a_null_third_score() {
    let (x, y, w) = fusion_fixture(5, 3000, [1.2, 0.4, 0.0]);
    let x2: Vec<[f64; 2]> = x.iter().map(|v| [v[0], v[1]]).collect();
    let x3: Vec<[f64; 3]> = x.iter().map(|v| [v[0], v[1], 0.0]).collect();
    let b2 = logistic_fit(&x2, &y, &w, 1e-3).unwrap();
    let f3 = logistic_fit3(&x3, &y, &w, 1e-3).unwrap();
    assert!(f3.converged && f3.iterations < 200);
    assert_eq!([f3.beta[0], f3.beta[1], f3.beta[2]], b2);
    assert_eq!(f3.beta[3], 0.0);
}

#[test]
fn three_slope_fit_weights_each_informative_score() {
    let (x, y, w) = fusion_fixture(9, 4000, [1.5, 0.0, 0.7]);
    let f = logistic_fit3(&x, &y, &w, 1e-3).unwrap();
    assert!(f.converged);
    assert!(f.beta[1] > f.beta[3] && f.beta[3] > 0.3, "{:?}", f.beta);
    assert!(f.beta[2].abs() < 0.3 * f.beta[3], "{:?}", f.beta);
    // The class-balanced intercept is invariant to scaling all weights.
    let w10: Vec<f64> = w.iter().map(|v| 10.0 * v).collect();
    let g = logistic_fit3(&x, &y, &w10, 1e-3).unwrap();
    for i in 0..4 {
        assert!((f.beta[i] - g.beta[i]).abs() < 1e-9);
    }
}

#[test]
fn three_slope_fit_rejects_a_singular_hessian_and_bad_inputs() {
    let (x, y, w) = fusion_fixture(2, 500, [1.0, 0.0, 0.0]);
    let same: Vec<[f64; 3]> = x.iter().map(|v| [v[0], v[0], v[0]]).collect();
    assert!(logistic_fit3(&same, &y, &w, 0.0).is_err());
    let one_class = vec![true; y.len()];
    assert!(logistic_fit3(&x, &one_class, &w, 1e-3).is_err());
    let mut bad = x.clone();
    bad[3][1] = f64::NAN;
    assert!(logistic_fit3(&bad, &y, &w, 1e-3).is_err());
}

fn trained_fixture(beta: [f64; 4]) -> Trained {
    let mut znorm = BTreeMap::new();
    znorm.insert('A', [(0.1, 0.2), (0.15, 0.25), (0.2, 0.3)]);
    znorm.insert('B', [(0.3, 0.1), (0.35, 0.12), (0.4, 0.14)]);
    znorm.insert('C', [(0.01, 0.05), (0.02, 0.06), (0.03, 0.07)]);
    Trained {
        models: BTreeMap::new(),
        znorm,
        fusion: BTreeMap::new(),
        thresholds: BTreeMap::new(),
        record: serde_json::Value::Null,
        fusion_f: Some(beta),
        record_v2: serde_json::Value::Null,
    }
}

#[test]
fn arm_f_fuses_b_c_a_with_per_k_impostor_constants() {
    let beta = [-1.0, 0.8, 1.1, 0.3];
    let tr = trained_fixture(beta);
    let mk = |base: f64| -> Vec<Mat> {
        (0..3)
            .map(|ki| Mat { per_dom: vec![vec![base + ki as f64 * 0.01, base + 0.5], vec![base - 0.2]] })
            .collect()
    };
    let mut mats = BTreeMap::new();
    mats.insert('A', mk(0.6));
    mats.insert('B', mk(0.7));
    mats.insert('C', mk(0.1));
    let f = tr.fused_f_mats(&mats);
    for ki in 0..3 {
        for d in 0..2 {
            for i in 0..f[ki].per_dom[d].len() {
                let s = |c: char| mats[&c][ki].per_dom[d][i];
                // Same expression order as v1's two-component fuse: b * (s - m) / d.
                let t = |b: f64, c: char| b * (s(c) - tr.znorm[&c][ki].0) / tr.znorm[&c][ki].1;
                let want = beta[0] + t(beta[1], 'B') + t(beta[2], 'C') + t(beta[3], 'A');
                assert_eq!(f[ki].per_dom[d][i], want);
            }
        }
    }
    let t3 = |base: f64| T3Scores { rec: vec![vec![base, base + 0.1]], src: vec![vec![base - 0.1]] };
    let mut sc = BTreeMap::new();
    sc.insert('A', t3(0.6));
    sc.insert('B', t3(0.7));
    sc.insert('C', t3(0.1));
    let g = t3_map3(&sc[&'B'], &sc[&'C'], &sc[&'A'], |b, c, a| tr.fuse_f(2, [b, c, a]));
    let t = |b: f64, c: char| b * (sc[&c].src[0][0] - tr.znorm[&c][2].0) / tr.znorm[&c][2].1;
    assert_eq!(g.src[0][0], beta[0] + t(beta[1], 'B') + t(beta[2], 'C') + t(beta[3], 'A'));
}

#[test]
fn gate_comparison_finds_changed_numbers_and_keys() {
    let a = json!({"m":{"x":{"point":0.1 + 0.2,"ci95":[0.1, f64::NAN]}},"n":3});
    let same: serde_json::Value = serde_json::from_slice(&serde_json::to_vec(&a).unwrap()).unwrap();
    assert_eq!(gate_compare(&a, &same).unwrap()["equal"], true);
    let mut b = same.clone();
    b["m"]["x"]["point"] = json!(0.5);
    b["extra/key"] = json!(1);
    let r = gate_compare(&a, &b).unwrap();
    assert_eq!(r["equal"], false);
    assert_eq!(r["differing_pointers"], json!(["/extra~1key", "/m/x/point"]));
}

#[test]
fn hash_only_sink_matches_the_written_file() {
    let dir = tempfile::tempdir().unwrap();
    let rows = [json!({"a":1,"s":[0.1, 1e-300]}), json!({"b":"x"})];
    let mut f = PairSink::open(Some(dir.path()), "p.jsonl").unwrap();
    let mut h = PairSink::open(None, "p.jsonl").unwrap();
    for r in &rows {
        f.write(r).unwrap();
        h.write(r).unwrap();
    }
    let (nf, sf) = f.finish().unwrap();
    let (nh, sh) = h.finish().unwrap();
    assert_eq!((nf, &sf), (nh, &sh));
    assert_eq!(crate::common::hash_file(&dir.path().join("p.jsonl")).unwrap(), sf);
}

#[test]
fn strip_f_restores_the_v1_row_layouts() {
    let mut t12 = json!({"k":[1,3,5],"scores":{"all_text":{"A":[0.1],"F":[2.0]},"other":{"A":[0.2],"F":[1.0]}}});
    strip_f(&mut t12).unwrap();
    assert_eq!(t12, json!({"k":[1,3,5],"scores":{"all_text":{"A":[0.1]},"other":{"A":[0.2]}}}));
    let mut t3 = json!({"k":5,"scores":{"A":0.1,"D":1.0,"F":2.0}});
    strip_f(&mut t3).unwrap();
    assert_eq!(t3, json!({"k":5,"scores":{"A":0.1,"D":1.0}}));
    let mut missing = json!({"scores":{"all_text":{"A":[0.1]}}});
    assert!(strip_f(&mut missing).is_err());
}

#[test]
fn v2_decision_requires_a_lower_bound_above_zero() {
    let at = |lo: f64, hi: f64| {
        let c = json!({"primary/T1/pooled/k5/auc":{"F-D":{"point":(lo+hi)/2.0,"ci95":[lo, hi]}}});
        let d = decision_v2(&c);
        (d["outcome"].as_str().unwrap().to_string(), d["interval_position"].as_str().unwrap().to_string())
    };
    assert_eq!(at(0.001, 0.01), ("cfg_adds_resolved_value_beyond_d".into(), "above_zero".into()));
    assert_eq!(at(0.0, 0.01).1, "includes_zero");
    assert_eq!(at(-0.01, 0.01).0, "no_resolved_cfg_contribution_beyond_encoder_plus_stylometry");
    assert_eq!(at(-0.02, -0.001).1, "below_zero");
    let c = json!({"primary/T1/pooled/k5/auc":{"F-D":{"point":null,"ci95":[null, null]}}});
    assert_eq!(decision_v2(&c)["outcome"], "not_evaluable");
}
