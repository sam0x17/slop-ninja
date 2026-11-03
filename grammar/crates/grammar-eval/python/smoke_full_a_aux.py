"""CPU smoke for the extended full-A trainer. Synthetic text only; no GPU.

memory: a synthetic N-row shared-contract bundle (text_ref into a blob) is
        streamed into a Train Population with the real V6 tokenizer; reports
        peak RSS, packed array bytes, load time and the voice-mixed order time.
loop:   a tiny random ModernBERT fits every auxiliary objective on a factorial
        bundle, scores a synthetic Train-mining pool, then the Rust tool
        selects mined humans, queues matched-positive requests, extends the
        bundle, a round-1 fit runs on it and the stop rule is applied.
"""
from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
import random
import subprocess
import time
from unittest import mock

import full_a
import full_a_aux as aux
import full_a_bundle as bundle
import mine_full_a
import synthetic_full_a as synthetic
import train_full_a as trainer
import v6_embeddings as bridge


def rss():
    return bundle.peak_rss_bytes()


def memory(args, out):
    from transformers import AutoTokenizer
    tokenizer = AutoTokenizer.from_pretrained(Path(args.artifact) / "classifier", local_files_only=True)
    root = out / "memory-bundle"
    root.mkdir(parents=True)
    rng = random.Random(7)
    gens = [("gen-%d" % i, "local_v1", 1.) for i in range(8)] + [("claude-%d" % i, "hosted_frontier_v1", 1.)
                                                               for i in range(4)]
    voices = [("voice-%d" % i, "vc-%d" % (i % 900), ["axis-%d" % (i % 13)]) for i in range(3000)]
    rows = []
    t0 = time.monotonic()
    with (root / "texts.bin").open("wb") as blob:
        for i in range(args.rows):
            text = synthetic.synthetic_text(rng, rng.randint(args.words // 2, args.words))
            group = "work-%d" % (i // 20)
            kind = i % 4
            if kind == 0:
                r = synthetic.row(f"r{i}", "train", text, "human_only", group=group, cohort="humans_v1")
            else:
                label, cohort, w = gens[i % len(gens)]
                voice, cluster, axes = voices[rng.randrange(len(voices))]
                if kind == 1:
                    spans, share = synthetic.mixed_spans(text, rng)
                    r = synthetic.row(f"r{i}", "train", text, "mixed", label, share, spans, cohort, w, group,
                                      voice=voice, cluster=cluster, axes=axes)
                else:
                    r = synthetic.row(f"r{i}", "train", text, "model_only", label, cohort=cohort, cohort_weight=w,
                                      group=group, voice=voice, cluster=cluster, axes=axes)
            data = r.pop("text").encode()
            r["text_ref"] = {"path": "texts.bin", "offset": blob.tell(), "length": len(data)}
            blob.write(data)
            rows.append(r)
    synthetic.write_bundle(root, rows)
    labels = sorted({r["generator_label"] for r in rows if r["generator_label"]})
    del rows
    written = time.monotonic() - t0
    before = rss()
    t0 = time.monotonic()
    pop = bundle.Population(root, "train", full_a.hf_tokenize(tokenizer), set(tokenizer.all_special_ids), labels,
                            cohort_weighted=True)
    load = time.monotonic() - t0
    after = rss()
    t0 = time.monotonic()
    order = aux.voice_mixed_order(*trainer.contrastive_keys(pop), 32, 4, seed=17)
    order_seconds = time.monotonic() - t0
    structure = aux.batch_structure(order[:32 * 200], *trainer.contrastive_keys(pop), 32)
    report = {"rows": len(pop), "bundle_write_seconds": written, "load_seconds": load,
              "rows_per_second": len(pop) / load, "peak_rss_before_load_bytes": before,
              "peak_rss_after_load_bytes": after, "population": pop.token_counts(),
              "voice_mixed_order_seconds": order_seconds, "batch_structure_first_200_batches": structure,
              "tokenizer": "V6 artifact tokenizer", "words_per_row_max": args.words}
    for f in root.iterdir():  # synthetic data only; keep the report
        f.unlink()
    root.rmdir()
    return report


def pool_doc(i, rng):
    text = synthetic.synthetic_text(rng, rng.randint(20, 40))
    return {"id": f"pool-{i}", "frozen_split": "train_mining", "split_assignment": {"split": "train_mining"},
            "origin": "human_only", "text": text, "text_sha256": hashlib.sha256(text.encode()).hexdigest(),
            "source_group": f"mining-fam-{i // 2}", "writer_key": f"writer-{i // 4}", "domain": "fineweb",
            "word_count": len(text.split()), "collection": "synthetic"}


def synthetic_splits(root, keys):
    """A synthetic splits directory: one manifest row per evaluation split,
    `keys` {split: (family, writer)} overriding the defaults, plus a
    diagnostic slice; SHA256SUMS lists every file (metadata only)."""
    files = {}
    for split, folder in (("cal", "cal"), ("dev_iter", "dev-iter"), ("dev_select", "dev-select"), ("test", "test")):
        family, writer = keys.get(split, (f"eval-fam-{split}", f"eval-writer-{split}"))
        files[f"{folder}/manifest.jsonl"] = {"split": split, "family_key": family, "group_key": writer,
                                             "text_sha256": hashlib.sha256(split.encode()).hexdigest()}
    files["diagnostics/synthetic/manifest.jsonl"] = {"split": "diagnostic_synthetic", "source_group": "diag-fam",
                                                     "writer_key": "diag-writer", "text_sha256": "0" * 64}
    lines = []
    for rel, row in files.items():
        path = root / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps(row) + "\n")
        lines.append(f"{bridge.sha256(path)}  ./{rel}\n")
    (root / "SHA256SUMS").write_text("".join(lines))
    return root


def loop(args, out):
    import test_full_a as t
    rust = Path(args.rust_bin)
    encoder = t.tiny_encoder(11)
    init = t.init_for(encoder)
    patches = (mock.patch.object(trainer, "load", lambda a, p, d: (encoder, t.FakeTokenizer(),
                                                                   {"artifact_id": "synthetic-parent"})),
               mock.patch.object(trainer, "initializer", lambda path: init),
               mock.patch.object(mine_full_a.exporter, "initializer", lambda path: init))
    for p in patches:
        p.start()
    report = {}
    rows = synthetic.factorial_rows(groups=3, splits=("train", "dev_iter"))
    digests = synthetic.write_bundle(out / "bundle-r0", rows)
    labels = sorted({r["generator_label"] for r in rows if r["generator_label"]})
    synthetic.write_config(out / "config-r0", digests[bundle.MANIFEST], labels)
    (out / "init.json").write_text("{}")

    def fit(data, config, name):
        a = type("A", (), {"data": str(data), "config": str(config), "artifact": "unused",
                           "init": str(out / "init.json"), "out": str(out / name), "arm": "E2wall", "seed": 17,
                           "device": "cpu", "report_split": None})
        t0 = time.monotonic()
        trainer.run(a)
        f = json.loads((out / name / "fit.json").read_text())
        return {"seconds": time.monotonic() - t0, "selected_evaluation": f["selected_evaluation"],
                "dev_iter_brier": f["selection_split_involvement_brier"],
                "batch_structure": f["aux_heads"]["batch_structure"],
                "attribution_accuracy": f["selected_dev_iter_diagnostics"]["attribution_accuracy"]["__all__"]}

    report["fit_round_0"] = fit(out / "bundle-r0", out / "config-r0", "fit-r0")
    rng = random.Random(3)
    pool = out / "pool.jsonl"
    with pool.open("w") as f:
        for i in range(40):
            f.write(json.dumps(pool_doc(i, rng)) + "\n")
    splits = synthetic_splits(out / "splits", {"cal": ("mining-fam-0", "writer-9")})
    s = type("S", (), {"data": str(out / "bundle-r0"), "config": str(out / "config-r0"), "fit": str(out / "fit-r0"),
                       "artifact": "unused", "init": str(out / "init.json"), "pool": str(pool),
                       "out": str(out / "scores-r1"), "device": "cpu", "precision": "fp32", "token_budget": 4096,
                       "limit": 0, "sample_seed": "smoke", "sample_keep": 0, "sample_modulus": 0})
    mine_full_a.score(s)
    run = lambda *a: json.loads(subprocess.run([str(rust), *map(str, a)], check=True, capture_output=True,
                                               text=True).stdout.strip().splitlines()[-1])
    report["select"] = run("select", "--scores", out / "scores-r1", "--pool", pool, "--bundle", out / "bundle-r0",
                           "--splits", splits, "--round", 1, "--top", 10, "--per-group-cap", 1, "--min-p", 0,
                           "--generators", "gen-a,gen-b", "--queue", out / "queue", "--out", out / "round-1")
    round_json = json.loads((out / "round-1" / "round.json").read_text())
    report["select_exclusions"] = round_json["exclusions"]
    report["extend"] = run("extend", "--bundle", out / "bundle-r0", "--add", out / "round-1" / "mined-rows.jsonl",
                           "--out", out / "bundle-r1")
    d1 = bundle.bundle_digests(out / "bundle-r1")
    synthetic.write_config(out / "config-r1", d1[bundle.MANIFEST], labels)
    report["fit_round_1"] = fit(out / "bundle-r1", out / "config-r1", "fit-r1")
    report["stop"] = run("stop", "--round-fits", out / "fit-r0", "--round-bundle", out / "bundle-r0",
                         "--round-fits", out / "fit-r1", "--round-bundle", out / "bundle-r1",
                         "--out", out / "stop-1.json")
    queue = json.loads((out / "queue" / "QUEUE.json").read_text())
    report["queue"] = {"rounds": queue["rounds"],
                       "first_request_fields": sorted(json.loads((out / "queue" / "round-1-requests.jsonl")
                                                             .read_text().splitlines()[0]))}
    log = json.loads((out / "fit-r1" / "cohort-weights.json").read_text())
    report["round_1_cohorts"] = {c: v["records"] for c, v in log["cohorts"].items()}
    for p in patches:
        p.stop()
    return report


if __name__ == "__main__":
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--out", required=True)
    p.add_argument("--artifact", required=True)
    p.add_argument("--rust-bin", required=True)
    p.add_argument("--rows", type=int, default=100000)
    p.add_argument("--words", type=int, default=260)
    p.add_argument("--part", choices=("memory", "loop", "both"), default="both")
    a = p.parse_args()
    out = Path(a.out)
    out.mkdir(parents=True)
    result = {"schema": "slop_ninja_full_a_aux_cpu_smoke_v1", "synthetic_only": True, "final_test_opened": False,
              "implementation_sha256": {m.__name__: bridge.sha256(m.__file__) for m in
                                        (trainer, aux, bundle, full_a, mine_full_a)}}
    if a.part in ("loop", "both"):
        result["loop"] = loop(a, out)
        bridge.write_json(out / "report.json", result)
    if a.part in ("memory", "both"):
        result["memory"] = memory(a, out)
    bridge.write_json(out / "report.json", result)
    print(json.dumps({"complete": True}), flush=True)
