//! `slop_ninja_a_augment`: augmented training copies and consistency probes
//! for bundle rows (`slop_ninja_a_bundle_row_v1`), human form-injection twins
//! (`probes --profile gen_form_inject_v1`, variants from the frozen S0-13
//! entry of `VALUES-R2.json`) and the per-class audit that computes those
//! variants from assembler Train splits (`inject-audit`). Offline; prints
//! counts and digests only.
//! Probe and copy outputs contain prose and belong in ignored storage.
use anyhow::{Context, Result, ensure};
use clap::{Parser, Subcommand};
use grammar_a_augment::form_inject;
use grammar_a_augment::{Audit, Profile, augment_row, class_words, probes_for_row, row_parts, selected};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
};

#[derive(Parser)]
#[command(name = "slop_ninja_a_augment")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Write one augmented copy for each row selected at --rate (every origin
    /// alike); originals are not repeated in the output.
    Copies {
        #[arg(long)]
        rows: PathBuf,
        /// Built-in profile name (train_v1, light_v1).
        #[arg(long, default_value = "train_v1")]
        profile: String,
        /// JSON profile file instead of a built-in profile.
        #[arg(long)]
        profile_file: Option<PathBuf>,
        #[arg(long)]
        rate: f64,
        #[arg(long)]
        seed: u64,
        /// Only rows of this split (default: all).
        #[arg(long)]
        split: Option<String>,
        #[arg(long)]
        out: PathBuf,
    },
    /// Write probes of one split: a text-free manifest for the evaluator and
    /// probe rows for inference. With a noise profile: consistency probes
    /// (exact repeats and light perturbations; needs --repeat-rate,
    /// --perturb-rate and --seed). With `--profile gen_form_inject_v1`: the
    /// two human injection twins (H-inj-C, H-inj-L) of every human-only row
    /// (needs --values); every row must be of --split.
    Probes {
        #[arg(long)]
        rows: PathBuf,
        #[arg(long)]
        split: String,
        #[arg(long, default_value = "light_v1")]
        profile: String,
        #[arg(long)]
        profile_file: Option<PathBuf>,
        #[arg(long)]
        repeat_rate: Option<f64>,
        #[arg(long)]
        perturb_rate: Option<f64>,
        #[arg(long)]
        seed: Option<u64>,
        /// gen_form_inject_v1: the round-2 value register (VALUES-R2.json);
        /// the variants are its frozen S0-13 entry's, and the run refuses a
        /// register without one.
        #[arg(long)]
        values: Option<PathBuf>,
        /// Directory for probes.jsonl, probe-rows.jsonl and summary.json.
        #[arg(long)]
        out: PathBuf,
    },
    /// Per-class audit for gen_form_inject_v1 (S0-13): for every injection
    /// class, the variant most over-represented in the positives (mixed and
    /// model-only rows with positive weight of the --positives Train splits)
    /// against the humans (human-only rows with positive weight of the
    /// --humans Train splits). Writes a text-free JSON record.
    InjectAudit {
        /// Assembler Train split directory (MANIFEST.json, rows.jsonl) whose
        /// human-only rows are humans (repeatable).
        #[arg(long, required = true)]
        humans: Vec<PathBuf>,
        /// Assembler Train split directory whose mixed and model-only rows
        /// are positives (repeatable).
        #[arg(long, required = true)]
        positives: Vec<PathBuf>,
        #[arg(long)]
        out: PathBuf,
    },
}

fn sha_file(p: &Path) -> Result<String> {
    Ok(hex::encode(Sha256::digest(fs::read(p).with_context(|| p.display().to_string())?)))
}

fn load_profile(name: &str, file: &Option<PathBuf>) -> Result<Profile> {
    let p = match file {
        Some(f) => serde_json::from_slice(&fs::read(f)?).context("profile file")?,
        None => Profile::builtin(name)?,
    };
    p.validate()?;
    Ok(p)
}

fn for_rows(path: &Path, mut f: impl FnMut(Value) -> Result<()>) -> Result<()> {
    let r = BufReader::new(fs::File::open(path).with_context(|| path.display().to_string())?);
    for (i, l) in r.lines().enumerate() {
        let l = l?;
        if l.trim().is_empty() {
            continue;
        }
        f(serde_json::from_str(&l).with_context(|| format!("{} line {}", path.display(), i + 1))?)?;
    }
    Ok(())
}

/// Atomic JSONL writer (tmp, fsync, rename) that hashes its bytes.
struct Out {
    tmp: PathBuf,
    path: PathBuf,
    f: std::io::BufWriter<fs::File>,
    h: Sha256,
    n: usize,
}

impl Out {
    fn create(path: &Path) -> Result<Out> {
        let tmp = path.with_extension("tmp");
        Ok(Out { f: std::io::BufWriter::new(fs::File::create(&tmp)?), tmp, path: path.into(), h: Sha256::new(), n: 0 })
    }
    fn push(&mut self, v: &Value) -> Result<()> {
        let mut b = serde_json::to_vec(v)?;
        b.push(b'\n');
        self.h.update(&b);
        self.f.write_all(&b)?;
        self.n += 1;
        Ok(())
    }
    fn finish(self) -> Result<Value> {
        let f = self.f.into_inner().map_err(|e| anyhow::anyhow!(e.to_string()))?;
        f.sync_all()?;
        fs::rename(&self.tmp, &self.path)?;
        Ok(json!({"path": self.path, "rows": self.n, "sha256": hex::encode(self.h.finalize())}))
    }
}

fn write_json(p: &Path, v: &Value) -> Result<()> {
    let tmp = p.with_extension("tmp");
    fs::write(&tmp, serde_json::to_vec_pretty(v)?)?;
    fs::rename(&tmp, p)?;
    Ok(())
}

fn main() -> Result<()> {
    run(Cli::parse())
}

fn run(cli: Cli) -> Result<()> {
    match cli.cmd {
        Cmd::Copies { rows, profile, profile_file, rate, seed, split, out } => {
            ensure!((0.0..=1.0).contains(&rate), "rate in [0, 1]");
            let prof = load_profile(&profile, &profile_file)?;
            let mut w = Out::create(&out)?;
            let mut audit = Audit::default();
            let mut ids = BTreeSet::new();
            for_rows(&rows, |r| {
                if split.as_ref().is_some_and(|s| r["split"] != s.as_str()) {
                    return Ok(());
                }
                let (id, origin, spans) = row_parts(&r)?;
                ensure!(ids.insert(id.clone()), "duplicate row id {id}");
                let words = class_words(r["text"].as_str().unwrap(), &spans)?;
                if selected("copy", seed, &id, rate) {
                    let a = augment_row(&r, &prof, seed)?;
                    audit.add(origin, words, Some(&a["augmentation"]));
                    w.push(&a)?;
                } else {
                    audit.add(origin, words, None);
                }
                Ok(())
            })?;
            let written = w.finish()?;
            let summary = json!({"schema": "slop_ninja_a_augment_copies_summary_v1", "rows": {"path": rows, "sha256": sha_file(&rows)?},
                "split": split, "profile": prof, "profile_sha256": prof.sha256(), "rate": rate, "seed": seed,
                "selection": "SHA-256 keyed draw of (seed, row id) below rate; labels are never read", "output": written, "audit": audit.report()});
            write_json(&out.with_extension("summary.json"), &summary)?;
            println!("{}", serde_json::to_string(&json!({"copies": written["rows"], "sha256": written["sha256"]}))?);
        }
        Cmd::Probes { rows, split, profile, profile_file, repeat_rate, perturb_rate, seed, values, out } if profile == form_inject::PROFILE => {
            ensure!(profile_file.is_none() && repeat_rate.is_none() && perturb_rate.is_none() && seed.is_none(),
                "{} takes no --profile-file, rates or seed: its draws are V21 plan keys", form_inject::PROFILE);
            let vpath = values.context("--profile gen_form_inject_v1 needs --values (VALUES-R2.json with the frozen S0-13 entry)")?;
            let vbytes = fs::read(&vpath).with_context(|| vpath.display().to_string())?;
            let vdoc: Value = serde_json::from_slice(&vbytes).context("value register")?;
            let (v, entry) = form_inject::variants_from_values(&vdoc)?;
            let vsha = hex::encode(Sha256::digest(&vbytes));
            fs::create_dir_all(&out)?;
            let mut man = Out::create(&out.join("probes.jsonl"))?;
            let mut pr = Out::create(&out.join("probe-rows.jsonl"))?;
            let rd = BufReader::new(fs::File::open(&rows).with_context(|| rows.display().to_string())?);
            let c = form_inject::twin_rows(rd, &split, &v, &vsha, |m, p| {
                man.push(m)?;
                pr.push(p)
            })?;
            let m = man.finish()?;
            let p = pr.finish()?;
            let summary = json!({"schema": "slop_ninja_a_form_inject_probes_summary_v1", "rows": {"path": rows, "sha256": sha_file(&rows)?, "rows": c.rows},
                "split": split, "profile": form_inject::PROFILE, "variants": v.to_json(),
                "values_file": {"path": vpath, "sha256": vsha, "key": form_inject::VALUES_KEY, "entry": entry},
                "twins_by_kind": c.twins_by_kind, "twins_with_unchanged_text": c.twins_with_unchanged_text,
                "edits_by_kind_and_class": c.edits_by_kind_and_class, "rows_without_twins_by_origin": c.rows_without_twins_by_origin,
                "manifest": m, "probe_rows": p,
                "use": "run A on probe-rows.jsonl; the evaluator pairs each twin with its base row (probe_of) by kind (h_inj_c, h_inj_l)"});
            write_json(&out.join("summary.json"), &summary)?;
            println!("{}", serde_json::to_string(&json!({"twins": m["rows"], "manifest_sha256": m["sha256"], "values_sha256": vsha}))?);
        }
        Cmd::Probes { rows, split, profile, profile_file, repeat_rate, perturb_rate, seed, values, out } => {
            ensure!(values.is_none(), "--values belongs to --profile {}", form_inject::PROFILE);
            let (Some(repeat_rate), Some(perturb_rate), Some(seed)) = (repeat_rate, perturb_rate, seed) else {
                anyhow::bail!("noise probes need --repeat-rate, --perturb-rate and --seed");
            };
            ensure!((0.0..=1.0).contains(&repeat_rate) && (0.0..=1.0).contains(&perturb_rate), "rates in [0, 1]");
            let prof = load_profile(&profile, &profile_file)?;
            fs::create_dir_all(&out)?;
            let mut man = Out::create(&out.join("probes.jsonl"))?;
            let mut pr = Out::create(&out.join("probe-rows.jsonl"))?;
            let mut counts: BTreeMap<String, BTreeMap<String, usize>> = BTreeMap::new();
            let mut unchanged = 0usize;
            let mut ids = BTreeSet::new();
            for_rows(&rows, |r| {
                if r["split"] != split.as_str() {
                    return Ok(());
                }
                let id = r["id"].as_str().context("id")?.to_owned();
                ensure!(ids.insert(id.clone()), "duplicate row id {id}");
                for (m, p) in probes_for_row(&r, &prof, seed, repeat_rate, perturb_rate)? {
                    *counts.entry(m["kind"].as_str().unwrap().into()).or_default().entry(m["origin"].as_str().unwrap().into()).or_default() += 1;
                    unchanged += usize::from(m["kind"] == "perturbed" && m["text_changed"] == false);
                    man.push(&m)?;
                    pr.push(&p)?;
                }
                Ok(())
            })?;
            let m = man.finish()?;
            let p = pr.finish()?;
            let summary = json!({"schema": "slop_ninja_a_consistency_probes_summary_v1", "rows": {"path": rows, "sha256": sha_file(&rows)?},
                "split": split, "profile": prof, "profile_sha256": prof.sha256(), "repeat_rate": repeat_rate, "perturb_rate": perturb_rate,
                "seed": seed, "probes_by_kind_and_origin": counts, "perturbed_probes_with_unchanged_text": unchanged,
                "manifest": m, "probe_rows": p,
                "use": "run A on probe-rows.jsonl; pass probes.jsonl and the probe predictions to a_comp_evaluate --probes/--probe-pred"});
            write_json(&out.join("summary.json"), &summary)?;
            println!("{}", serde_json::to_string(&json!({"probes": m["rows"], "manifest_sha256": m["sha256"]}))?);
        }
        Cmd::InjectAudit { humans, positives, out } => {
            let rep = form_inject::audit_train_splits(&humans, &positives)?;
            write_json(&out, &rep)?;
            println!("{}", serde_json::to_string(&json!({"positives": rep["positives"], "humans": rep["humans"], "audit_sha256": sha_file(&out)?, "variants": rep["variants"]}))?);
        }
    }
    Ok(())
}

#[cfg(test)]
mod cli_tests {
    //! The gen_form_inject_v1 command paths, end to end on synthetic rows.
    use super::{Cli, run};
    use clap::Parser;
    use serde_json::{Value, json};
    use sha2::{Digest, Sha256};
    use std::fs;
    use std::path::{Path, PathBuf};

    struct TmpDir(PathBuf);

    impl TmpDir {
        fn new(name: &str) -> TmpDir {
            let p = std::env::temp_dir().join(format!("grammar-a-augment-cli-{}-{name}", std::process::id()));
            let _ = fs::remove_dir_all(&p);
            fs::create_dir_all(&p).unwrap();
            TmpDir(p)
        }
    }

    impl Drop for TmpDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn row(id: &str, split: &str, text: &str, origin: &str, w: f64) -> Value {
        let spans: Vec<[usize; 2]> = if origin == "human_only" { vec![] } else { vec![[0, text.len()]] };
        json!({"id": id, "split": split, "origin": origin, "share": if origin == "human_only" { 0.0 } else { 1.0 }, "text": text,
            "text_sha256": hex::encode(Sha256::digest(text.as_bytes())), "ai_byte_spans": spans, "weight": w, "source_group": format!("g-{id}")})
    }

    fn jsonl(rows: &[Value]) -> String {
        rows.iter().map(|r| serde_json::to_string(r).unwrap() + "\n").collect()
    }

    fn train_dir(root: &Path, name: &str, rows: &[Value]) -> PathBuf {
        let d = root.join(name);
        fs::create_dir_all(&d).unwrap();
        let body = jsonl(rows);
        fs::write(d.join("rows.jsonl"), &body).unwrap();
        let m = json!({"schema": "slop_ninja_a_bundle_manifest_v1", "build_id": "b-cli", "split": "train", "sealed": false,
            "rows": rows.len(), "files": {"rows.jsonl": hex::encode(Sha256::digest(body.as_bytes()))}});
        fs::write(d.join("MANIFEST.json"), serde_json::to_vec(&m).unwrap()).unwrap();
        d
    }

    fn cli(args: &[&str]) -> anyhow::Result<()> {
        run(Cli::try_parse_from(std::iter::once("slop_ninja_a_augment").chain(args.iter().copied()))?)
    }

    fn read(p: &Path) -> Value {
        serde_json::from_slice(&fs::read(p).unwrap()).unwrap()
    }

    #[test]
    fn inject_audit_takes_humans_and_positives_by_role() {
        let tmp = TmpDir::new("audit");
        let h = train_dir(&tmp.0, "h", &[
            row("h1", "train", "\"Plain,\" he said to us.", "human_only", 1.0),
            row("h2", "train", "Nothing at all here today.", "human_only", 1.0),
            row("z1", "train", "\u{201C}Zero weight,\u{201D} it said.", "human_only", 0.0),
            row("m1", "train", "\u{201C}A model row,\u{201D} it said.", "model_only", 1.0),
        ]);
        let p = train_dir(&tmp.0, "p", &[
            row("p1", "train", "\u{201C}Curly model text,\u{201D} it said.", "model_only", 1.0),
            row("p2", "train", "\"A human row in the positives.\"", "human_only", 1.0),
        ]);
        let out = tmp.0.join("audit.json");
        cli(&["inject-audit", "--humans", h.to_str().unwrap(), "--positives", p.to_str().unwrap(), "--out", out.to_str().unwrap()]).unwrap();
        let a = read(&out);
        assert_eq!((a["humans"].clone(), a["positives"].clone()), (json!(2), json!(1)));
        assert_eq!(a["variants"]["quote"], "curly");
        assert_eq!(a["classes"]["quote"]["variants"][0]["score_numerator"], "2");
        assert_eq!(a["inputs"][0]["role"], "humans");
        assert_eq!(a["inputs"][0]["input"]["train_dir"], json!(h));
        assert_eq!(a["inputs"][1]["input"]["train_dir"], json!(p));
        // The audit has no all-splits mode: a rows file is not a Train split directory.
        assert!(cli(&["inject-audit", "--humans", h.join("rows.jsonl").to_str().unwrap(), "--positives", p.to_str().unwrap(), "--out", out.to_str().unwrap()]).is_err());
        assert!(cli(&["inject-audit", "--humans", h.to_str().unwrap(), "--positives", p.to_str().unwrap(), "--split", "train", "--out", out.to_str().unwrap()]).is_err());
    }

    #[test]
    fn inject_probes_read_the_frozen_variants_and_one_split() {
        let tmp = TmpDir::new("probes");
        let text = "\"Go now,\" she said - and left... It was late.\nThe next line follows here.";
        let rows = tmp.0.join("rows.jsonl");
        fs::write(&rows, jsonl(&[row("h1", "dev-iter-2", text, "human_only", 1.0), row("o1", "dev-iter-2", text, "model_only", 1.0)])).unwrap();
        let variants = json!({"quote": "curly", "dash": "em_closed", "ellipsis": "char", "emphasis": null, "trailing_whitespace": "\\s", "separator": "\\n\\n"});
        let values = tmp.0.join("VALUES-R2.json");
        fs::write(&values, serde_json::to_vec(&json!({"schema": "slop_ninja_a_round2_values_v1", "values": {"S0-13": {"variants": variants, "audit": {"sha256": "ab"}}}})).unwrap()).unwrap();
        let out = tmp.0.join("out");
        let args = |v: &Path, r: &Path, split: &str| -> anyhow::Result<()> {
            cli(&["probes", "--rows", r.to_str().unwrap(), "--split", split, "--profile", "gen_form_inject_v1", "--values", v.to_str().unwrap(), "--out", out.to_str().unwrap()])
        };
        args(&values, &rows, "dev-iter-2").unwrap();
        let s = read(&out.join("summary.json"));
        assert_eq!(s["twins_by_kind"], json!({"h_inj_c": 1, "h_inj_l": 1}));
        assert_eq!(s["rows_without_twins_by_origin"], json!({"model_only": 1}));
        assert_eq!(s["variants"], variants);
        let vsha = hex::encode(Sha256::digest(fs::read(&values).unwrap()));
        assert_eq!(s["values_file"]["sha256"], vsha);
        assert_eq!(s["values_file"]["entry"]["audit"]["sha256"], "ab");
        let man: Vec<Value> = fs::read_to_string(out.join("probes.jsonl")).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        assert_eq!(man.len(), 2);
        assert!(man.iter().all(|m| m["values_sha256"] == vsha.as_str() && m["split"] == "dev-iter-2" && m.get("text").is_none()));
        // Rows of another split stop the run; so does a register without S0-13.
        assert!(args(&values, &rows, "test-2").is_err());
        let empty = tmp.0.join("empty.json");
        fs::write(&empty, serde_json::to_vec(&json!({"schema": "slop_ninja_a_round2_values_v1", "values": {"S0-7": {}}})).unwrap()).unwrap();
        assert!(args(&empty, &rows, "dev-iter-2").is_err());
        // The audit output is not the frozen record.
        let audit = tmp.0.join("audit.json");
        fs::write(&audit, serde_json::to_vec(&json!({"schema": "slop_ninja_a_form_inject_audit_v1", "variants": variants})).unwrap()).unwrap();
        assert!(args(&audit, &rows, "dev-iter-2").is_err());
        assert!(cli(&["probes", "--rows", rows.to_str().unwrap(), "--split", "dev-iter-2", "--profile", "gen_form_inject_v1", "--out", out.to_str().unwrap()]).is_err(), "--values is required");
    }
}
