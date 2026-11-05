//! The state round-trips through every encoding it crosses: the state calls'
//! JSON bodies, `JSON.parse`'s object as `serde_wasm_bindgen` reads it, and
//! storage's JS `Map` form.
//!
//! `serde_wasm_bindgen` runs only inside a JS host, so [`Js`] mirrors the
//! values it reads and the rules of its `Deserializer` (0.6): a JSON object
//! key is a string, a number is a double read as an integer only when it is
//! a safe integer, and an integer type refuses a string. The first test
//! checks the mirror against the live failure of 2026-11-04.

use std::collections::BTreeMap;

use serde::de::{self, DeserializeSeed, MapAccess, SeqAccess, Unexpected, Visitor};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{Begun, Durable, Save, parse_body};
use crate::alerts::PublisherObs;

type Error = de::value::Error;

/// A JS value as `serde_wasm_bindgen` sees it.
#[derive(Clone, Debug)]
enum Js {
    Null,
    Bool(bool),
    Number(f64),
    Str(String),
    Array(Vec<Js>),
    /// A plain object: string keys, in insertion order.
    Object(Vec<(String, Js)>),
    /// A JS `Map`, which keeps its keys' types.
    Map(Vec<(Js, Js)>),
}

impl Js {
    /// `JSON.parse` of the JSON text of `v`: every number a double.
    fn parse(v: &Value) -> Self {
        match v {
            Value::Null => Js::Null,
            Value::Bool(b) => Js::Bool(*b),
            Value::Number(n) => Js::Number(n.as_f64().expect("finite")),
            Value::String(s) => Js::Str(s.clone()),
            Value::Array(a) => Js::Array(a.iter().map(Js::parse).collect()),
            Value::Object(o) => {
                Js::Object(o.iter().map(|(k, v)| (k.clone(), Js::parse(v))).collect())
            }
        }
    }

    fn unexpected(&self) -> Unexpected<'_> {
        match self {
            Js::Null => Unexpected::Unit,
            Js::Bool(b) => Unexpected::Bool(*b),
            Js::Number(n) => Unexpected::Float(*n),
            Js::Str(s) => Unexpected::Str(s),
            Js::Array(_) => Unexpected::Seq,
            Js::Object(_) | Js::Map(_) => Unexpected::Map,
        }
    }

    fn safe_integer(&self) -> Option<f64> {
        const MAX_SAFE: f64 = 9_007_199_254_740_991.0;
        match self {
            Js::Number(n) if n.fract() == 0.0 && n.abs() <= MAX_SAFE => Some(*n),
            _ => None,
        }
    }

    /// The object at `path` (field names) replaced by a JS `Map` whose keys
    /// are numbers: how storage holds a Rust map keyed by `u64`.
    fn with_number_keyed_map(mut self, path: &[&str]) -> Self {
        let mut at = &mut self;
        for field in path {
            let Js::Object(fields) = at else {
                panic!("no object at {field}")
            };
            at = &mut fields
                .iter_mut()
                .find(|(k, _)| k == field)
                .expect("field")
                .1;
        }
        let Js::Object(entries) = at.clone() else {
            panic!("not an object")
        };
        *at = Js::Map(
            entries
                .into_iter()
                .map(|(k, v)| (Js::Number(k.parse::<u64>().expect("numeric key") as f64), v))
                .collect(),
        );
        self
    }
}

macro_rules! unsigned {
    ($($name:ident => $visit:ident),*) => {$(
        fn $name<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
            match self.safe_integer() {
                Some(n) if n >= 0.0 => visitor.$visit(n as u64),
                _ => Err(de::Error::invalid_type(self.unexpected(), &visitor)),
            }
        }
    )*};
}

macro_rules! signed {
    ($($name:ident => $visit:ident),*) => {$(
        fn $name<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
            match self.safe_integer() {
                Some(n) => visitor.$visit(n as i64),
                None => Err(de::Error::invalid_type(self.unexpected(), &visitor)),
            }
        }
    )*};
}

impl<'de> de::Deserializer<'de> for Js {
    type Error = Error;

    fn deserialize_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        match self {
            Js::Null => visitor.visit_unit(),
            Js::Bool(b) => visitor.visit_bool(b),
            Js::Number(n) => match Js::Number(n).safe_integer() {
                Some(i) => visitor.visit_i64(i as i64),
                None => visitor.visit_f64(n),
            },
            Js::Str(s) => visitor.visit_string(s),
            Js::Array(a) => visitor.visit_seq(Seq(a.into_iter())),
            Js::Object(o) => visitor.visit_map(Entries::new(
                o.into_iter().map(|(k, v)| (Js::Str(k), v)).collect(),
            )),
            Js::Map(m) => visitor.visit_map(Entries::new(m)),
        }
    }

    unsigned!(deserialize_u8 => visit_u64, deserialize_u16 => visit_u64, deserialize_u32 => visit_u64, deserialize_u64 => visit_u64);
    signed!(deserialize_i8 => visit_i64, deserialize_i16 => visit_i64, deserialize_i32 => visit_i64, deserialize_i64 => visit_i64);

    fn deserialize_option<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        match self {
            Js::Null => visitor.visit_none(),
            v => visitor.visit_some(v),
        }
    }

    fn deserialize_string<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        match self {
            Js::Str(s) => visitor.visit_string(s),
            v => Err(de::Error::invalid_type(v.unexpected(), &visitor)),
        }
    }

    fn deserialize_str<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        self.deserialize_string(visitor)
    }

    fn deserialize_identifier<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        self.deserialize_string(visitor)
    }

    fn deserialize_bool<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        match self {
            Js::Bool(b) => visitor.visit_bool(b),
            v => Err(de::Error::invalid_type(v.unexpected(), &visitor)),
        }
    }

    fn deserialize_seq<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        match self {
            Js::Array(a) => visitor.visit_seq(Seq(a.into_iter())),
            v => Err(de::Error::invalid_type(v.unexpected(), &visitor)),
        }
    }

    fn deserialize_tuple<V: Visitor<'de>>(
        self,
        _len: usize,
        visitor: V,
    ) -> Result<V::Value, Error> {
        self.deserialize_seq(visitor)
    }

    /// An iterable of pairs (a `Map`) or a plain object's entries.
    fn deserialize_map<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        match self {
            v @ (Js::Object(_) | Js::Map(_)) => v.deserialize_any(visitor),
            v => Err(de::Error::invalid_type(v.unexpected(), &visitor)),
        }
    }

    /// A plain object read field by field.
    fn deserialize_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        _fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Error> {
        match self {
            v @ Js::Object(_) => v.deserialize_any(visitor),
            v => Err(de::Error::invalid_type(v.unexpected(), &visitor)),
        }
    }

    fn deserialize_newtype_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        visitor: V,
    ) -> Result<V::Value, Error> {
        visitor.visit_newtype_struct(self)
    }

    fn deserialize_ignored_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        visitor.visit_unit()
    }

    serde::forward_to_deserialize_any! {
        u128 i128 f32 f64 char bytes byte_buf unit unit_struct tuple_struct enum
    }
}

struct Seq(std::vec::IntoIter<Js>);

impl<'de> SeqAccess<'de> for Seq {
    type Error = Error;
    fn next_element_seed<T: DeserializeSeed<'de>>(
        &mut self,
        seed: T,
    ) -> Result<Option<T::Value>, Error> {
        self.0.next().map(|v| seed.deserialize(v)).transpose()
    }
}

struct Entries {
    rest: std::vec::IntoIter<(Js, Js)>,
    value: Option<Js>,
}

impl Entries {
    fn new(entries: Vec<(Js, Js)>) -> Self {
        Self {
            rest: entries.into_iter(),
            value: None,
        }
    }
}

impl<'de> MapAccess<'de> for Entries {
    type Error = Error;
    fn next_key_seed<K: DeserializeSeed<'de>>(
        &mut self,
        seed: K,
    ) -> Result<Option<K::Value>, Error> {
        let Some((k, v)) = self.rest.next() else {
            return Ok(None);
        };
        self.value = Some(v);
        seed.deserialize(k).map(Some)
    }
    fn next_value_seed<T: DeserializeSeed<'de>>(&mut self, seed: T) -> Result<T::Value, Error> {
        seed.deserialize(self.value.take().expect("value after key"))
    }
}

/// What `JSON.parse` of `t`'s `serde_json` text gives, read back by
/// `serde_wasm_bindgen`.
fn through_json_parse<T: Serialize + for<'d> Deserialize<'d>>(t: &T) -> Result<T, Error> {
    let text = serde_json::to_string(t).expect("serializable");
    T::deserialize(Js::parse(&serde_json::from_str(&text).expect("json")))
}

/// Epochs as the live testnet reached them, and one past 2^32.
const EPOCHS: [u64; 5] = [0, 9, 116, 117, 1 << 33];

fn state() -> Durable {
    let mut s = Durable {
        publisher: PublisherObs {
            refused: vec![(
                "dashboard".into(),
                "dashboard state not saved; nothing written".into(),
            )],
            ..Default::default()
        },
        last_published: Some(41),
        undelivered: 2,
        undelivered_since_ms: Some(1_793_000_000_000),
        ..Default::default()
    };
    s.dashboard.epoch = Some(117);
    s.dashboard.cohort = Some(12);
    s.dashboard.stage = Some("scoring".into());
    s.dashboard.seen = EPOCHS
        .iter()
        .map(|&x| (x, 1_793_000_000 + x * 60))
        .collect();
    s
}

/// The mirror reproduces the live error with the plain `u64` key type the
/// memo had: the stored `seen` map, as JSON, stopped the state object.
#[test]
fn a_plain_u64_keyed_map_fails_through_json_parse_as_it_did_live() {
    #[derive(Debug, Serialize, Deserialize)]
    struct Plain {
        seen: BTreeMap<u64, u64>,
    }
    let plain = Plain {
        seen: [(117, 1_793_000_000)].into(),
    };
    let err = through_json_parse(&plain).unwrap_err().to_string();
    assert!(
        err.contains(r#"invalid type: string "117", expected u64"#),
        "{err}"
    );
    // Storage's `Map` form keeps the key a number: that path always loaded.
    let stored =
        Js::parse(&json!({"seen": {"117": 1_793_000_000}})).with_number_keyed_map(&["seen"]);
    assert_eq!(Plain::deserialize(stored).unwrap().seen, plain.seen);
}

/// The run's request body (`/checkpoint`, `/end`) and `/begin`'s answer are
/// `serde_json` both ways; several `u64` keys survive.
#[test]
fn the_state_calls_round_trip_the_state_as_json() {
    let s = state();
    let save = Save {
        holder: "run:1".into(),
        now_ms: 5,
        state: s.clone(),
    };
    let body = serde_json::to_vec(&save).unwrap();
    let back: Save = parse_body(&body).expect("the state object parses the body");
    assert_eq!(back.state, s);
    assert_eq!(
        back.state
            .dashboard
            .seen
            .keys()
            .copied()
            .collect::<Vec<_>>(),
        EPOCHS
    );
    let begun = serde_json::to_string(&Begun {
        state: s.clone(),
        interrupted: true,
    })
    .unwrap();
    let back: Begun = serde_json::from_str(&begun).unwrap();
    assert_eq!(back.state, s);
    assert!(back.interrupted);
}

/// The state also loads from `JSON.parse`'s object read by
/// `serde_wasm_bindgen` (the runtime's `Request::json`), from storage's
/// `Map` form, and from storage holding the JSON object form.
#[test]
fn the_state_loads_from_each_js_form_with_u64_keys() {
    let s = state();
    assert_eq!(through_json_parse(&s).unwrap(), s);
    let parsed = Js::parse(&serde_json::to_value(&s).unwrap());
    let stored = parsed.clone().with_number_keyed_map(&["dashboard", "seen"]);
    assert_eq!(Durable::deserialize(stored).unwrap(), s);
    assert_eq!(Durable::deserialize(parsed).unwrap(), s);
    let save = Save {
        holder: "run:1".into(),
        now_ms: 5,
        state: s.clone(),
    };
    assert_eq!(through_json_parse(&save).unwrap().state, s);
}

/// State saved before the memo had a `seen` map, or with an empty one,
/// still loads.
#[test]
fn older_state_without_seen_loads() {
    let mut v = serde_json::to_value(Durable::default()).unwrap();
    v["dashboard"].as_object_mut().unwrap().remove("seen");
    assert_eq!(
        Durable::deserialize(Js::parse(&v)).unwrap(),
        Durable::default()
    );
    assert_eq!(
        parse_body::<Durable>(v.to_string().as_bytes()).ok(),
        Some(Durable::default())
    );
}

/// A key is a canonical non-negative decimal: anything else is refused, not
/// guessed.
#[test]
fn malformed_keys_are_refused() {
    for bad in [
        r#"{"-1":1}"#,
        r#"{"01":1}"#,
        r#"{"":1}"#,
        r#"{"1.5":1}"#,
        r#"{"x":1}"#,
        r#"{"18446744073709551616":1}"#,
    ] {
        let text = format!(r#"{{"seen":{bad}}}"#);
        let memo: Result<crate::dashboard::Memo, _> = serde_json::from_str(&text);
        assert!(memo.is_err(), "{bad}");
        let js = Js::parse(&serde_json::from_str(&text).unwrap());
        assert!(crate::dashboard::Memo::deserialize(js).is_err(), "{bad}");
    }
    let negative = Js::parse(&json!({"seen": {}})).with_number_keyed_map(&["seen"]);
    let Js::Object(mut fields) = negative else {
        unreachable!()
    };
    fields[0].1 = Js::Map(vec![(Js::Number(-1.0), Js::Number(1.0))]);
    assert!(crate::dashboard::Memo::deserialize(Js::Object(fields)).is_err());
}
