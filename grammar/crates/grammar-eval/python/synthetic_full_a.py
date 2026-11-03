"""Synthetic shared-contract bundles for offline trainer tests and the CPU smoke.

Programmatic word sequences only: no corpus, provider or detector text.
The real bundles are assembled in Rust; this writer mirrors their declared
shape (rows.jsonl, weights.json, MANIFEST.json) so the reader is exercised.
"""
from __future__ import annotations

from collections import Counter
import hashlib
import json
import math
from pathlib import Path
import random

import full_a
import full_a_bundle as bundle
import train_full_a as trainer

WORDS = ("the river stone field garden window morning letter quiet small village measured rainfall "
         "temperature growth basket road evening lamp harbour winter season market bridge coach "
         "traveller old new careful bright narrow distant open plain").split()


def synthetic_text(rng, words):
    out, sentence = [], []
    for _ in range(words):
        sentence.append(rng.choice(WORDS))
        if len(sentence) >= rng.randint(6, 14):
            out.append(" ".join(sentence).capitalize() + ".")
            sentence = []
    if sentence:
        out.append(" ".join(sentence).capitalize() + ".")
    return " ".join(out)


def row(id, split, text, origin, label=None, share=None, spans=None, cohort="local_v1", cohort_weight=1.,
        group="g0", domain="wikipedia", voice=None, cluster=None, axes=None, held_out=False, operation=None):
    share = {"human_only": 0., "model_only": 1.}.get(origin, share)
    return {"id": id, "split": split, "text": text, "text_sha256": hashlib.sha256(text.encode()).hexdigest(),
            "origin": origin, "share": share, "ai_byte_spans": spans, "generator_label": label,
            "cohort_id": cohort, "cohort_weight": cohort_weight, "source_group": group, "domain": domain,
            "genre": None, "voice_id": voice, "voice_cluster_id": cluster, "voice_origin": None if voice is None
            else "invented", "style_axes": axes, "operation": operation, "held_out_style": held_out,
            "provenance_sha256": hashlib.sha256(("prov:" + id).encode()).hexdigest()}


def mixed_spans(text, rng):
    """One replacement span over a whole word run (UTF-8 byte offsets)."""
    words = [(m.start(), m.end()) for m in __import__("re").finditer(r"\S+", text)]
    a = rng.randrange(0, max(1, len(words) - 3))
    b = min(len(words) - 1, a + rng.randint(1, 3))
    s, e = words[a][0], words[b][1]
    return [[len(text[:s].encode()), len(text[:e].encode())]], (b - a + 1) / len(words)


def weights_doc(rows, policies=None):
    """weights.json declaring every Train cohort and label with its row weight."""
    cohorts = {}
    counts = Counter((r["cohort_id"], bundle.label_key(r["generator_label"])) for r in rows if r["split"] == "train")
    for (cohort, key), n in sorted(counts.items()):
        w = {r["cohort_weight"] for r in rows if r["split"] == "train" and r["cohort_id"] == cohort and
             bundle.label_key(r["generator_label"]) == key}
        assert len(w) == 1
        c = cohorts.setdefault(cohort, {"policy": (policies or {}).get(cohort, "uniform_v1"), "default": True,
                                        "labels": {}})
        c["labels"][key] = {"records": n, "weight": w.pop()}
    return {"schema": "slop_ninja_a_bundle_weights_v1", "base_weighting": "family_origin_balanced",
            "cohorts": cohorts}


def row_weights_doc(rows):
    """Trainer weights.json in the assembler_row_weight_v1 form: per cohort and
    label the Train record count, weight sum, minimum and maximum."""
    cohorts, total = {}, 0.
    for r in rows:
        if r["split"] != "train":
            continue
        c = cohorts.setdefault(r["cohort_id"], {"policy": "uniform", "default": True, "cohort_weight": 1.,
                                                "labels": {}})
        e = c["labels"].setdefault(bundle.label_key(r["generator_label"]),
                                   {"records": 0, "weight_sum": 0., "weight_min": math.inf, "weight_max": -math.inf})
        w = r["cohort_weight"]
        e["records"] += 1
        e["weight_sum"] += w
        e["weight_min"], e["weight_max"] = min(e["weight_min"], w), max(e["weight_max"], w)
        total += w
    return {"schema": "slop_ninja_a_trainer_weights_v1", "base_weighting": bundle.ROW_WEIGHT_SCHEME,
            "train_weight_sum": total, "cohorts": cohorts}


def write_bundle(root, rows, weights=None):
    root = Path(root)
    root.mkdir(parents=True, exist_ok=True)
    with (root / bundle.ROWS).open("w") as f:
        for r in rows:
            f.write(json.dumps(r) + "\n")
    (root / bundle.WEIGHTS).write_text(json.dumps(weights or weights_doc(rows)))
    counts = Counter((r["split"], r["domain"], r["origin"], r["generator_label"]) for r in rows)
    (root / bundle.MANIFEST).write_text(json.dumps({
        "schema": "slop_ninja_a_bundle_manifest_v1", "inputs": [],
        "counts": [{"split": s, "domain": d, "origin": o, "generator_label": g, "rows": n}
                   for (s, d, o, g), n in sorted(counts.items(), key=lambda kv: tuple(map(str, kv[0])))]}))
    return bundle.bundle_digests(root)


def protocol_template():
    plain_share = {"conditional_loss_weight": 0., "bins": None}
    none = {"span_head": None, "share_mode": "head", "share_supervision": plain_share,
            "batching": {"kind": "random"}, "style_adversary": None, "contrastive": None}
    share = {"conditional_loss_weight": 1., "bins": {"loss_weight": .5, "hidden": 16}}
    span = {"pooling": "attention", "token_loss_weight": 1.}
    adversary = {"loss_weight": 1., "hidden": 16, "min_cluster_rows": 1, "axes": True,
                 "reversal": {"kind": "dann", "max": .5, "ramp_epochs": 1, "gamma": 10.}}
    contrastive = {"loss_weight": .5, "temperature": .2, "projection_hidden": 16, "projection_dim": 8}
    mixed = {"kind": "voice_mixed", "per_generator": 2}
    return {"schema": trainer.PROTOCOL_SCHEMA, "status": "frozen", "final_test_opened": False,
            "seeds": [17, 29, 41], "max_epochs": 2, "logical_batch_size": 6, "microbatch_token_budget": 2048,
            "max_tokens": 2048,
            "optimizer": {"name": "AdamW", "encoder_peak_learning_rate": 1e-4, "layer_lr_decay": .9,
                          "heads_learning_rate": 1e-3, "weight_decay": .01, "betas": [.9, .999], "eps": 1e-8,
                          "gradient_clip_norm": 1., "aux_gradient_clip_norm": 1.},
            "schedule": {"name": trainer.SCHEDULE, "warmup_fraction_of_total_steps": .05, "shape": "cosine", "horizon_epochs": 1.5,
                         "floor_fraction": 0.},
            "precision": {"train": "fp32", "evaluate": "fp32"}, "gradient_checkpointing": True,
            "attention_implementation": "eager",
            "early_stopping": {"metric": "dev_iter_uncalibrated_balanced_involvement_brier", "split": "dev_iter",
                               "evaluations_per_epoch": 2, "patience_evaluations": 10, "min_delta": 0.},
            "cuda_memory_fraction": .45, "attribution": {"loss_weight": 1.},
            "train_weighting": {"scheme": "family_origin_balanced", "positive_mass": None},
            "arms": {"E1": dict(none),
                     "E2w": dict(none, span_head=span, share_mode="word_token_mean_projected"),
                     "E1share": dict(none, share_supervision=share),
                     "E2wshare": dict(none, span_head=span, share_mode="word_token_mean_projected",
                                      share_supervision=share),
                     "E1adv": dict(none, batching=mixed, style_adversary=adversary),
                     "E1con": dict(none, batching=mixed, contrastive=contrastive),
                     "E2wall": dict(none, span_head=span, share_mode="word_token_mean_projected", batching=mixed,
                                    share_supervision=share, style_adversary=adversary, contrastive=contrastive)}}


def contract_for(manifest_sha, ai_profiles, parent="synthetic-parent"):
    return {"schema": trainer.CONTRACT_SCHEMA, "parent_artifact_id": parent, "bundle_manifest_sha256": manifest_sha,
            "input_mode": "text", "raw_dimension": 768, "zero_cfg_coordinates": full_a.CFG_ZEROS,
            "maximum_tokens_including_specials": full_a.MAX_TOKENS, "token_truncation": False,
            "origin_order": ["human_only", "model_only", "mixed"], "span_label_policy": full_a.SPAN_LABEL_POLICY,
            "span_coordinates": "utf8_byte_offsets_in_exact_final_text", "human_profiles": [],
            "ai_profiles": sorted(ai_profiles),
            "embedding_normalization": {"fit": "train_query_only_equal_source_family_weight", "dimension": 768,
                                        "mean": [0.] * 768, "scale": [1.] * 768, "calibration_used_for_fit": False,
                                        "development_used_for_fit": False, "reference_used_for_fit": False,
                                        "final_test_opened": False}}


def write_config(config, manifest_sha, ai_profiles, protocol=None):
    config = Path(config)
    config.mkdir(parents=True, exist_ok=True)
    (config / "contract.json").write_text(json.dumps(contract_for(manifest_sha, ai_profiles)))
    (config / "protocol.json").write_text(json.dumps(protocol or protocol_template()))


GENERATORS = (("gen-a", "local_v1", 1.), ("gen-b", "local_v1", 1.), ("claude-x", "hosted_frontier_v1", 1.5))
VOICES = (("voice-1", "vc-1", ["character_genre"]), ("voice-2", "vc-2", ["regional_english"]),
          ("voice-3", "vc-3", ["character_genre", "emotional_tone"]))


def factorial_rows(seed=5, groups=4, words=(12, 30), splits=("train", "dev_iter"), held_out_dev=True):
    """Humans plus generator x voice positives per source group, per split."""
    rng = random.Random(seed)
    rows = []
    for split in splits:
        for g in range(groups):
            group = f"{split}-work-{g}"
            human = synthetic_text(rng, rng.randint(*words))
            rows.append(row(f"{split}-h{g}", split, human, "human_only", group=group, cohort="humans_v1"))
            for label, cohort, weight in GENERATORS:
                for v, (voice, cluster, axes) in enumerate(VOICES):
                    held = held_out_dev and split != "train" and v == 2
                    text = synthetic_text(rng, rng.randint(*words))
                    rid = f"{split}-{label}-{g}-{v}"
                    if (g + v) % 2:
                        spans, share = mixed_spans(text, rng)
                        rows.append(row(rid, split, text, "mixed", label, share, spans, cohort, weight, group,
                                        voice=voice, cluster=cluster, axes=axes, held_out=held, operation="splice"))
                    else:
                        rows.append(row(rid, split, text, "model_only", label, cohort=cohort, cohort_weight=weight,
                                        group=group, voice=voice, cluster=cluster, axes=axes, held_out=held,
                                        operation="mirror"))
    return rows
