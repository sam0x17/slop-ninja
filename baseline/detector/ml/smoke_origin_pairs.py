"""One synthetic paired MPS optimizer step; no corpus or detector-quality claim."""
import argparse
import json
from pathlib import Path
import time

import torch
from common import configure_cpu, load_checkpoint, sha256, software
from train import paired_origin_loss


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--checkpoint", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--anchor-tokens", type=int, default=64)
    parser.add_argument("--peer-tokens", type=int, default=80)
    args = parser.parse_args()
    if not all(1 <= n <= 2048 for n in (args.anchor_tokens, args.peer_tokens)):
        parser.error("synthetic token lengths must be between 1 and 2048")
    if args.out.exists():
        raise ValueError("use a new smoke output")
    configure_cpu()
    _, model = load_checkpoint(args.checkpoint, 17)
    model.to("mps").train()
    generator = torch.Generator().manual_seed(31)
    anchor = torch.randint(256, 40000, (4, args.anchor_tokens), generator=generator).to("mps")
    peer = torch.randint(256, 40000, (4, args.peer_tokens), generator=generator).to("mps")
    labels = torch.tensor([0, 1, 1, 2], device="mps")
    weights = torch.tensor([1., 0.5, 0.5, 2.], device="mps")
    optimizer = torch.optim.AdamW(model.parameters(), lr=0.00001, weight_decay=0.01)
    torch.mps.synchronize()
    start = time.monotonic()
    left = model(input_ids=anchor, attention_mask=torch.ones_like(anchor)).logits
    right = model(input_ids=peer, attention_mask=torch.ones_like(peer)).logits
    loss, divergence = paired_origin_loss(left, right, labels, weights, 0.5)
    if not torch.isfinite(loss) or not torch.isfinite(divergence):
        raise ValueError("nonfinite paired objective")
    loss.backward()
    norm = torch.nn.utils.clip_grad_norm_(model.parameters(), 1.0, error_if_nonfinite=True)
    optimizer.step()
    torch.mps.synchronize()
    result = {"schema": "slop_ninja_origin_pairs_mps_smoke_v1", "status": "passed",
              "synthetic_only": True, "corpus_loaded": False, "device": "mps", "software": software(),
              "anchor_shape": list(anchor.shape), "peer_shape": list(peer.shape),
              "parameters": sum(p.numel() for p in model.parameters()),
              "seconds": time.monotonic() - start, "loss": float(loss.detach().cpu()),
              "js_divergence": float(divergence.detach().cpu()), "gradient_norm": float(norm.detach().cpu()),
              "current_allocated_bytes": torch.mps.current_allocated_memory(),
              "driver_allocated_bytes": torch.mps.driver_allocated_memory(),
              "checkpoint_pin_sha256": sha256(args.checkpoint / "checkpoint.json"),
              "training_source_sha256": sha256(Path(__file__).with_name("train.py")),
              "smoke_source_sha256": sha256(Path(__file__))}
    with args.out.open("x") as out:
        out.write(json.dumps(result, indent=2) + "\n")
    print(json.dumps(result))


if __name__ == "__main__":
    main()
