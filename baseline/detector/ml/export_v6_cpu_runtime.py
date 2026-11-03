"""Copy V6 into an explicitly identified Linux CPU runtime variant; never refit it."""

import argparse
import hashlib
import json
from pathlib import Path
import shutil

from common import canonical, sha256, verify_files, write_json


PARENT_ID = "4e061df4789e546dfdd58c8005860a98fb8f200a4c7642b7491461f44f16f138"
PARENT_MANIFEST_SHA256 = "1f4700f2a1855ca9cec5d0b9fbbe830a2a07588a4e95a77a9a754bd6cc3de73f"
CPU_INDEX = "https://download.pytorch.org/whl/cpu"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--parent", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--cpu-kernel-profile", choices=("native", "portable-compatible"),
                        default="native", help="Declare a fixed CPU kernel profile without changing reference tolerances")
    args = parser.parse_args()
    parent = args.parent.resolve(strict=True)
    output = args.output.resolve()
    if output.exists() or output.is_relative_to(parent):
        parser.error("use a new output directory outside the immutable parent")
    if sha256(parent / "manifest.json") != PARENT_MANIFEST_SHA256:
        parser.error("parent manifest is not the frozen V6 release")
    manifest = json.loads((parent / "manifest.json").read_text())
    claimed = manifest.pop("artifact_id")
    if claimed != PARENT_ID or hashlib.sha256(canonical(manifest)).hexdigest() != claimed:
        parser.error("parent identity differs from the frozen V6 release")
    if manifest["reference_runtime"]["packages"]["torch"] != "2.14.0":
        parser.error("unexpected parent PyTorch version")
    verify_files(parent, manifest["files"])
    output.mkdir(parents=True, exist_ok=False)
    for name, expected in manifest["files"].items():
        source = parent / name
        if not source.resolve(strict=True).is_relative_to(parent):
            raise ValueError("parent asset resolves outside its artifact")
        target = output / name
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(source, target)
        if sha256(target) != expected:
            raise ValueError(f"asset changed during copying: {name}")

    requirements = output / "runner/requirements.lock"
    lines = requirements.read_text().splitlines()
    if lines.count("torch==2.14.0") != 1:
        raise ValueError("expected exactly one frozen PyTorch requirement")
    lines[lines.index("torch==2.14.0")] = "torch==2.14.0+cpu"
    requirements.write_text(f"--extra-index-url {CPU_INDEX}\n" + "\n".join(lines) + "\n")
    provenance = {
        "parent_artifact_id": PARENT_ID,
        "parent_manifest_sha256": PARENT_MANIFEST_SHA256,
        "purpose": "Linux CPU runtime packaging; no training or calibration change",
        "changed_packages": {"torch": {"from": "2.14.0", "to": "2.14.0+cpu"}},
        "package_index": CPU_INDEX,
        "preserved": ["classifier", "tokenizer", "calibration", "reference_vectors", "runner_code"],
        "qualification": "Requires separate dependency validation and unchanged-reference replay",
        "limitations": ["Not an independent detector for panel diversity or reward accounting",
                        "Parent scores and cutoffs retain their original artifact identity",
                        "No accuracy, CUDA, confidential-hardware or subnet-qualification claim"],
    }
    extra_files = []
    if args.cpu_kernel_profile == "portable-compatible":
        # Set these before importing torch/common. All original runner files,
        # weights, reference vectors and the 1e-6 check remain unchanged.
        profile = {"MKL_CBWR": "COMPATIBLE", "ATEN_CPU_CAPABILITY": "default",
                   "OMP_NUM_THREADS": "1", "MKL_NUM_THREADS": "1"}
        provenance["fixed_environment"] = profile
        provenance["cpu_kernel_profile"] = "mkl-compatible-aten-default-v1"
        provenance["entrypoint"] = "runner/infer_compatible.py"
        provenance["qualification"] = "Requires unchanged bundled reference replay under the declared CPU profile"
        manifest["reference_runtime"]["fixed_environment"] = profile
        wrapper = ("\"\"\"Pinned CPU instruction profile; unchanged bundled reference checks.\"\"\"\n"
                   "import os\n"
                   f"os.environ.update({profile!r})\n"
                   "from infer import main\n\n"
                   "if __name__ == '__main__':\n    main()\n")
        (output / "runner/infer_compatible.py").write_text(wrapper)
        extra_files.append("runner/infer_compatible.py")
    write_json(output / "RUNTIME_VARIANT.json", provenance)
    manifest["reference_runtime"]["packages"]["torch"] = "2.14.0+cpu"
    for name in ["runner/requirements.lock", "RUNTIME_VARIANT.json", *extra_files]:
        manifest["files"][name] = sha256(output / name)
    manifest["artifact_id"] = hashlib.sha256(canonical(manifest)).hexdigest()
    write_json(output / "manifest.json", manifest)
    print(json.dumps({"artifact_id": manifest["artifact_id"], "parent_artifact_id": PARENT_ID,
                      "manifest_sha256": sha256(output / "manifest.json"),
                      "status": "runtime_variant_awaiting_replay"}))


if __name__ == "__main__":
    main()
