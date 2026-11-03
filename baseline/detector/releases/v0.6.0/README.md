# Slop Ninja revision detector experiment v0.6.0

This experimental ModernBERT classifier estimates three English document-origin
classes: `human_only`, `model_only`, and `mixed`. It accepts at most 2,048 tokens,
including special tokens. These probabilities describe document-origin classes;
they do not estimate the fraction of words written by AI. The artifact remains
an unqualified research candidate.

Read [RESULTS.md](RESULTS.md) for the complete comparison with v0.5.0, including
the original drafts, both revision stages, the Phi-4 transfer route, the separate
human-root diagnostic and the fresh Pangram comparison. Preserve the reported
false positives, sensitivity tradeoffs and source-family denominators when
interpreting the measurements.

The selected artifact is
`4e061df4789e546dfdd58c8005860a98fb8f200a4c7642b7491461f44f16f138`.
The manifest SHA256 is
`1f4700f2a1855ca9cec5d0b9fbbe830a2a07588a4e95a77a9a754bd6cc3de73f`.
Selection used uncalibrated human/model Development log loss across two
learning rates and four epochs per rate. The selected checkpoint came from
epoch 2 at learning rate 0.00001. Calibration fitted its temperature separately;
the Test outcomes and Pangram observations did not select the checkpoint.

## Run locally

These commands use the original macOS runtime profile. Linux CPU users should
follow the [runtime-variant instructions](../../../../docs/platform-support.md#v6-linux-cpu-runtime).
That variant preserves the trained weights and calibration while declaring its
CPU wheel separately; it has its own artifact identity and replay evidence.

After extracting the release bundle (not part of this snapshot), install the pinned Python 3.12 dependencies
into a virtual environment outside the immutable `encoder/` directory:

```sh
python3.12 -m venv .venv
.venv/bin/python -m pip install -r encoder/runner/requirements.lock
HF_HUB_OFFLINE=1 TRANSFORMERS_OFFLINE=1 PYTHONDONTWRITEBYTECODE=1 \
  .venv/bin/python encoder/runner/infer.py --artifact encoder < requests.jsonl
```

Each input line accepts only `id` and `text`, for example:

```json
{"id":"example","text":"The editor asked for a shorter introduction."}
```

The runner verifies artifact hashes, pinned dependencies and the two bundled
synthetic reference examples before reading requests. It returns probabilities
in the order recorded in `labels`, plus `model_or_assistance_probability`, which
is `P(model_only) + P(mixed)`. Empty or overlength text returns
`unsupported_input`. Inference needs no provider API or checkpoint download.

The verified reference uses CPU FP32, eager attention, one request at a time and
FP64 probability postprocessing. Its pinned packages include PyTorch 2.14.0 and
Transformers 4.57.6. Preserve the bundled numerical checks when using another
host. The selected artifact's successful replay is recorded in
`provenance/runtime-verification.json`.

`operating-points.json` contains the frozen Calibration cutoffs for the combined
probability. Flag only when the score is strictly greater than the cutoff:

| Calibration target | Cutoff | Calibration human false positives |
| --- | ---: | ---: |
| 1% | 0.8436493155285434 | 1/108 |
| 5% | 0.4287369114260753 | 5/108 |

These settings target empirical Calibration rates. Actual Test rates and their
uncertainty appear in RESULTS.md. Production history can be ambiguous from text
alone, and the model has no fitted adjustment for deployment class prevalence.
Origin scores do not measure semantic fidelity, intended tone or author imitation.

## Contents and terms

`encoder/` preserves every manifest-bound byte of the selected model, including
`MODEL_CARD.json`, training and calibration metadata, tokenizer, weights, runtime
and data-rights manifest. `results/` contains the complete aggregate evaluation,
paired comparisons and Pangram confirmation. `provenance/` binds the source
commit, source credits, protocols, runtime verification and prompt notices.

Source credits cover 4,903 Train, Development and Calibration records from 685
source works, including their ancestry. These are attribution counts; the exact
fitting and selection views are recorded in `encoder/training.json`. The
[published dataset catalog](../../../../datasets)
describes available corpus snapshots. Individual provider reports and invocation
logs remain outside this bundle.

Fine-tuned weights use CC BY-SA 4.0. Read `encoder/LICENSE`,
`encoder/DATA_RIGHTS.json`, `provenance/source-credits.json` and `NOTICE.md`.
The upstream encoder retains Apache 2.0 terms in `encoder/UPSTREAM_LICENSE`.
Original runtime code uses the MIT license in `LICENSE.code`. Verify extracted
files with `shasum -a 256 -c SHA256SUMS`.
