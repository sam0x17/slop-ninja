//! A scorer reply's span fields (`slop_ninja_span_emission_v1`), read into an
//! [`Emission`](crate::Emission): `span_status` absent means the artifact has
//! no token head (no emission ran); `"ok"` means `prediction.spans` holds the
//! decoded spans; anything else is an emission failure.
use serde_json::Value;

use crate::{Emission, Span};

/// What a reply says about one document's spans.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SpanReply {
    PredictionFailed,
    NotEmitted,
    Failed,
    Spans(Vec<Span>),
    Malformed,
}

impl SpanReply {
    pub fn emission(&self) -> Emission<'_> {
        match self {
            Self::PredictionFailed => Emission::PredictionFailed,
            Self::NotEmitted => Emission::NotEmitted,
            Self::Failed => Emission::Failed,
            Self::Spans(s) => Emission::Spans(s),
            Self::Malformed => Emission::Malformed,
        }
    }
}

fn u32_field(v: &Value, k: &str) -> Option<u32> {
    v.get(k)?.as_u64().and_then(|x| u32::try_from(x).ok())
}

/// Reads one scorer result. `prediction_ok` is whether the document outcome
/// is a valid prediction (the coordinator's own reading of the reply); when
/// it is not, the document has no spans whatever the reply says.
pub fn span_reply(result: &Value, prediction_ok: bool) -> SpanReply {
    if !prediction_ok {
        return SpanReply::PredictionFailed;
    }
    match result.get("span_status") {
        None | Some(Value::Null) => SpanReply::NotEmitted,
        Some(Value::String(s)) if s == "ok" => {
            let Some(arr) = result.get("prediction").and_then(|p| p.get("spans")).and_then(Value::as_array) else {
                return SpanReply::Malformed;
            };
            let mut out = Vec::with_capacity(arr.len());
            for s in arr {
                match (u32_field(s, "start_byte"), u32_field(s, "end_byte"), u32_field(s, "words")) {
                    (Some(start_byte), Some(end_byte), Some(words)) => out.push(Span { start_byte, end_byte, words }),
                    _ => return SpanReply::Malformed,
                }
            }
            SpanReply::Spans(out)
        }
        Some(_) => SpanReply::Failed,
    }
}
