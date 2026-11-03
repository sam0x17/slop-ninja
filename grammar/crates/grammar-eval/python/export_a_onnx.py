"""Qualified ONNX export of an A release model: the banded f32 graph the scorer serves.

usage: export_a_onnx.py --release-dir DIR --variant plain|span|tower --out FILE.onnx
                        [--record FILE.json] [--compare-graph REF.onnx] [--allow-unpinned]
       export_a_onnx.py structure FILE.onnx     print the structure digest only

Input is an A release package in the released layout:
  DIR/runtime-base/config.json   ModernBERT-base classifier config (the V6 parent's)
  DIR/model/encoder.safetensors  the fine-tuned encoder
  DIR/model/heads.json           slop_ninja_full_a_heads_v1 document heads (tower: heads v3)
  DIR/model/contract.json        slop_ninja_full_a_contract_v2 (frozen normalization)
  DIR/model/tower.safetensors    tower only: the span tower of a spans-S2 S2c export (export v4)

Variants (the allow-listed graph classes of the artifact spec,
docs/testnet/artifact-spec.md):
  plain  outputs origin_logits, involvement_logit, conditional_mixed_share, ai_logits,
         human_logits, h (R0's graph; the scorer reads h)
  span   the same graph plus token_states, the final-norm token states a span head reads
  tower  the plain graph plus span_tower_token_states: the span tower declared by heads v3
         (copies of the top encoder layers and of the final norm, from tower.safetensors)
         run on the hidden state entering its first copied layer with the encoder's masks
         and positions, then its final-norm copy. The heads v3 token head reads that output;
         its weights stay in heads.json, outside the graph. The document outputs are the
         plain graph's.

The graph is the R0 export recipe: torch.onnx.export (TorchScript exporter, dynamo off),
opset 17, constant folding, banded local attention (64-query blocks against their
192-key neighbourhood; global layers unchanged), traced at one 256-token input with
dynamic batch and sequence axes. Inputs must be padded to a multiple of 64 with
attention_mask zeros, as the scorer does.

The structure of the graph (everything except the initializer float values) depends on
the exporter and model code versions, so the export refuses to run outside PINNED unless
--allow-unpinned is given. The record says qualified=true only when the environment is
the pinned one, the graph's structure digest equals its variant's allow-listed class
digest (CLASS_STRUCTURE, the values in subnet/crates/artifact-check/src/spec.rs), and
every validation output is finite. A structure or finiteness failure also exits 1 after
the record is written. Validation errors against torch are recorded, not gated: they
depend on the weights.

The record is evidence, never an authority. Admission is decided only by the artifact
checker (subnet/crates/artifact-check, `slop_ninja_artifact_check graph FILE`), whose
Rust walk computes the structure digest and refuses everything outside the accepted
ONNX subset. `structure_sha256` below is an independent implementation of the digest
for the record; it refuses the tensor layouts that would make the two disagree (a
repeated data_type, data before data_type) but checks nothing else.

Weights are read from the release files and checked bit for bit against them after
loading. The one exception is the empty human roster's single placeholder row: the
torch heads keep one row, heads.json exports zero rows, and the export writes zeros.
The scorer does not read human_logits.

Every weight that becomes an initializer must have values of its own. torch.onnx.export
merges initializers of the same dtype, shape and strides whose values are equal
(torch.equal, so 0 and -0 count as equal) into one, and every allow-listed class keeps
each weight as its own initializer, so a model with two equal weights exports to a graph
of another structure and length. A span tower tensor left at the encoder tensor it was
copied from (a norm or layer that was frozen or never trained) is the likely case. The
export refuses such a model before tracing and names the equal tensors.

Validation runs ONNX Runtime on synthetic token ids (no text) at several lengths and
compares every output with the unbanded, unpadded torch model. Nothing here reads or
prints document text.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
import platform
import sys
import time
from pathlib import Path

os.environ.setdefault("HF_HUB_OFFLINE", "1")
os.environ.setdefault("TRANSFORMERS_OFFLINE", "1")
os.environ.setdefault("TOKENIZERS_PARALLELISM", "false")

SCHEMA = "slop_ninja_a_onnx_export_v1"
# The environment that produced R0's graph (two independent exports, one digest).
PINNED = {"python": "3.12", "torch": "2.9.1", "transformers": "4.57.6"}
B = 64
TRACE_TOKENS = 256
CLS, SEP = 50281, 50282
CFG_ZEROS = 1820
PLAIN_OUTPUTS = ["origin_logits", "involvement_logit", "conditional_mixed_share", "ai_logits",
                 "human_logits", "h"]
SPAN_OUTPUTS = PLAIN_OUTPUTS + ["token_states"]
TOWER_OUTPUTS = PLAIN_OUTPUTS + ["span_tower_token_states"]
STATE_OUTPUTS = ("token_states", "span_tower_token_states")
# The heads v3 span tower this export builds (export_full_a.tower_declaration; full_a_tower.rs).
TOWER_HEADS_SCHEMA = "slop_ninja_full_a_heads_v3"
TOWER_RULE = "slop_ninja_s2_span_tower_v1: token_head(final_norm_copy(layers_copy[L-K..L-1](h_in(L-K))))"
TOWER_FILE = "tower.safetensors"
TOWER_LAYER_TENSORS = ("attn_norm.weight", "attn.Wqkv.weight", "attn.Wo.weight", "mlp_norm.weight", "mlp.Wi.weight",
                       "mlp.Wo.weight")
# slop_ninja_onnx_structure_v2 digests of the allow-listed graph classes
# (subnet/crates/artifact-check/src/spec.rs GRAPHS and TOWER_GRAPH; a Rust test checks they agree).
CLASS_STRUCTURE = {
    "plain": "b5440ec244c6dc519b961560b9cc69201652ec617c7da90ba0bb36fd4c42a731",
    "span": "3a35507f1233803d5dc5355594d9ec8c0f292d52ac3d6f06df61c259441c2f6a",
    "tower": "0b1feeb0b90d79d77999993958fdbfd0b033a6d1d9c29d6b08605451839e3b9c",
}
VALIDATION_LENGTHS = (64, 256, 300, 512, 1000, 2048)


def sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for b in iter(lambda: f.read(1 << 20), b""):
            h.update(b)
    return h.hexdigest()


def demand(ok, message):
    if not ok:
        raise SystemExit("refused: " + message)


def versions():
    import numpy
    import onnx
    import onnxruntime
    import torch
    import transformers
    return {"python": platform.python_version(), "torch": torch.__version__,
            "transformers": transformers.__version__, "onnx": onnx.__version__,
            "onnxruntime": onnxruntime.__version__, "numpy": numpy.__version__}


def pinned(v):
    base = {k: v[k].split("+")[0] for k in PINNED}
    base["python"] = ".".join(v["python"].split(".")[:2])
    return {k: (base[k], PINNED[k]) for k in PINNED if base[k] != PINNED[k]}


def banded_eager(module, qkv, attention_mask, sliding_window_mask, position_ids, local_attention, bs, dim,
                 output_attentions=False, **_):
    """ModernBERT eager attention with banded local layers (the R0 export function)."""
    import torch
    import torch.nn.functional as F
    from transformers.models.modernbert import modeling_modernbert as mm
    cos, sin = module.rotary_emb(qkv, position_ids=position_ids)
    q, k, v = qkv.transpose(3, 1).unbind(dim=2)
    q, k = mm.apply_rotary_pos_emb(q, k, cos, sin)
    scale = module.head_dim ** -0.5
    if local_attention == (-1, -1):
        w = torch.matmul(q, k.transpose(2, 3)) * scale + attention_mask
        p = F.softmax(w, dim=-1, dtype=torch.float32).to(q.dtype)
        o = torch.matmul(p, v)
    else:
        half = local_attention[0]
        b, h, _, d = q.shape
        neg = torch.finfo(q.dtype).min
        keymask = attention_mask[:, :, :1, :]  # (b,1,1,L) additive, 0 or min
        kp = F.pad(k, (0, 0, B, B))
        vp = F.pad(v, (0, 0, B, B))
        mp = F.pad(keymask, (B, B), value=neg)
        qb = q.reshape(b, h, -1, B, d)
        kb = kp.reshape(b, h, -1, B, d)
        vb = vp.reshape(b, h, -1, B, d)
        mb = mp.reshape(b, 1, -1, 1, B)
        kw = torch.cat((kb[:, :, :-2], kb[:, :, 1:-1], kb[:, :, 2:]), dim=3)
        vw = torch.cat((vb[:, :, :-2], vb[:, :, 1:-1], vb[:, :, 2:]), dim=3)
        mw = torch.cat((mb[:, :, :-2], mb[:, :, 1:-1], mb[:, :, 2:]), dim=4)
        i = torch.arange(B).unsqueeze(1)
        j = torch.arange(3 * B).unsqueeze(0)
        band = torch.where((j - B - i).abs() <= half, 0.0, neg).to(q.dtype)
        s = torch.matmul(qb, kw.transpose(-1, -2)) * scale + band + mw
        p = F.softmax(s, dim=-1, dtype=torch.float32).to(q.dtype)
        o = torch.matmul(p, vw).reshape(b, h, -1, d)
    o = o.transpose(1, 2).contiguous().view(bs, -1, dim)
    return (o,)


STRUCTURE_TAG = b"slop_ninja_onnx_structure_v2\0"


def _varint(buf, i):
    v = shift = 0
    while True:
        b = buf[i]
        i += 1
        v |= (b & 0x7F) << shift
        shift += 7
        if not b & 0x80:
            return v, i


def _fields(buf, start, end):
    """(number, wire, value_start, value_end, varint_value) for each field in [start, end)."""
    i = start
    while i < end:
        tag, i = _varint(buf, i)
        number, wire = tag >> 3, tag & 7
        if wire == 0:
            v, j = _varint(buf, i)
            yield number, wire, i, j, v
            i = j
        elif wire == 2:
            n, j = _varint(buf, i)
            yield number, wire, j, j + n, None
            i = j + n
        elif wire in (1, 5):
            j = i + (8 if wire == 1 else 4)
            yield number, wire, i, j, None
            i = j
        else:
            raise SystemExit("refused: group wire type")
    if i != end:
        raise SystemExit("refused: field crosses its message end")


def _float_payloads(buf, start, end, kind, out):
    """Collects the payload ranges of FLOAT initializer data (float_data 4, raw_data 9).

    Only GraphProto.initializer tensors are elided; Constant node tensors stay hashed.
    The data type in force at each payload decides, as in the Rust walk."""
    if kind == "initializer":
        dtype = None
        for number, wire, a, b, v in _fields(buf, start, end):
            if number == 2 and wire == 0:
                demand(dtype is None, "repeated tensor data_type")
                dtype = v
            elif number in (4, 9) and wire == 2:
                demand(dtype is not None, "tensor data before data_type")
                if dtype == 1:
                    out.append((a, b))
        return
    children = {"model": {7: "graph"}, "graph": {5: "initializer"}}[kind]
    for number, wire, a, b, _ in _fields(buf, start, end):
        if wire == 2 and number in children:
            _float_payloads(buf, a, b, children[number], out)


def structure_sha256(path):
    """slop_ninja_onnx_structure_v2: SHA-256 of the tag and the file without FLOAT initializer payloads."""
    import mmap
    with open(path, "rb") as f, mmap.mmap(f.fileno(), 0, access=mmap.ACCESS_READ) as m:
        buf = memoryview(m)
        ranges = []
        _float_payloads(buf, 0, len(buf), "model", ranges)
        h = hashlib.sha256(STRUCTURE_TAG)
        pos = 0
        for a, b in sorted(ranges):
            h.update(buf[pos:a])
            pos = b
        h.update(buf[pos:])
        del buf
        return h.hexdigest()


def bits_equal(a, b):
    import torch
    if a.shape != b.shape or a.dtype != b.dtype:
        return False
    if a.dtype == torch.float32:
        return bool(torch.equal(a.contiguous().view(torch.int32), b.contiguous().view(torch.int32)))
    return bool(torch.equal(a, b))


def load_release(release, tower=False):
    """The release model as the trainer builds it, with every tensor checked against the files.

    With `tower`, also the span tower heads v3 declares (load_tower); the token head's
    weights stay in heads.json and are not loaded."""
    import torch
    from safetensors.torch import load_file
    from transformers import AutoConfig, AutoModelForSequenceClassification
    here = Path(__file__).resolve().parent
    sys.path.insert(0, str(here))
    import full_a
    from warm_start_a import WarmStartA

    base, model_dir = release / "runtime-base", release / "model"
    heads = json.loads((model_dir / "heads.json").read_bytes())
    contract = json.loads((model_dir / "contract.json").read_bytes())
    demand(heads["schema"] == (TOWER_HEADS_SCHEMA if tower else "slop_ninja_full_a_heads_v1"), "heads schema")
    if tower:
        demand(heads["span_head"] is None and isinstance(heads.get("token_head"), dict) and
               heads["token_head"].get("input") == "span_tower_final_norm_token_states",
               "tower heads: a token head on the span tower's states and no span head")
    demand(contract["schema"] == "slop_ninja_full_a_contract_v2", "contract schema")
    demand(heads["contract_sha256"] == sha256(model_dir / "contract.json"), "heads are bound to another contract")
    demand(heads["share_mode"] in full_a.SHARE_MODES, "share mode")

    config = AutoConfig.from_pretrained(base, local_files_only=True)
    config.reference_compile = False
    encoder = AutoModelForSequenceClassification.from_config(config, attn_implementation="eager",
                                                             torch_dtype=torch.float32)
    demand(encoder.__class__.__name__ == "ModernBertForSequenceClassification" and
           encoder.config.hidden_size == 768 and encoder.config.classifier_pooling == "mean",
           "unsupported encoder architecture")
    weights = load_file(str(model_dir / "encoder.safetensors"))
    encoder.load_state_dict(weights, strict=True)
    encoder.eval().requires_grad_(False)
    sd = encoder.state_dict()
    bad = sorted(k for k in set(sd) | set(weights) if k not in sd or k not in weights or
                 not bits_equal(sd[k].cpu(), weights[k]))
    demand(not bad and all(p.dtype == torch.float32 for p in encoder.parameters()), "encoder tensors differ")

    profiles = {"human": contract["human_profiles"], "ai": contract["ai_profiles"]}
    w = heads["weights"]
    if tower:
        # The token head runs outside the graph, on span_tower_token_states (full_a_tower.rs).
        token = sorted(k for k in w if k.startswith("token_head."))
        demand(token, "tower heads without token head weights")
        w = {k: v for k, v in w.items() if not k.startswith("token_head.")}
    init = {"base_temperature": heads["base_temperature"],
            "weights": {k: w[k] for k in ("origin.weight", "origin.bias")}}
    norm = contract["embedding_normalization"]
    module = WarmStartA(init, norm["mean"], norm["scale"], len(profiles["human"]), len(profiles["ai"]), "text")
    placeholders, compared = [], 0
    with torch.no_grad():
        for name, p in module.named_parameters():
            demand(name in w, "heads.json lacks " + name)
            shape = w[name]["shape"]
            if shape != list(p.shape):
                role = name.split(".")[0]
                demand(role in ("human", "ai") and not profiles[role] and shape[0] == 0,
                       "heads.json shape differs for " + name)
                p.zero_()
                placeholders.append(name)
                continue
            t = torch.tensor(w[name]["values"], dtype=torch.float64).reshape(shape)
            demand(bool(torch.equal(t.float().double(), t)), name + " is not float32-exact")
            p.copy_(t.float())
            compared += 1
        for key in ("text_mean", "text_scale"):
            t = torch.tensor(heads[key], dtype=torch.float64)
            demand(bits_equal(t.float(), getattr(module, key).reshape(t.shape)),
                   key + " differs from the contract normalization")
            compared += 1
    demand(set(w) == {n for n, _ in module.named_parameters()}, "heads.json has unrecognized weights")
    module.eval().requires_grad_(False)
    model = full_a.FullA(encoder, module, None, "head", None).eval()
    model.requires_grad_(False)
    loaded = {"encoder_tensors": len(sd), "head_tensors_compared": compared,
              "zero_placeholders": placeholders, "span_head_in_heads": heads["span_head"] is not None}
    if not tower:
        return model, loaded, None
    span_tower, record = load_tower(encoder, heads["span_tower"], model_dir / TOWER_FILE)
    loaded.update(token_head_tensors_excluded=len(token), **record)
    return model, loaded, span_tower


def load_tower(encoder, declaration, path):
    """The heads v3 span tower: copies of the declared encoder layers and of the final norm, every tensor
    from tower.safetensors and checked bit for bit after loading. The declaration must be the one the
    S2c exporter writes for this encoder (export_full_a.tower_declaration)."""
    import copy
    import torch
    from safetensors.torch import load_file
    from full_a_s2 import SpanTower
    config = encoder.config
    n = config.num_hidden_layers
    d = declaration
    demand(isinstance(d, dict) and d.get("rule") == TOWER_RULE and d.get("encoder_layers") == n, "tower rule")
    source = d.get("source_layer")
    demand(type(source) is int and 1 <= source < n and d.get("layers") == list(range(source, n)), "tower layers")
    kinds = ["global" if i % config.global_attn_every_n_layers == 0 else "local" for i in d["layers"]]
    demand(d.get("attention") == kinds and d.get("global_attn_every_n_layers") == config.global_attn_every_n_layers
           and d.get("local_attention") == config.local_attention, "tower attention differs from the encoder's")
    demand(d.get("capture") == "hidden_state_entering_source_layer" and d.get("masks_and_positions") == "encoder"
           and d.get("final_norm") == "copy" and d.get("weights") == TOWER_FILE and
           d.get("tensor_prefix") == "span_tower.", "tower capture, final norm or weights file")
    layers = [copy.deepcopy(encoder.model.layers[i]) for i in d["layers"]]
    tower = SpanTower(layers, copy.deepcopy(encoder.model.final_norm))
    weights = load_file(str(path))
    prefix = d["tensor_prefix"]
    names = sorted([f"{prefix}layers.{j}.{t}" for j in range(len(layers)) for t in TOWER_LAYER_TENSORS] +
                   [prefix + "norm.weight"])
    demand(sorted(weights) == names, "tower.safetensors tensor roster")
    tower.load_state_dict({k[len(prefix):]: v for k, v in weights.items()}, strict=True)
    tower.eval().requires_grad_(False)
    sd = tower.state_dict()
    demand(sorted(prefix + k for k in sd) == names and
           all(bits_equal(sd[k[len(prefix):]].cpu(), weights[k]) and weights[k].dtype == torch.float32
               for k in names), "tower tensors differ from tower.safetensors")
    for j, i in enumerate(d["layers"]):
        mine, src = tower.layers[j].attn, encoder.model.layers[i].attn
        demand(mine.layer_id == i and tuple(mine.local_attention) == tuple(src.local_attention) and
               torch.equal(mine.rotary_emb.inv_freq, src.rotary_emb.inv_freq), "tower copy differs from its layer")
    return (tower, source), {"tower_tensors": len(names), "tower_parameters": sum(v.numel() for v in weights.values()),
                             "tower_source_layer": source, "tower_layers": d["layers"]}


def graph_weights(model, tower):
    """The tensors the graph's initializers come from, named by release file: the encoder tensors the graph
    reads (FullA reads the encoder's model and head, never its classifier), the document heads' tensors and
    text normalization, and for the tower variant the span tower's tensors."""
    out = [("encoder.safetensors " + k, v) for k, v in model.encoder.state_dict().items()
           if not k.startswith("classifier.")]
    out += [("heads.json " + k, v) for k, v in model.heads.state_dict().items()]
    if tower is not None:
        out += [(f"{TOWER_FILE} span_tower.{k}", v) for k, v in tower[0].state_dict().items()]
    return out


def equal_weights(named):
    """The name pairs of (name, tensor) entries that torch.onnx.export would merge into one initializer:
    its initializer deduplication (the TorchScript exporter in inference mode) merges tensors of the same
    dtype, shape and strides whose values are equal (torch.equal)."""
    import torch
    groups = {}
    for name, t in named:
        groups.setdefault((t.dtype, tuple(t.shape), t.stride()), []).append((name, t))
    pairs = []
    for group in groups.values():
        for i, (a, x) in enumerate(group):
            pairs += [(a, b) for b, y in group[i + 1:] if torch.equal(x, y)]
    return pairs


class Export:
    """The traced module: R0's OnnxA, plus the final-norm token states for the span variant, or the span
    tower's normed states for the tower variant (`tower` = (SpanTower, source layer))."""

    def __init__(self, model, span, tower=None):
        import torch

        class Plain(torch.nn.Module):
            def __init__(self, model):
                super().__init__()
                self.model = model

            def forward(self, input_ids, attention_mask):
                content = attention_mask.bool()
                o = self.model(input_ids, attention_mask, content, autocast=False)
                return (o["origin_logits"], o["involvement_logit"], o["conditional_mixed_share"],
                        o["ai_logits"], o["human_logits"], o["h"])

        class Span(Plain):
            def forward(self, input_ids, attention_mask):
                captured = {}
                encode = self.model.encode

                def capture(ids, mask):
                    hidden, h = encode(ids, mask)
                    captured["hidden"] = hidden
                    return hidden, h

                self.model.encode = capture
                try:
                    out = super().forward(input_ids, attention_mask)
                finally:
                    del self.model.encode
                return out + (captured["hidden"],)

        class Tower(Plain):
            """The plain graph plus the span tower (full_a_s2.TowerFullA's tower path): the hidden state
            entering the source layer and that layer's keyword arguments (the encoder's masks and
            positions) are captured by a forward pre-hook and run through the tower's layer copies and
            final-norm copy."""

            def __init__(self, model, tower, source):
                super().__init__(model)
                self.tower, self.source = tower, source

            def forward(self, input_ids, attention_mask):
                captured = {}

                def capture(_module, args, kwargs):
                    captured["hidden"], captured["kwargs"] = args[0], dict(kwargs)

                hook = self.model.encoder.model.layers[self.source].register_forward_pre_hook(capture,
                                                                                            with_kwargs=True)
                try:
                    out = super().forward(input_ids, attention_mask)
                finally:
                    hook.remove()
                return out + (self.tower(captured["hidden"], captured["kwargs"]),)

        if tower is not None:
            self.module = Tower(model, *tower).eval()
            self.outputs = TOWER_OUTPUTS
        else:
            self.module = (Span if span else Plain)(model).eval()
            self.outputs = SPAN_OUTPUTS if span else PLAIN_OUTPUTS


def synthetic_ids(n):
    """[CLS] + n-2 deterministic in-vocabulary ids + [SEP]; no text."""
    import numpy as np
    body = [1000 + (i * 7919) % 49000 for i in range(n - 2)]
    return np.array([CLS] + body + [SEP], dtype=np.int64)


def padded(x):
    import numpy as np
    n = len(x)
    m = -(-n // B) * B
    ids = np.zeros((1, m), np.int64)
    mask = np.zeros((1, m), np.int64)
    ids[0, :n], mask[0, :n] = x, 1
    return ids, mask


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--release-dir", type=Path, required=True)
    ap.add_argument("--variant", choices=("plain", "span", "tower"), required=True)
    ap.add_argument("--out", type=Path, required=True)
    ap.add_argument("--record", type=Path)
    ap.add_argument("--compare-graph", type=Path, help="report which float tensors differ from this graph")
    ap.add_argument("--allow-unpinned", action="store_true")
    a = ap.parse_args()

    v = versions()
    drift = pinned(v)
    demand(not drift or a.allow_unpinned, f"environment differs from the pinned exporter: {drift}")
    demand(not a.out.exists(), "output exists; exports never overwrite")
    import numpy as np
    import onnxruntime as ort
    import torch
    from transformers.models.modernbert import modeling_modernbert as mm

    torch.set_num_threads(4)
    release = a.release_dir.resolve(strict=True)
    model, loaded, tower = load_release(release, a.variant == "tower")
    weights = graph_weights(model, tower)
    merged = equal_weights(weights)
    demand(not merged, "weights with equal values, which the exporter would merge into one initializer, so the "
           "graph would leave its class: " + "; ".join(f"{x} = {y}" for x, y in merged))
    loaded["distinct_graph_weights"] = len(weights)
    del weights
    wrapper = Export(model, a.variant == "span", tower)

    # References from the unchanged (unbanded, unpadded) model.
    inputs = {n: synthetic_ids(n) for n in VALIDATION_LENGTHS}
    refs = {}
    with torch.no_grad():
        for n, x in inputs.items():
            xi = torch.from_numpy(x[None])
            refs[n] = [t.numpy() for t in wrapper.module(xi, torch.ones_like(xi))]
    mm.MODERNBERT_ATTENTION_FUNCTION["eager"] = banded_eager

    ids, mask = padded(synthetic_ids(TRACE_TOKENS))
    a.out.parent.mkdir(parents=True, exist_ok=True)
    tmp = a.out.with_name(a.out.name + ".exporting")
    t0 = time.time()
    axes = {"input_ids": {0: "batch", 1: "seq"}, "attention_mask": {0: "batch", 1: "seq"},
            **{o: {0: "batch"} for o in PLAIN_OUTPUTS}}
    if a.variant != "plain":
        axes[wrapper.outputs[-1]] = {0: "batch", 1: "seq"}
    with torch.no_grad():
        torch.onnx.export(wrapper.module, (torch.from_numpy(ids), torch.from_numpy(mask)), str(tmp),
                          input_names=["input_ids", "attention_mask"], output_names=wrapper.outputs,
                          opset_version=17, do_constant_folding=True, dynamo=False, dynamic_axes=axes)
    export_s = time.time() - t0
    outputs = wrapper.outputs
    # Free the torch model before ONNX Runtime loads the graph: the peak stays near
    # one model copy plus the session.
    del wrapper, model, tower
    import gc
    gc.collect()

    so = ort.SessionOptions()
    so.intra_op_num_threads = 4
    sess = ort.InferenceSession(str(tmp), so, providers=["CPUExecutionProvider"])
    rows = []
    for n, x in inputs.items():
        pi, pm = padded(x)
        got = sess.run(None, {"input_ids": pi, "attention_mask": pm})
        diffs = {}
        for name, ref, out in zip(outputs, refs[n], got):
            if name in STATE_OUTPUTS:
                out = out[:, :n]
            diffs[name] = float(np.abs(np.float64(ref) - np.float64(out)).max())
        rows.append({"tokens": n, "padded_to": int(pi.shape[1]), "max_abs": diffs})
    del sess
    gc.collect()
    os.replace(tmp, a.out)

    structure = structure_sha256(a.out)
    finite = all(math.isfinite(x) for r in rows for x in r["max_abs"].values())
    failures = []
    if drift:
        failures.append("environment differs from the pinned exporter")
    if structure != CLASS_STRUCTURE[a.variant]:
        failures.append("structure digest is not the allow-listed " + a.variant + " class digest")
    if not finite:
        failures.append("a validation output is not finite")
    record = {"schema": SCHEMA, "variant": a.variant, "outputs": outputs,
              "qualified": not failures, "qualification_failures": failures,
              "environment_drift": drift, "versions": v,
              "script_sha256": sha256(Path(__file__)), "full_a_sha256": sha256(Path(__file__).with_name("full_a.py")),
              "warm_start_a_sha256": sha256(Path(__file__).with_name("warm_start_a.py")),
              "inputs": {p: sha256(release / p) for p in ("runtime-base/config.json", "model/encoder.safetensors",
                                                          "model/heads.json", "model/contract.json") +
                         (("model/" + TOWER_FILE,) if a.variant == "tower" else ())},
              "loaded": loaded, "trace_tokens": TRACE_TOKENS, "block": B, "opset": 17,
              "export_seconds": export_s, "graph_sha256": sha256(a.out), "graph_bytes": a.out.stat().st_size,
              "structure_sha256": structure, "class_structure_sha256": CLASS_STRUCTURE[a.variant],
              "validation": rows,
              "validation_max_abs_h": max(r["max_abs"]["h"] for r in rows)}
    if a.variant == "tower":
        record["full_a_s2_sha256"] = sha256(Path(__file__).with_name("full_a_s2.py"))
    if a.compare_graph:
        record["compare"] = compare(a.out, a.compare_graph)
    text = json.dumps(record, indent=1) + "\n"
    if a.record:
        a.record.write_text(text)
    print(text, end="")
    if structure != CLASS_STRUCTURE[a.variant] or not finite:
        raise SystemExit("not qualified: " + "; ".join(failures))


def compare(path, ref_path):
    """Float tensors of the export that differ from a reference graph, by name (no values)."""
    import numpy as np
    import onnx
    from onnx import numpy_helper

    def floats(p):
        m = onnx.load(str(p), load_external_data=False)
        out = {t.name: t for t in m.graph.initializer}
        for i, n in enumerate(m.graph.node):
            for at in n.attribute:
                if at.type == onnx.AttributeProto.TENSOR:
                    out[f"node{i}:{at.name}"] = at.t
        return out

    a, b = floats(path), floats(ref_path)
    differ = []
    for k in sorted(set(a) | set(b)):
        if k not in a or k not in b:
            differ.append({"tensor": k, "only_in": "export" if k in a else "reference"})
            continue
        x, y = numpy_helper.to_array(a[k]), numpy_helper.to_array(b[k])
        if x.shape != y.shape or x.dtype != y.dtype:
            differ.append({"tensor": k, "shape_or_dtype": True})
        elif x.tobytes() != y.tobytes():
            d = float(np.abs(x.astype(np.float64) - y.astype(np.float64)).max())
            differ.append({"tensor": k, "elements": int(x.size), "max_abs": d if math.isfinite(d) else None})
    return {"reference_sha256": sha256(ref_path), "tensors": len(a), "differing": differ}


if __name__ == "__main__":
    if len(sys.argv) == 3 and sys.argv[1] == "structure":
        print(structure_sha256(sys.argv[2]))
    else:
        main()
