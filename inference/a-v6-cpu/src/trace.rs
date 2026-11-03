//! Read-only synthetic tensor instrumentation; disabled in ordinary probes.
use candle::Tensor;
use serde_json::{Value, json};
use std::sync::{Mutex, OnceLock};

static TRACE: OnceLock<Mutex<(usize, Vec<Value>)>> = OnceLock::new();

pub fn begin() {
    TRACE
        .set(Mutex::new((0, Vec::new())))
        .expect("one trace per process");
}
pub fn layer(index: usize) {
    if let Some(t) = TRACE.get() {
        t.lock().unwrap().0 = index;
    }
}
pub fn record(name: &str, value: &Tensor) -> candle::Result<()> {
    if let Some(t) = TRACE.get() {
        t.lock()
            .unwrap()
            .1
            .push(json!({"name":name, "shape":value.dims(),
            "values":value.flatten_all()?.to_dtype(candle::DType::F64)?.to_vec1::<f64>()?}));
    }
    Ok(())
}
pub fn local(name: &str, value: &Tensor) -> candle::Result<()> {
    if let Some(t) = TRACE.get() {
        let index = t.lock().unwrap().0;
        record(&format!("model.layers.{index}.{name}"), value)?;
    }
    Ok(())
}
pub fn finish() -> Vec<Value> {
    std::mem::take(&mut TRACE.get().unwrap().lock().unwrap().1)
}

pub fn compare(reference: &Value, actual: &Value) -> anyhow::Result<Value> {
    use anyhow::{Context, ensure};
    use std::collections::BTreeMap;
    let refs = reference["tensors"]
        .as_array()
        .context("reference tensors")?;
    let actual = actual["tensors"]
        .as_array()
        .context("actual tensors")?
        .iter()
        .map(|v| Ok((v["name"].as_str().context("tensor name")?, v)))
        .collect::<anyhow::Result<BTreeMap<_, _>>>()?;
    ensure!(refs.len() == actual.len(), "trace count differs");
    let mut rows = Vec::new();
    let mut first = Value::Null;
    for r in refs {
        let name = r["name"].as_str().context("name")?;
        let a = actual
            .get(name)
            .with_context(|| format!("missing {name}"))?;
        ensure!(r["shape"] == a["shape"], "shape differs for {name}");
        let rv = r["values"].as_array().context("reference values")?;
        let av = a["values"].as_array().context("actual values")?;
        ensure!(rv.len() == av.len(), "value count differs");
        let mut max = 0.0f64;
        let mut squared = 0.;
        let mut differing = 0;
        let mut first_value = Value::Null;
        for (i, (r, a)) in rv.iter().zip(av).enumerate() {
            let (r, a) = (
                r.as_f64().context("reference number")?,
                a.as_f64().context("actual number")?,
            );
            let d = (r - a).abs();
            max = max.max(d);
            squared += d * d;
            if d != 0. {
                differing += 1;
                if first_value.is_null() {
                    first_value = json!({"flat_index":i,"reference":r,"rust":a});
                }
            }
        }
        let row = json!({"name":name,"shape":r["shape"],"values":rv.len(),"differing_values":differing,
            "max_absolute_error":max,"rms_error":(squared / rv.len() as f64).sqrt(),"first_difference":first_value});
        if first.is_null() && differing > 0 {
            first = row.clone();
        }
        rows.push(row);
    }
    Ok(
        json!({"first_divergent_operation":first,"operations":rows,"synthetic_only":true,
            "operator_equivalence_not_inferred":true}),
    )
}
