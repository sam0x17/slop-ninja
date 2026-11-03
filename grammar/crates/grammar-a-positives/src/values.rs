//! Measurement-fixed values read from `VALUES-R2.json` (protocol A2 §2.8 and
//! §12.6; B5b review finding 5).
//!
//! A rule input fixed by measurement is read from the frozen register, never
//! from the command line: `mirror-plan` reads r05 (S0-6) and `stratify-train
//! --target` reads v_fam (S0-7). Each refuses when its value is absent, and
//! each record names the register's path and SHA-256 and the value as read.
//!
//! **The entries.** `values.<id>` holds `id` (`S0-6`, `S0-7`), `name`
//! (`r05`, `v_fam`) and `value`:
//! * S0-7's `value` is a non-negative integer (the median accepted local
//!   Train rows per instruct family; written at A9).
//! * S0-6's `value` is r05 as an exact decimal or ratio ([`crate::plan::R05`]):
//!   a JSON string (`"0.8125"`, `"13/16"`), or a JSON number, read through
//!   its shortest round-trip decimal text. S0-6 is not written yet: its
//!   estimator and word unit wait for a forward amendment (the §4.3
//!   re-verdict's stop record), which also fixes this entry's form.
//! * S0-8 (`id` `S0-8`, `name` `supervision_targets`; amendment 27):
//!   its `value` is the output of `slop_ninja_a_assemble supervision`
//!   (schema [`SUPERVISION_SCHEMA`], rule [`SUPERVISION_RULE`]), whole and
//!   unchanged: each covered domain's targets under
//!   `r2_domain_supervision_v1` (B5b part 2's top-up plan and D5's step read
//!   it; BUILD-PLAN 11.4: "the plan reads S0-8 from the register by its
//!   schema string"). Not written yet.
//! * S0-10 (`id` `S0-10`, `name` `base_cohort_stratification`; amendment
//!   27): its `value` is the base-cohort stratification
//!   ([`crate::stratify::S0_10_SCHEMA`]): per Train base family, the
//!   per-domain targets of accepted rows at v_fam, written by
//!   `stratify-train --write-stratification` and read by D7's step
//!   (`stratify-train --stratification`). Writing it into the register (the
//!   entry file's `id`, `name` and `value`, unchanged) is a later A9 step,
//!   not part of the build. Not written yet.
use crate::plan::R05;
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

/// The register's schema.
pub const VALUES_SCHEMA: &str = "slop_ninja_a_round2_values_v1";
/// r05's register id and name.
pub const R05_ID: (&str, &str) = ("S0-6", "r05");
/// v_fam's register id and name.
pub const V_FAM_ID: (&str, &str) = ("S0-7", "v_fam");
/// S0-8's register id and name (protocol A2 §12.6: "Supervision targets";
/// fixed by amendment 27).
pub const S0_8_ID: (&str, &str) = ("S0-8", "supervision_targets");
/// S0-10's register id and name (protocol A2 §12.6: "Base-cohort
/// stratification"; fixed by amendment 27).
pub const S0_10_ID: (&str, &str) = ("S0-10", "base_cohort_stratification");
/// The schema of S0-8's value: `slop_ninja_a_assemble supervision`'s output
/// (`grammar_a_assemble::supervision::OUTPUT_SCHEMA`; read by its string, so
/// this crate does not depend on the assembler).
pub const SUPERVISION_SCHEMA: &str = "slop_ninja_a_r2_domain_supervision_v1";
/// The rule S0-8's value names (`grammar_a_assemble::supervision::SUPERVISION_RULE`).
pub const SUPERVISION_RULE: &str = "r2_domain_supervision_v1";

/// `VALUES-R2.json`, read once and hashed.
pub struct Values {
    pub path: PathBuf,
    pub sha256: String,
    v: Value,
}

impl Values {
    pub fn open(path: &Path) -> Result<Self> {
        let b = std::fs::read(path).with_context(|| path.display().to_string())?;
        let v: Value = serde_json::from_slice(&b).with_context(|| path.display().to_string())?;
        ensure!(v["schema"] == VALUES_SCHEMA, "{} is not VALUES-R2.json (schema {VALUES_SCHEMA})", path.display());
        Ok(Values { path: path.to_owned(), sha256: crate::text::sha_hex(&b), v })
    }
    /// The entry `values.<id>`, whose `id` and `name` must be the given ones;
    /// an absent entry is refused (the step it precedes cannot run yet).
    pub fn entry(&self, (id, name): (&str, &str)) -> Result<&Value> {
        let e = &self.v["values"][id];
        ensure!(!e.is_null(), "{} holds no {id} ({name}): the value is fixed by measurement and frozen before the step that uses it (protocol A2 sections 2.8 and 12.6); this step cannot run before it", self.path.display());
        ensure!(e["id"] == id && e["name"] == name, "{}: values.{id} is not the {id} {name} entry", self.path.display());
        Ok(e)
    }
    /// The record a tool's output carries: the register's path and digest,
    /// the entry's id and name, and its value as read.
    pub fn record(&self, (id, name): (&str, &str), value: Value) -> Value {
        json!({"path": self.path, "sha256": self.sha256, "id": id, "name": name, "value": value})
    }
}

/// r05 (S0-6) from the register, exact.
pub fn r05(v: &Values) -> Result<(R05, Value)> {
    let e = v.entry(R05_ID)?;
    let text = match &e["value"] {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        x => anyhow::bail!("{}: S0-6's value {x} is neither an exact decimal string nor a number", v.path.display()),
    };
    let r = R05::parse(&text).with_context(|| format!("{}: S0-6's value", v.path.display()))?;
    Ok((r, v.record(R05_ID, json!(text))))
}

/// v_fam (S0-7) from the register.
pub fn v_fam(v: &Values) -> Result<(usize, Value)> {
    let e = v.entry(V_FAM_ID)?;
    let n = e["value"].as_u64().with_context(|| format!("{}: S0-7's value is not a non-negative integer", v.path.display()))?;
    Ok((n as usize, v.record(V_FAM_ID, json!(n))))
}

/// S0-8 (the supervision targets) from the register: its value, which must
/// be the supervision tool's output by schema and rule, and the record (the
/// register's path and digest, and the value's digest; the value itself is
/// counts and shares, never text).
pub fn supervision(v: &Values) -> Result<(Value, Value)> {
    let e = v.entry(S0_8_ID)?;
    let val = &e["value"];
    ensure!(val["schema"] == SUPERVISION_SCHEMA && val["rule"] == SUPERVISION_RULE,
        "{}: S0-8's value is not the {SUPERVISION_SCHEMA} output of rule {SUPERVISION_RULE}", v.path.display());
    ensure!(val["domains"].as_object().is_some_and(|d| !d.is_empty()), "{}: S0-8's value covers no domain", v.path.display());
    let digest = crate::text::sha_hex(&serde_json::to_vec(val)?);
    Ok((val.clone(), v.record(S0_8_ID, json!({"schema": SUPERVISION_SCHEMA, "value_json_sha256": digest}))))
}

/// S0-10 (the base-cohort stratification) from the register: its value,
/// which must be [`crate::stratify::S0_10_SCHEMA`]'s, and the record.
pub fn base_stratification(v: &Values) -> Result<(Value, Value)> {
    let e = v.entry(S0_10_ID)?;
    let val = &e["value"];
    ensure!(val["schema"] == crate::stratify::S0_10_SCHEMA, "{}: S0-10's value is not a {} record", v.path.display(), crate::stratify::S0_10_SCHEMA);
    let digest = crate::text::sha_hex(&serde_json::to_vec(val)?);
    Ok((val.clone(), v.record(S0_10_ID, json!({"schema": crate::stratify::S0_10_SCHEMA, "value_json_sha256": digest}))))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(d: &Path, v: Value) -> PathBuf {
        let p = d.join(format!("v{}.json", crate::text::sha_hex(v.to_string().as_bytes())));
        std::fs::write(&p, serde_json::to_vec(&v).unwrap()).unwrap();
        p
    }

    #[test]
    fn values_are_read_from_the_register_and_refused_when_absent() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        let s07 = json!({"id": "S0-7", "name": "v_fam", "value": 2235});
        let p = file(d, json!({"schema": VALUES_SCHEMA, "values": {"S0-7": s07}}));
        let v = Values::open(&p).unwrap();
        let (n, rec) = v_fam(&v).unwrap();
        assert_eq!(n, 2235);
        assert_eq!(rec["sha256"], crate::text::sha_hex(&std::fs::read(&p).unwrap()));
        // S0-6 absent: refused
        let e = r05(&v).err().unwrap().to_string();
        assert!(e.contains("holds no S0-6"), "{e}");
        // a string and a number, both exact; a float in exponent form or an out-of-form value refused
        for (val, floor) in [(json!("0.8"), 188), (json!("13/16"), 185), (json!(0.8125), 185), (json!(1), 150)] {
            let p = file(d, json!({"schema": VALUES_SCHEMA, "values": {"S0-6": {"id": "S0-6", "name": "r05", "value": val}}}));
            let (r, rec) = r05(&Values::open(&p).unwrap()).unwrap();
            assert_eq!(r.floor_words(), floor, "{val}");
            assert_eq!(rec["id"], "S0-6");
        }
        for bad in [json!("1e-1"), json!(true), json!(null), json!(0)] {
            let p = file(d, json!({"schema": VALUES_SCHEMA, "values": {"S0-6": {"id": "S0-6", "name": "r05", "value": bad}}}));
            assert!(r05(&Values::open(&p).unwrap()).is_err(), "{bad}");
        }
        // another entry's id or name, another schema
        let p = file(d, json!({"schema": VALUES_SCHEMA, "values": {"S0-7": {"id": "S0-7", "name": "r05", "value": 3}}}));
        assert!(v_fam(&Values::open(&p).unwrap()).err().unwrap().to_string().contains("not the S0-7 v_fam entry"));
        let p = file(d, json!({"schema": "other", "values": {}}));
        assert!(Values::open(&p).is_err());
        let p = file(d, json!({"schema": VALUES_SCHEMA, "values": {"S0-7": {"id": "S0-7", "name": "v_fam", "value": -1}}}));
        assert!(v_fam(&Values::open(&p).unwrap()).is_err());
    }

    #[test]
    fn s0_8_and_s0_10_are_read_by_schema_and_refused_when_absent() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        let p = file(d, json!({"schema": VALUES_SCHEMA, "values": {"S0-7": {"id": "S0-7", "name": "v_fam", "value": 5}}}));
        let v = Values::open(&p).unwrap();
        assert!(supervision(&v).err().unwrap().to_string().contains("holds no S0-8"));
        assert!(base_stratification(&v).err().unwrap().to_string().contains("holds no S0-10"));
        let sup = json!({"schema": SUPERVISION_SCHEMA, "rule": SUPERVISION_RULE, "domains": {"legal": {}}});
        let p = file(d, json!({"schema": VALUES_SCHEMA, "values": {"S0-8": {"id": "S0-8", "name": "supervision_targets", "value": sup}}}));
        let (val, rec) = supervision(&Values::open(&p).unwrap()).unwrap();
        assert_eq!(val["domains"], json!({"legal": {}}));
        assert_eq!((rec["id"].as_str(), rec["sha256"].as_str()), (Some("S0-8"), Some(crate::text::sha_hex(&std::fs::read(&p).unwrap()).as_str())));
        // another schema, another rule, no domain, another name
        for bad in [json!({"schema": "x", "rule": SUPERVISION_RULE, "domains": {"a": {}}}), json!({"schema": SUPERVISION_SCHEMA, "rule": "x", "domains": {"a": {}}}),
            json!({"schema": SUPERVISION_SCHEMA, "rule": SUPERVISION_RULE, "domains": {}})] {
            let p = file(d, json!({"schema": VALUES_SCHEMA, "values": {"S0-8": {"id": "S0-8", "name": "supervision_targets", "value": bad}}}));
            assert!(supervision(&Values::open(&p).unwrap()).is_err());
        }
        let p = file(d, json!({"schema": VALUES_SCHEMA, "values": {"S0-8": {"id": "S0-8", "name": "targets", "value": sup}}}));
        assert!(supervision(&Values::open(&p).unwrap()).err().unwrap().to_string().contains("not the S0-8 supervision_targets entry"));
        let st = json!({"schema": crate::stratify::S0_10_SCHEMA, "targets": {}});
        let p = file(d, json!({"schema": VALUES_SCHEMA, "values": {"S0-10": {"id": "S0-10", "name": "base_cohort_stratification", "value": st}}}));
        assert_eq!(base_stratification(&Values::open(&p).unwrap()).unwrap().0, st);
        let p = file(d, json!({"schema": VALUES_SCHEMA, "values": {"S0-10": {"id": "S0-10", "name": "base_cohort_stratification", "value": {"schema": "x"}}}}));
        assert!(base_stratification(&Values::open(&p).unwrap()).is_err());
    }
}
