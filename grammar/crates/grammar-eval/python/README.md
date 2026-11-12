# grammar-eval Python

This directory holds the Python that an ML dependency requires: the *A*
training, export, final-block, span and Tier C recipes, their harnesses and
their tests. Rust supplies corpus preparation, features, evaluation and the
artifact checks.

The earlier author-evidence trainers (the coordinate, model-size frontier and
author-projection chains behind `docs/author-projection.md` and
`docs/global-voices-transfer.md`) and their run notes were removed. Their
source remains in Git history, for example at commit `7f1d68db`.

Run the tests from the repository root, in an environment built from
`public_a/requirements-linux-x86_64-cu128.lock` (see the
[training guide](../../../../docs/testnet/training-guide.md)):

```sh
.venv/bin/python -m unittest discover \
  -s grammar/crates/grammar-eval/python -p 'test_*.py' -v
```

## Public A recipe

`public_a.py` and the `public_a/` configuration train an *A* artifact from
ModernBERT-base and published data only; see the
[training guide](../../../../docs/testnet/training-guide.md). It runs
`train_full_a.py`, `export_full_a.py` and `export_a_onnx.py` with a loader
for the public parent in place of the V6 parent's, and writes the artifact
spec's schemas. `test_public_a.py` checks its pins against the artifact
checker's sources.

## Document-model training options

`train_full_a.py` takes three document-model options (`full_a_doc.py`). Each
is off by default, and with all of them off the training computation is
unchanged:

- `share_supervision.bin_balance` in an arm reweights the share terms of mixed
  rows so that each populated share bin carries an equal part of the mixed
  share mass, within a clip ratio;
- `attribution.unsupervised_cohort_ids` in the protocol leaves the listed
  cohorts' rows without attribution supervision and outside the contrastive
  structure, while their AI words still count toward share and involvement;
- a `distill` block in an arm, with `--distill-targets FILE`, mixes each
  document term with a soft term against teacher-mean targets (alpha,
  temperature); alpha 0 trains bit for bit as no distillation.

`raw_full_a.py` writes a fit's FP32 raw heads for its Train block (`train`) or
for a rows file (`rows`). The Rust example `seed_mean_full_a` in
`grammar-a-baseline` averages several fits' Train raw heads into a targets
file (`distill-targets`) and several fits' mining score files into one file of
the same schema (`mining-mean`). `bitwise_full_a.py compare` with
`--candidate-config` and `--candidate-distill-targets` checks a distillation
config at alpha 0 against the same config without it.

## Public reference cases

`reference_cases/a-public-reference-cases-v1.jsonl` holds 200 synthetic
reference cases written for SlopNinja's public reference packages. Every
place, person, organisation and event in them is invented; none is corpus or
evaluation text. They cover seven registers (encyclopedic, news, scientific
abstract, literary prose, regulatory notice, blog, forum and dialogue) as
plain drafts, deliberately model-like prose and mixtures of the two, with
lists, headings, tables, quotations, code and short passages in other
languages, from one sentence to the 2,048-token limit. Each id names how its
text was written (`plain`, `model` or `mixed`).
`export_full_a.py export --synthetic-cases FILE` appends such a file's cases
after the built-in synthetic cases in `reference-requests.jsonl`, so the
export's reference predictions cover them too.
