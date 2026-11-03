use super::*;

fn success() -> Value {
    json!({"artifact_id":V6,"representation_contract_sha256":"synthetic-representation","runtime":"cpu_float32_eager",
        "labels":["human_only","model_only","mixed"],"status":"ok","token_count":2048,"logits":[0.,0.,0.],
        "probabilities":[1./3.,1./3.,1./3.],"model_or_assistance_probability":2./3.})
}
#[test]
fn exact_token_limit_is_supported_but_successful_2049_is_rejected() {
    let v = success();
    let p = checked_row(&v, "synthetic-representation")
        .unwrap()
        .unwrap();
    assert!(p.get("ai_share_percent").is_none());
    assert!(p.get("human").is_none());
    let mut bad = v;
    bad["token_count"] = json!(2049);
    assert!(checked_row(&bad, "synthetic-representation").is_err());
    bad["token_count"] = json!(2048);
    bad["truncated"] = json!(true);
    assert!(checked_row(&bad, "synthetic-representation").is_err());
}
#[test]
fn empty_long_and_failed_inputs_have_no_invented_predictions() {
    for (tokens, reason) in [(0, "empty_text"), (2049, "unsupported_length:2049>2048")] {
        let mut v = success();
        v["status"] = json!("unsupported_input");
        v["token_count"] = json!(tokens);
        v["reason"] = json!(reason);
        for k in ["logits", "probabilities", "model_or_assistance_probability"] {
            v[k] = Value::Null;
        }
        assert!(
            checked_row(&v, "synthetic-representation")
                .unwrap()
                .is_none()
        );
        v["reason"] = json!("unspecified");
        assert!(checked_row(&v, "synthetic-representation").is_err());
        v["status"] = json!("error");
        assert!(
            checked_row(&v, "synthetic-representation")
                .unwrap()
                .is_none()
        );
        v["probabilities"] = json!([0.5, 0.25, 0.25]);
        assert!(checked_row(&v, "synthetic-representation").is_err());
    }
}
#[test]
fn changed_labels_runtime_and_frozen_calibration_are_rejected() {
    for field in ["labels", "runtime", "probabilities", "representation"] {
        let mut v = success();
        match field {
            "labels" => v["labels"] = json!(["model_only", "human_only", "mixed"]),
            "runtime" => v["runtime"] = json!("cuda"),
            "probabilities" => {
                v["probabilities"] = json!([0.5, 0.25, 0.25]);
                v["model_or_assistance_probability"] = json!(0.5);
            }
            _ => v["representation_contract_sha256"] = json!("changed"),
        }
        assert!(checked_row(&v, "synthetic-representation").is_err());
    }
}
#[test]
fn representation_requires_exact_original_classifier_hook_and_temperature() {
    let r = json!({"schema":"slop_ninja_v6_penultimate_contract_v1","artifact_id":V6,"dimension":768,"embedding_dtype":"float32",
        "hook":{"module":"model.classifier","argument":0,"normalization":"none","projection":"none"},
        "input":{"truncation":false,"max_tokens_including_special_tokens":2048},"model":{"temperature":TEMPERATURE}});
    let frozen = json!({"schema":"slop_ninja_v6_embeddings_run_v1","status":"complete","dimension":768,
        "representation_contract":r,"representation_contract_sha256":b::digest(b"synthetic representation")});
    representation(&frozen, &frozen).unwrap();
    for field in ["temperature", "hook", "truncation"] {
        let mut v = frozen.clone();
        match field {
            "temperature" => v["representation_contract"]["model"]["temperature"] = json!(1.),
            "hook" => v["representation_contract"]["hook"]["module"] = json!("pooled_hidden"),
            _ => v["representation_contract"]["input"]["truncation"] = json!(true),
        }
        assert!(representation(&frozen, &v).is_err());
        assert!(representation(&v, &v).is_err());
    }
}
