//! Small synthetic numeric artifacts only; no retained fitted weights or prose.
use serde_json::{Value, json};
pub(super) fn contract() -> Value {
    json!({"input_dimension":2,"human_profiles":["human-a","human-b"],"ai_profiles":["ai-a","ai-b"]})
}
pub(super) fn weights() -> Value {
    let mut tensors = serde_json::Map::new();
    for (name, rows, cols) in [
        ("encoder.0", 2, 2),
        ("encoder.2", 2, 2),
        ("origin", 3, 2),
        ("mixed_share", 1, 2),
        ("human", 2, 2),
        ("ai", 2, 2),
    ] {
        tensors.insert(
            format!("{name}.weight"),
            json!({"shape":[rows,cols],"values":vec![0.;rows*cols]}),
        );
        tensors.insert(
            format!("{name}.bias"),
            json!({"shape":[rows],"values":vec![0.;rows]}),
        );
    }
    json!({"schema":"slop_ninja_shared_a_weights_v1","architecture":{"dimension":2,"hidden":2,"embedding":2,"human_profiles":2,"ai_profiles":2},
        "operations":["linear","tanh","softmax","sigmoid"],"contract_sha256":"a".repeat(64),"temperature":1.,
        "profiles":{"human":["human-a","human-b"],"ai":["ai-a","ai-b"]},"thresholds":{"human":0.6,"ai":0.6},
        "supervision":{"origin":3,"share":3,"mixed_fractional_share":1,"human":2,"ai":2},"origin_order":["human_only","model_only","mixed"],"control":"joint","weights":tensors})
}
pub(super) fn row(id: &str) -> Value {
    json!({"id":id,"source_group":format!("family-{id}"),"frozen_split":"test","purpose":"query",
    "input":{"contract_sha256":"a".repeat(64),"x":[0.,0.]},"targets":{"origin":0,"ai_involvement":false,"share":0.,"human":0,"ai":null},
    "roles":{"human":{"present":true,"identity":"human-a","known":true},"ai":{"present":false,"identity":null,"known":false}}})
}
