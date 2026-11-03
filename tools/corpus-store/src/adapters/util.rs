//! Field helpers shared by adapters.

use serde_json::Value;

/// Field by path: `a.b.c` (object keys; numeric components index arrays).
pub fn get<'a>(v: &'a Value, path: &str) -> Option<&'a Value> {
    let mut cur = v;
    for k in path.split('.') {
        cur = match cur {
            Value::Object(m) => m.get(k)?,
            Value::Array(a) => a.get(k.parse::<usize>().ok()?)?,
            _ => return None,
        };
    }
    if cur.is_null() { None } else { Some(cur) }
}

/// First present path, as a string (numbers and booleans are rendered).
pub fn s(v: &Value, paths: &[&str]) -> Option<String> {
    for p in paths {
        if let Some(x) = get(v, p) {
            match x {
                Value::String(t) => return Some(t.clone()),
                Value::Number(n) => return Some(n.to_string()),
                Value::Bool(b) => return Some(b.to_string()),
                _ => return Some(x.to_string()),
            }
        }
    }
    None
}

pub fn f(v: &Value, paths: &[&str]) -> Option<f64> {
    for p in paths {
        if let Some(x) = get(v, p) {
            if let Some(n) = x.as_f64() {
                return Some(n);
            }
            if let Some(t) = x.as_str() {
                if let Ok(n) = t.parse() {
                    return Some(n);
                }
            }
        }
    }
    None
}

pub fn i(v: &Value, paths: &[&str]) -> Option<i64> {
    for p in paths {
        if let Some(x) = get(v, p).and_then(|x| x.as_i64()) {
            return Some(x);
        }
    }
    None
}

/// A copy of `v` without the listed top-level keys (for `extra_json`).
pub fn without(v: &Value, keys: &[&str]) -> Value {
    match v {
        Value::Object(m) => Value::Object(m.iter().filter(|(k, _)| !keys.contains(&k.as_str())).map(|(k, x)| (k.clone(), x.clone())).collect()),
        x => x.clone(),
    }
}

/// Replace every string longer than `max` bytes (anywhere in `v`) by
/// `{"$blob": sha256}` so metadata copies never duplicate text.
pub fn strip_long(v: &Value, max: usize) -> Value {
    match v {
        Value::String(t) if t.len() > max => serde_json::json!({"$sha256": hex::encode(crate::skeleton::sha256(t.as_bytes())), "$len": t.len()}),
        Value::Array(a) => Value::Array(a.iter().map(|x| strip_long(x, max)).collect()),
        Value::Object(m) => Value::Object(m.iter().map(|(k, x)| (k.clone(), strip_long(x, max))).collect()),
        x => x.clone(),
    }
}

/// Canonical split role names.
pub fn split_role(s: &str) -> String {
    let t = s.replace('-', "_").to_lowercase();
    match t.as_str() {
        "dev" | "development" => "dev".into(),
        "calibration" => "cal".into(),
        "mining" | "train_mining" => "train_mining".into(),
        _ => t,
    }
}
