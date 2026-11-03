# Studio detector training environment

The isolated environment lives under the Studio's experiment directory. The SSH
destination is local configuration; paths below are relative to the remote home:

```text
~/slop_ninja_runs/reference_detector_20260911/detector-training/
  .venv/                         Python 3.12.14, pinned ML and spaCy packages
  ml/                            Snapshot of the detector ML runners and lock
  grammar/                       syntax_bridge.py and requirements-grammar.lock
  model-cache/modernbert-base/    Pinned upstream weights and tokenizer
  logs/setup.log                 Detached environment setup log
  setup_environment.py           Exact setup procedure
  setup-status.json              Command outcomes and completion status
  verify_environment.py          Synthetic CPU and parser checks
  verification.json              Results and copied-file content hashes
  installed-requirements.lock    Actual installed package versions
  runs/                          Reserved for later detector runs
```

This directory is separate from the generator runtime. Setup does not load or
unload an LM Studio model and does not run detector training. The copied encoder
checkpoint is `answerdotai/ModernBERT-base` revision
`8949b909ec900327062f0ebf497f51aef5e6f0c8`, with weight SHA-256
`340ac08b74eef0d7bdec2d7981a6a3d4249bf0e6aab60634b72ad02c2b8023a9`.

For a fresh directory containing those copied assets, setup uses the existing
Homebrew Python 3.12.14 and uv 0.12.9:

```sh
/opt/homebrew/bin/uv venv --python /opt/homebrew/bin/python3.12 .venv
UV_LINK_MODE=copy /opt/homebrew/bin/uv pip install --python .venv/bin/python \
  -r ml/requirements.lock -r grammar/requirements-grammar.lock
/opt/homebrew/bin/uv pip check --python .venv/bin/python
.venv/bin/python verify_environment.py
```

The locks agree on shared packages, so one environment can run both the ML
dependency and spaCy annotation bridge. Rust still owns corpus handling and
feature extraction. The bridge is copied unchanged from
`grammar/crates/grammar-spacy/python/syntax_bridge.py`.

Verification loads the checkpoint offline, initializes the three-class head
with seed 17, and runs two synthetic strings on one CPU thread. It checks MPS
availability without executing a GPU workload. The parser check preserves the
exact synthetic strings and compares the installed model identity with the
local installation:

```text
spacy=3.8.16;model=en_core_web_sm@3.8.0
model-files-sha256=c99c4f7c306a310c603f9bbfb32d299e2b1199c964efe108253536432c98b68f
bridge-sha256=4ed4d4c9b6d64de2ae5f704e4433c083124cba4b1e47c3456315bbd9c6b0a9b0
```

Setup and verification completed on 2026-09-11, macOS 26.5.2 arm64. All 58
installed packages passed the dependency check. The two CPU forwards took
0.0944 seconds in total, with 12 and 11 tokens respectively. MPS was built and
available; the parser and bridge hashes matched the local installation exactly.
The isolated environment uses 0.80 GiB and the checkpoint cache 0.57 GiB; about
53 GiB of disk space remained after setup.

The setup process runs in its own session with stdin closed and stdout/stderr
written to the remote log, so it survives an SSH disconnect. Later training
must use the same arrangement or a remote supervisor; attaching a laptop
terminal is insufficient for a durable run. No corpus files or real training
jobs are included in this setup.

The checkpoint occupies about 600 MB. Each retained detector bundle adds about
600 MB, plus a temporary selected checkpoint during training. The earlier local
MPS check used 11.04 GiB with four 1,024-token sequences. Memory use at the
pilot's 2,048-token cap has not been measured; reserve 32 GiB for the first run
and record its actual allocation. The Studio has 256 GiB of unified memory.
