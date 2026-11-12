"""FP32 raw heads of a completed full-A fit on its Train block or on a rows file.

train: every row of the fit bundle's Train block, zero-weight rows included,
       in block order (the input of `seed_mean_full_a distill-targets`).
rows:  every row of a shared-contract rows file outside the bundle whose rows
       all name the given split (train, train_mining, dev_iter or cal). Old
       Dev-select and Test are refused here: Test has its own retrospective
       route (`export_full_a.py score-rows`) and Dev-select is never scored
       outside a fit's one-time report.

Each document is one unpadded FP32 forward (as `export_full_a.py raw`), and
each output row is `train_full_a.raw_prediction`'s. The fit is checked as the
exporter checks it (`export_full_a.completed_fit`: the bound bundle and block
digests, contract, protocol, initializer, module digests and the selected
checkpoint). `manifest.json` records the fit and checkpoint digests, the AI
roster, the input digest and the output digest. No calibration, selection or
Test access. This route lives beside the exporter, not in it, so the
exporter's own bytes (which its export manifest records) stay unchanged.
"""
from __future__ import annotations

import argparse
import json
import os
from pathlib import Path

os.environ.setdefault("MKL_CBWR", "COMPATIBLE")
os.environ.setdefault("ATEN_CPU_CAPABILITY", "default")

import torch

import export_full_a as exporter
import full_a
import full_a_bundle as bundle
import shared_a as shared
import train_full_a as trainer
import v6_embeddings as bridge


RAW_HEADS_SCHEMA = "slop_ninja_full_a_raw_heads_v1"
ROW_SPLITS = ("train", "train_mining", "dev_iter", "cal")


def read(p):
    return json.loads(Path(p).read_bytes())


def rows_of_block(root, split):
    """(Meta, text) for every row of one bundle block, zero-weight rows included."""
    texts = bundle.TextSource(Path(root))
    try:
        for m, raw_row in bundle.stream(root, [split]):
            yield m, texts.text(raw_row, m)
    finally:
        texts.close()


@torch.no_grad()
def raw_heads(args, source, source_record):
    root, config, fit, out = (Path(p).resolve() for p in (args.data, args.config, args.fit, args.out))
    shared.demand(not out.exists(), "new output required")
    digests, report, binding, saved = exporter.completed_fit(root, config, fit, args.init,
                                                             getattr(args, "allow_diagnostic_subset", False))
    device = torch.device(args.device)
    trainer.configure(device, trainer.validate_protocol(read(config / "protocol.json"))["cuda_memory_fraction"])
    model, tokenizer, contract, protocol, arm, profiles = exporter.restore_model(args, root, config, binding, saved,
                                                                                device)
    tokenize, special = full_a.hf_tokenize(tokenizer), set(tokenizer.all_special_ids)
    out.mkdir()
    n, seen = 0, set()
    t = lambda v, dt: torch.tensor([v], dtype=dt, device=device)  # noqa: E731
    with (out / "raw.jsonl").open("xb") as f:
        for m, text in source(root):
            shared.demand(m.id not in seen, f"duplicate row id {m.id}")
            seen.add(m.id)
            ids, offsets = tokenize(text)
            content = full_a.content_mask_for(ids, special)
            words = full_a.word_mask_for(text, ids, offsets, special)
            o = model(t(ids, torch.long), t([1] * len(ids), torch.long), t(content, torch.bool), autocast=False,
                      words=t(words, torch.bool))
            f.write(bridge.canonical(trainer.raw_prediction(m, o, len(ids))) + b"\n")
            n += 1
    shared.demand(n > 0, "no rows")
    bridge.write_json(out / "manifest.json", {"schema": RAW_HEADS_SCHEMA, "complete": True, **source_record,
        "rows": n, "arm": binding["arm"], "seed": binding["seed"], "share_mode": arm["share_mode"],
        "ai_profiles": profiles["ai"], "bundle_sha256": digests, "contract_sha256": binding["contract_sha256"],
        "fit_sha256": bridge.sha256(fit / "fit.json"),
        "selected_checkpoint_sha256": report["selected_checkpoint_sha256"],
        "raw_sha256": bridge.sha256(out / "raw.jsonl"), "implementation_sha256": bridge.sha256(__file__),
        "exporter_sha256": bridge.sha256(exporter.__file__), "trainer_sha256": bridge.sha256(trainer.__file__),
        "precision": "fp32", "final_test_opened": False, "calibrated": False, "device": str(device),
        "torch": torch.__version__,
        "involvement_logit_semantics": "log(p_model+p_mixed)-log(p_human) of the final uncalibrated origin",
        "origin_logits_semantics": "original_classifier_linear_divided_by_base_temperature; no CAL map"})
    print(json.dumps({"complete": True, "rows": n}), flush=True)


def train(args):
    raw_heads(args, lambda root: rows_of_block(root, "train"), {"partition": "train", "source": "bundle_train_block"})


def rows(args):
    shared.demand(args.split in ROW_SPLITS, "rows-file split must be train, train_mining, dev_iter or cal")
    path = Path(args.rows).resolve(strict=True)
    raw_heads(args, lambda root: exporter.rows_of_file(path, args.split),
              {"partition": args.split, "source": "rows_file", "rows_sha256": bridge.sha256(path)})


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest="command", required=True)
    for name, func in (("train", train), ("rows", rows)):
        p = sub.add_parser(name)
        for key in ("data", "config", "fit", "artifact", "init", "out"):
            p.add_argument("--" + key, required=True)
        p.add_argument("--device", choices=("cpu", "cuda"), default="cuda")
        p.add_argument("--allow-diagnostic-subset", action="store_true",
                       help="smoke only: accept a diagnostic Train-subset fit")
        if name == "rows":
            p.add_argument("--rows", required=True, help="shared-contract rows file (JSONL)")
            p.add_argument("--split", choices=ROW_SPLITS, required=True, help="the split every row must name")
        p.set_defaults(func=func)
    a = parser.parse_args(argv)
    a.func(a)


if __name__ == "__main__":
    main()
