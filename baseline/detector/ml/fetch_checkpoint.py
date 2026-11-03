"""Explicit network-only preparation for the pinned public encoder checkpoint."""

import argparse
import hashlib
import json
from pathlib import Path
import urllib.request

from huggingface_hub import snapshot_download
from checkpoints import (CHECKPOINTS, DEFAULT_MODEL_ID, MODEL_FILES,
                         checkpoint_spec, validate_config)

MODEL_ID = DEFAULT_MODEL_ID
REVISION = CHECKPOINTS[MODEL_ID]["revision"]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--model", choices=sorted(CHECKPOINTS), default=DEFAULT_MODEL_ID)
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()
    if (args.output / "checkpoint.json").exists():
        parser.error("checkpoint already finalized; use a new output directory")
    spec = CHECKPOINTS[args.model]
    snapshot_download(args.model, revision=spec["revision"], local_dir=args.output,
                      allow_patterns=[*MODEL_FILES, "README.md", "LICENSE"])
    for name in (*MODEL_FILES, "README.md"):
        if not (args.output / name).is_file():
            raise FileNotFoundError(f"checkpoint download missing {name}")
    validate_config(json.loads((args.output / "config.json").read_text()), spec)
    (args.output / "UPSTREAM_README.md").write_bytes((args.output / "README.md").read_bytes())
    # The model card declares Apache-2.0 but this revision has no separate license file.
    license_path = args.output / "LICENSE"
    if not license_path.exists():
        with urllib.request.urlopen("https://www.apache.org/licenses/LICENSE-2.0.txt", timeout=30) as response:
            license_path.write_bytes(response.read())
    files = {p.name: hashlib.sha256(p.read_bytes()).hexdigest() for p in sorted(args.output.iterdir())
             if p.is_file() and p.name != "checkpoint.json"}
    pin = {"model_id": args.model, "revision": spec["revision"],
           "weight_license": spec["weight_license"], "files": files}
    checkpoint_spec(pin)
    (args.output / "checkpoint.json").write_text(json.dumps(pin, indent=2, sort_keys=True) + "\n")
    print(json.dumps({"checkpoint": str(args.output), **pin}))


if __name__ == "__main__":
    main()
