"""spans-S2 span-tower golden vectors (synthetic weights and token ids only; no corpus text).

The trainer's own `full_a_s2.TowerFullA` on a tiny 22-layer ModernBERT (eager attention, the encoder layout of
ModernBERT-base: global attention every third layer, local elsewhere), evaluated by the export's FP64 reference
function (`export_full_a.fp64_reference` under `export_full_a.reference_attention`). Every weight is a dyadic
value from a fixed generator that the Rust test regenerates bit for bit, so the file carries only the inputs
and the expected outputs: the encoder's final-norm token states, the span tower's normed states and the token
head's logits. The Rust runtime branch (`inference/a-v6-cpu`: the hidden state entering the first copied layer,
the copied layers with the encoder's masks and positions, the copied final norm, then the token head) must
reproduce them in its FP64 profile.

usage: python span_tower_golden.py golden OUT.json
"""
from __future__ import annotations

import json
import os
import sys

os.environ.setdefault("MKL_CBWR", "COMPATIBLE")
os.environ.setdefault("ATEN_CPU_CAPABILITY", "default")

import torch
from torch import nn

import export_full_a as ex
import full_a
import full_a_s2 as s2

GOLDEN_SCHEMA = "slop_ninja_span_tower_golden_v1"
WEIGHT_RULE = ("slop_ninja_tower_golden_weights_v1: per tensor (state-dict name), splitmix64 seeded with "
               "fnv1a64(name); u = ((x >> 48) - 32768) / 32768 per value in row-major order; "
               "*norm.weight = 1 + u/16, tok_embeddings.weight = u, every other tensor = u/8")
CONFIG = {"vocab_size": 64, "hidden_size": 128, "num_hidden_layers": 22, "num_attention_heads": 2,
          "intermediate_size": 32, "max_position_embeddings": 64, "layer_norm_eps": 1e-5, "pad_token_id": 0,
          "global_attn_every_n_layers": 3, "global_rope_theta": 160000.0, "local_attention": 8,
          "local_rope_theta": 10000.0}
TOWER_LAYERS = 4
TOKEN_HEAD_WIDTH = 8
CLS, SEP, PAD = 1, 2, 0
# One token sees only position 0, where every rotary table entry is exact (cos 1, sin 0); longer cases also
# carry the float32 cos/sin rounding of each runtime's table (torch's vectorized float32 cos/sin and glibc's
# cosf/sinf differ by one ulp in some entries).
CASES = [
    ("one-token", [CLS]),
    ("six-tokens", [CLS] + [(5 * i + 7) % 61 + 3 for i in range(4)] + [SEP]),
    ("twenty-tokens", [CLS] + [(7 * i + 3) % 61 + 3 for i in range(18)] + [SEP]),
    ("forty-tokens", [CLS] + [(11 * i + 5) % 61 + 3 for i in range(38)] + [SEP]),
]
MASK64 = (1 << 64) - 1


def fnv1a64(data):
    h = 0xcbf29ce484222325
    for b in data:
        h = ((h ^ b) * 0x100000001b3) & MASK64
    return h


def splitmix64(seed):
    state = seed
    while True:
        state = (state + 0x9e3779b97f4a7c15) & MASK64
        z = state
        z = ((z ^ (z >> 30)) * 0xbf58476d1ce4e5b9) & MASK64
        z = ((z ^ (z >> 27)) * 0x94d049bb133111eb) & MASK64
        yield z ^ (z >> 31)


def golden_values(name, count):
    """The generator's values for one tensor, exactly representable in float32."""
    rng = splitmix64(fnv1a64(name.encode()))
    us = [((next(rng) >> 48) - 32768) / 32768 for _ in range(count)]
    if name.endswith("norm.weight"):
        return [1. + u / 16 for u in us]
    if name.endswith("tok_embeddings.weight"):
        return us
    return [u / 8 for u in us]


def fill(module, prefix):
    with torch.no_grad():
        for name, p in module.named_parameters():
            v = torch.tensor(golden_values(prefix + name, p.numel()), dtype=torch.float64).to(p.dtype).view(p.shape)
            full_a.demand(torch.equal(v.double().view(-1), torch.tensor(golden_values(prefix + name, p.numel()),
                                                                          dtype=torch.float64)),
                          "golden weights must be exact in float32")
            p.copy_(v)


class StubHeads(nn.Module):
    """Document heads stand-in: the tower path never reads them (TowerFullA keeps them on the frozen path)."""

    def forward(self, x):
        n = x.shape[0]
        return {"encoding": x[:, :CONFIG["hidden_size"]], "origin_logits": x.new_zeros((n, 3)),
                "human_logits": x.new_zeros((n, 2)), "ai_logits": x.new_zeros((n, 2)),
                "conditional_mixed_share": x.new_zeros((n,))}


def build():
    from transformers import ModernBertConfig, ModernBertForSequenceClassification
    c = CONFIG
    config = ModernBertConfig(vocab_size=c["vocab_size"], hidden_size=c["hidden_size"],
                              intermediate_size=c["intermediate_size"], num_hidden_layers=c["num_hidden_layers"],
                              num_attention_heads=c["num_attention_heads"],
                              max_position_embeddings=c["max_position_embeddings"], norm_eps=c["layer_norm_eps"],
                              local_attention=c["local_attention"],
                              global_attn_every_n_layers=c["global_attn_every_n_layers"],
                              global_rope_theta=c["global_rope_theta"], local_rope_theta=c["local_rope_theta"],
                              pad_token_id=PAD, bos_token_id=CLS, eos_token_id=SEP, cls_token_id=CLS,
                              sep_token_id=SEP, classifier_pooling="mean", num_labels=3, attention_dropout=0.,
                              embedding_dropout=0., mlp_dropout=0., classifier_dropout=0., reference_compile=False)
    config._attn_implementation = "eager"
    encoder = ModernBertForSequenceClassification(config).eval()
    fill(encoder, "")
    head = full_a.TokenHead(hidden=c["hidden_size"], width=TOKEN_HEAD_WIDTH)
    fill(head, "token_head.")
    model = s2.TowerFullA(encoder, StubHeads(), token_head=head, tower_layers=TOWER_LAYERS)
    model.attach_tower()
    # Own tower weights (not the copies' initial values), so a runtime that read the encoder's layers fails.
    fill(model.span_tower, ex.TOWER_PREFIX)
    return model.eval()


def flat(t):
    return t[0].double().flatten().tolist()


def golden_cases():
    model = build()
    m64, _ = ex.fp64_reference(model)
    ex.tower_rotary(m64)
    captured = {}
    m64.encoder.model.final_norm.register_forward_hook(lambda m, a, out: captured.__setitem__("final", out))
    m64.span_tower.register_forward_hook(lambda m, a, out: captured.__setitem__("tower", out))
    cases = []
    with torch.no_grad(), ex.reference_attention():
        for cid, ids in CASES:
            t = torch.tensor([ids])
            # The content mask feeds only an E2 span head (none here); one token has no CLS...SEP frame.
            content = torch.tensor([full_a.content_mask_for(ids, {PAD, CLS, SEP}) if len(ids) > 1 else [False]])
            o = m64(t, torch.ones_like(t), content, autocast=False)
            full_a.demand(o["span_logits"].dtype == torch.float64, "golden reference must be float64")
            cases.append({"id": cid, "input_ids": ids, "final_norm_states": flat(captured["final"]),
                          "span_tower_states": flat(captured["tower"]),
                          "token_logits": o["span_logits"][0].double().tolist()})
    return {"schema": GOLDEN_SCHEMA,
            "generator": "span_tower_golden.py golden_cases (synthetic weights and token ids only)",
            "rules": {"tower": s2.TOWER_RULE, "token_head": full_a.TOKEN_HEAD_RULE, "weights": WEIGHT_RULE,
                      "reference_function": ex.REFERENCE_FUNCTION, "reference_rotary": ex.REFERENCE_ROTARY,
                      "token_input": ex.TOWER_INPUT},
            "config": CONFIG, "token_head_width": TOKEN_HEAD_WIDTH, "tower": ex.tower_declaration(m64),
            "tower_tensors": ex.tower_tensor_names(TOWER_LAYERS), "cases": cases}


def golden_bytes(value=None):
    value = golden_cases() if value is None else value
    return json.dumps(value, sort_keys=True, indent=1, ensure_ascii=False, allow_nan=False).encode() + b"\n"


if __name__ == "__main__":
    full_a.demand(len(sys.argv) == 3 and sys.argv[1] == "golden", "usage: span_tower_golden.py golden OUT.json")
    with open(sys.argv[2], "xb") as f:
        f.write(golden_bytes())
