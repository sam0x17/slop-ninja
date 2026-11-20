"""Synthetic CPU checks for frozen final-step span fits. No model downloads or corpus reads."""
import copy
from contextlib import redirect_stderr, redirect_stdout
import io
import json
from pathlib import Path
from types import SimpleNamespace
import unittest
from unittest import mock

import numpy as np
import torch

import full_a_bundle as bundle
import full_a_s2 as s2
import raw_full_a
import teacher_a
import test_full_a_r2 as r2
import test_full_a_teacher as fixtures
import train_full_a as trainer

FORBIDDEN = ("dev_iter_involvement_brier", "dev_iter_joint_loss", "dev_iter_diagnostics",
             "selection_value", "mixed_word_brier")


def span_protocol(teacher=True, tower=False):
    p = fixtures.teacher_protocol() if teacher else r2.row_protocol()
    arm = copy.deepcopy(p["arms"]["E1"])
    arm.update(token_head={"hidden": 8, "loss_weight": 1., "learning_rate": .001},
               freeze="encoder_and_document_heads", token_weighting="row_weight")
    if tower:
        arm["s2"] = {"mechanism": "span_tower", "layers": 1, "learning_rate": .001}
    p["arms"] = {"S": arm}
    p["span_target_floor"] = {"policy": "slop_ninja_span_target_floor24_v1", "min_words": 24}
    return p


class FrozenTeacher(fixtures.TinyClass):
    def test_token_then_tower_preserve_documents_and_do_not_print_metrics(self):
        root, config, rows = fixtures.write_teacher_bundle(self.d)
        init = self.d / "init"
        fixtures.run_fit(root, config, init, self.snapshot)
        previous = init
        for tower in (False, True):
            name = "tower" if tower else "token"
            conf, out = self.d / (name + "-config"), self.d / name
            conf.mkdir()
            (conf / "contract.json").write_bytes((config / "contract.json").read_bytes())
            (conf / "protocol.json").write_text(json.dumps(span_protocol(tower=tower)))
            log = io.StringIO()
            with redirect_stdout(log), redirect_stderr(log), mock.patch.object(trainer, "evaluate", wraps=trainer.evaluate) as evaluate:
                fixtures.run_fit(root, conf, out, self.snapshot, arm="S", init_fit=str(previous))
            self.assertEqual(evaluate.call_count, 1, "only the final checkpoint may read metrics")
            for key in FORBIDDEN:
                self.assertNotIn(key, log.getvalue())
                for file in ("init-raw-gate.json", "init-token-gate.json"):
                    self.assertNotIn(key, (out / file).read_text())
            self.assertEqual((init / "dev_iter-raw.jsonl").read_bytes(), (out / "dev_iter-raw.jsonl").read_bytes())
            self.assertEqual(json.loads((out / "init-raw-gate.json").read_text())["document_raw_max_abs_error"], 0.)
            self.assertEqual(len(json.loads((out / "history.json").read_text())), 1)
            saved = torch.load(out / "selected.pt", map_location="cpu", weights_only=False)["model"]
            reference = torch.load(init / "selected.pt", map_location="cpu", weights_only=False)["model"]
            for part in ("encoder", "heads"):
                for key, value in reference[part].items():
                    self.assertTrue(torch.equal(value, saved[part][key]), (part, key))
            # Restore through the real teacher-aware scorer and compare token
            # logits as well as documents with the trainer's retained outputs.
            row_file = self.d / (name + "-rows.jsonl")
            row_file.write_text("".join(json.dumps(r) + "\n" for r in rows if r["split"] == "dev_iter"))
            raw = self.d / (name + "-raw")
            args = SimpleNamespace(data=str(root), config=str(conf), fit=str(out), artifact=str(self.snapshot),
                                   init=None, device="cpu", out=str(raw), rows=str(row_file), split="dev_iter",
                                   token_logits=True)
            raw_full_a.rows(args)
            self.assertEqual((raw / "raw.jsonl").read_bytes(), (out / "dev_iter-raw.jsonl").read_bytes())
            a = [json.loads(s) for s in (raw / "token-logits.jsonl").read_text().splitlines()]
            b = [json.loads(s) for s in (out / "dev_iter-token-logits.jsonl").read_text().splitlines()]
            self.assertEqual([(r["id"],r["token_logits"]) for r in a], [(r["id"],r["token_logits"]) for r in b])
            offsets = [json.loads(s) for s in (raw / "token-offsets.jsonl").read_text().splitlines()]
            for r in offsets:
                self.assertEqual(r["tokens"], len(r["token_offsets"]))
                self.assertFalse(r["labels_included"])
                self.assertNotIn("token_labels", r)
            previous = out

    def test_final_span_resume_matches_uninterrupted_fit(self):
        root, config, _ = fixtures.write_teacher_bundle(self.d)
        init = self.d / "init"
        fixtures.run_fit(root, config, init, self.snapshot)
        conf = self.d / "span-config"
        conf.mkdir()
        (conf / "contract.json").write_bytes((config / "contract.json").read_bytes())
        (conf / "protocol.json").write_text(json.dumps(span_protocol(tower=True)))
        whole, resumed = self.d / "whole", self.d / "resumed"
        fixtures.run_fit(root, conf, whole, self.snapshot, arm="S", init_fit=str(init))
        real_save, calls = trainer.atomic_save, []
        def interrupt(path, value):
            real_save(path, value)
            if path.name == "recovery.pt":
                calls.append(None)
                if len(calls) == 2:
                    raise KeyboardInterrupt("synthetic interruption after durable checkpoint")
        with mock.patch.object(trainer, "atomic_save", interrupt), self.assertRaises(KeyboardInterrupt):
            fixtures.run_fit(root, conf, resumed, self.snapshot, arm="S", init_fit=str(init))
        with mock.patch.object(trainer, "identity_outputs", side_effect=AssertionError("gate must not replay")):
            fixtures.run_fit(root, conf, resumed, self.snapshot, arm="S", init_fit=str(init))
        for file in ("encoder.safetensors", "heads.safetensors", "dev_iter-raw.jsonl", "dev_iter-token-logits.jsonl"):
            self.assertEqual((whole / file).read_bytes(), (resumed / file).read_bytes(), file)

    def test_trained_tower_warm_start_restores_weights_and_refuses_discard(self):
        root, config, _ = fixtures.write_teacher_bundle(self.d)
        init = self.d / "init"
        fixtures.run_fit(root, config, init, self.snapshot)
        conf = self.d / "tower-config"
        conf.mkdir()
        (conf / "contract.json").write_bytes((config / "contract.json").read_bytes())
        protocol = span_protocol(tower=True)
        (conf / "protocol.json").write_text(json.dumps(protocol))
        trained = self.d / "trained-tower"
        fixtures.run_fit(root, conf, trained, self.snapshot, arm="S", init_fit=str(init))
        saved = torch.load(trained / "selected.pt", map_location="cpu", weights_only=False)["model"]
        original_gate = trainer.identity_outputs
        calls = []
        def check_restored(model, *args):
            for part in ("span_tower", "token_head"):
                for key, value in getattr(model, part).state_dict().items():
                    self.assertTrue(torch.equal(value.cpu(), saved[part][key]), (part, key))
            calls.append(True)
            return original_gate(model, *args)
        out = self.d / "continued"
        with mock.patch.object(trainer, "identity_outputs", side_effect=check_restored):
            fixtures.run_fit(root, conf, out, self.snapshot, arm="S", init_fit=str(trained))
        self.assertEqual(calls, [True])
        self.assertEqual(json.loads((out / "init-token-gate.json").read_text())["token_logit_max_abs_error"], 0.)
        protocol["arms"]["S"]["s2"]["layers"] = 2
        (conf / "protocol.json").write_text(json.dumps(protocol))
        with self.assertRaisesRegex(ValueError, "tower depth"):
            fixtures.run_fit(root, conf, self.d / "wrong-depth", self.snapshot, arm="S", init_fit=str(trained))
        (conf / "protocol.json").write_text(json.dumps(span_protocol()))
        with self.assertRaisesRegex(ValueError, "must preserve it"):
            fixtures.run_fit(root, conf, self.d / "discarded", self.snapshot, arm="S", init_fit=str(trained))
        with (trained / "dev_iter-token-logits.jsonl").open("ab") as f:
            f.write(b"\n")
        with self.assertRaisesRegex(ValueError, "token output changed"):
            fixtures.run_fit(root, conf, self.d / "tampered", self.snapshot, arm="S", init_fit=str(trained))

    def test_base_frozen_final_step_uses_the_same_output_only_gate(self):
        import tempfile
        import synthetic_full_a as synthetic
        with tempfile.TemporaryDirectory() as d:
            d = Path(d)
            rows = synthetic.factorial_rows(groups=2)
            root = d / "bundle"
            r2.write_trainer_bundle(root, rows)
            plain, span = d / "plain-config", d / "span-config"
            r2.write_config(plain, root, rows)
            r2.write_config(span, root, rows, span_protocol(teacher=False))
            r2.fit(root, plain, d / "init", final_step=True)
            with mock.patch.object(trainer, "evaluate", wraps=trainer.evaluate) as evaluate:
                r2.fit(root, span, d / "span", arm="S", final_step=True, init_fit=str(d / "init"))
            self.assertEqual(evaluate.call_count, 1)
            self.assertEqual((d / "init/dev_iter-raw.jsonl").read_bytes(), (d / "span/dev_iter-raw.jsonl").read_bytes())
            with self.assertRaisesRegex(ValueError, "requires a document-only starting checkpoint"):
                r2.fit(root, plain, d / "discard-token", final_step=True,
                       init_fit=str(d / "span"))

            protocol = span_protocol(teacher=False)
            protocol["arms"]["S"]["freeze"] = "none"
            trainable = d / "trainable-span-config"
            r2.write_config(trainable, root, rows, protocol)
            with self.assertRaisesRegex(ValueError, "final-step span initialization requires frozen"):
                r2.fit(root, trainable, d / "trainable-span", arm="S", final_step=True, init_fit=str(d / "init"))


class InputOnly(unittest.TestCase):
    def test_document_protocol_with_legacy_bound_reader_needs_no_floor_api(self):
        legacy = SimpleNamespace(**{k:v for k,v in vars(bundle).items() if k != "validate_span_target_floor"})
        with mock.patch.object(trainer, "bundle", legacy):
            trainer.validate_protocol(r2.row_protocol())
            protocol = r2.row_protocol()
            protocol["span_target_floor"] = None
            trainer.validate_protocol(protocol)

    def test_token_identity_rejects_changed_text_nonfinite_and_duplicate_rows(self):
        import tempfile
        row = {"id":"synthetic", "text_sha256":"a"*64,"token_logits":[1.,2.]}
        with tempfile.TemporaryDirectory() as d:
            path = Path(d)
            (path / "dev_iter-token-logits.jsonl").write_text(json.dumps(row)+"\n")
            for change in ({"token_logits":[1.]},{"token_logits":[float("nan"),2.]},{"text_sha256":"b"*64}):
                with self.subTest(change=change), self.assertRaises(ValueError):
                    trainer.init_token_gate([dict(row,**change)],path)
            with self.assertRaisesRegex(ValueError,"duplicate"):
                trainer.init_token_gate([row,row],path)

    def test_identity_gate_refuses_nonfinite_truncated_and_duplicate_outputs(self):
        import tempfile
        row = {"id":"synthetic", "text_sha256":"a"*64, "origin_logits":[0.,1.,2.],
               "human_logits":[0.], "ai_logits":[0.,1.], "involvement_logit":1.,
               "conditional_mixed_share":.5, "uncalibrated_share":.7}
        with tempfile.TemporaryDirectory() as d:
            path = Path(d)
            (path / "dev_iter-raw.jsonl").write_text(json.dumps(row)+"\n")
            for change in ({"ai_logits":[0.]}, {"origin_logits":[0.,float("nan"),2.]},
                           {"uncalibrated_share":float("inf")}):
                with self.subTest(change=change), self.assertRaises(ValueError):
                    trainer.init_raw_gate([dict(row,**change)],path)
            with self.assertRaisesRegex(ValueError,"duplicate"):
                trainer.init_raw_gate([row,row],path,subset=True)

    def test_identity_path_has_no_label_or_metric_access(self):
        # Only input arrays and two identity fields exist. Accessing labels,
        # targets, weights, denominators, word labels or batch would fail.
        pop = SimpleNamespace(meta=[SimpleNamespace(id="synthetic",text_sha256="a"*64)],
                              starts=np.array([0,3]), ids=np.array([1,2,3]),
                              flags=np.array([0,bundle.FLAG_CONTENT | bundle.FLAG_WORD,0]))
        out = {"origin":torch.tensor([[.8,.1,.1]]),"origin_logits":torch.zeros(1,3),
               "human_logits":torch.zeros(1,1),"ai_logits":torch.zeros(1,2),
               "involvement_logit":torch.zeros(1),"conditional_mixed_share":torch.ones(1)*.5,
               "token_mean":None,"pooled":None,"share":torch.ones(1)*.15,"span_logits":torch.zeros(1,3)}
        model = mock.Mock(return_value=out)
        raw, tokens = trainer.identity_outputs(model,pop,torch.device("cpu"))
        self.assertEqual(raw[0]["id"],"synthetic")
        self.assertEqual(tokens[0]["token_logits"],[0.,0.,0.])
        self.assertEqual(model.call_args.kwargs["autocast"],False)

    def test_last_four_layers_keep_global_local_pattern_for_both_depths(self):
        from transformers import ModernBertConfig, ModernBertForSequenceClassification
        import full_a
        for depth in (22,28):
            c = ModernBertConfig(vocab_size=32,hidden_size=16,intermediate_size=16,num_hidden_layers=depth,
                                num_attention_heads=2,global_attn_every_n_layers=3,local_attention=8,
                                pad_token_id=0,cls_token_id=1,sep_token_id=2,bos_token_id=1,eos_token_id=2,
                                reference_compile=False)
            encoder = ModernBertForSequenceClassification(c)
            norm = {"mean":[0.]*16,"scale":[1.]*16}
            heads = teacher_a.fresh_heads(16,norm,0,2,17)
            head = full_a.TokenHead(hidden=16,width=8)
            model = s2.TowerFullA(encoder,heads,token_head=head,tower_layers=4)
            model.attach_tower()
            teacher_a.check_tower(model)
            self.assertEqual([l.attn.layer_id for l in model.span_tower.layers],list(range(depth-4,depth)))
            model.span_tower.layers[0].attn.layer_id = 0
            with self.assertRaisesRegex(ValueError,"geometry differs"):
                teacher_a.check_tower(model)


if __name__ == "__main__":
    torch.set_num_threads(1)
    unittest.main()
