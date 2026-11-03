"""Plot the matched clause-family study from its frozen aggregate report."""
import argparse
import csv
import hashlib
import json
from pathlib import Path

import matplotlib

matplotlib.use("Agg")
import matplotlib.pyplot as plt
import numpy as np


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("protocol", "report", "out"):
        parser.add_argument("--" + name, type=Path, required=True)
    args = parser.parse_args()
    report = json.loads(args.report.read_text())
    protocol = json.loads(args.protocol.read_text())
    if report["schema"] != "slopninja-named-family-calibration-test-result-v1":
        raise ValueError("unexpected result schema")
    if report["protocol_sha256"] != digest(args.protocol):
        raise ValueError("report does not bind the supplied protocol")
    if protocol["schema"] != "slopninja-clause-family-study-protocol-v1" or args.out.exists():
        raise ValueError("expected the frozen study protocol and a new output directory")
    models = report["models"]
    galleries = sorted(models["raw14"]["galleries"])
    if galleries != sorted(models["raw15"]["galleries"]) or len(galleries) != 4:
        raise ValueError("gallery catalogs differ")
    rows = []
    for arm in ("raw14", "raw15"):
        for name in ["pooled"] + galleries:
            result = models[arm]["seed_mean"] if name == "pooled" else models[arm]["galleries"][name]
            rows.append({"arm": arm, "gallery": name, "authors": result["author_count"],
                         "queries": result["query_count"], "top1_percent": 100 * result["macro_author"]["top1"]})
    gain = report["primary"]
    point, (lower, upper) = 100 * gain["mean_difference"], np.array(gain["percentile_95"]) * 100
    if not np.isfinite([point, lower, upper]).all() or lower > upper:
        raise ValueError("invalid paired interval")

    plt.rcParams.update({"font.family": "DejaVu Sans", "font.size": 11,
                         "axes.spines.top": False, "axes.spines.right": False,
                         "svg.fonttype": "none"})
    fig, (left, right) = plt.subplots(1, 2, figsize=(12, 5), gridspec_kw={"width_ratios": [2.4, 1]})
    x = np.arange(5)
    for offset, arm, label, color in ((-0.19, "raw14", "Original 14", "#526b85"),
                                     (0.19, "raw15", "With clause-child family", "#1b8a7a")):
        values = [row["top1_percent"] for row in rows if row["arm"] == arm]
        bars = left.bar(x + offset, values, width=0.36, color=color, label=label)
        left.bar_label(bars, labels=[f"{value:.1f}" for value in values], padding=3, fontsize=9)
    supports = [row["authors"] for row in rows if row["arm"] == "raw14"]
    left.set_xticks(x, [f"{label}\n{count} authors" for label, count in
                       zip(["Pooled", "Gallery 1", "Gallery 2", "Gallery 3", "Gallery 4"], supports)])
    left.set_ylim(0, 100)
    left.set_ylabel("Author-macro top-1 (%)")
    left.set_title("Retrieval on new authors")
    left.legend(loc="upper left", frameon=False, fontsize=10)
    left.grid(axis="y", alpha=0.18)
    left.set_axisbelow(True)

    right.axvline(0, color="#68717d", linewidth=1, linestyle="--")
    right.plot([lower, upper], [0, 0], color="#1b8a7a", linewidth=3)
    right.plot(point, 0, "o", color="#1b8a7a", markersize=8)
    right.text(point, 0.18, f"{point:+.2f} points", ha="center", weight="bold")
    right.text((lower + upper) / 2, -0.2, f"95% interval\n[{lower:+.2f}, {upper:+.2f}]", ha="center")
    right.set_xlim(min(-0.5, lower - 0.5), max(0.5, upper + 0.5))
    right.set_ylim(-0.6, 0.6)
    right.set_yticks([])
    right.set_xlabel("Extended minus original (points)")
    right.set_title("Primary paired comparison")
    right.spines["left"].set_visible(False)
    fig.suptitle("Does a compositional clause family improve author retrieval?", fontsize=15, weight="bold")
    fig.text(0.5, 0.025, "Three-seed mean. Paired interval resamples authors within the four fixed galleries.",
             ha="center", color="#526070", fontsize=10)
    fig.tight_layout(rect=(0, 0.055, 1, 0.93))
    args.out.mkdir(parents=True)
    for extension in ("png", "svg", "pdf"):
        fig.savefig(args.out / f"clause-family-study.{extension}", dpi=180, facecolor="white")
    plt.close(fig)
    with (args.out / "aggregate.csv").open("w", newline="") as stream:
        writer = csv.DictWriter(stream, fieldnames=list(rows[0]))
        writer.writeheader()
        writer.writerows(rows)
    provenance = {"schema": "slopninja-clause-family-figure-v1", "protocol_sha256": digest(args.protocol),
                  "report_sha256": digest(args.report), "source_sha256": digest(Path(__file__)),
                  "matplotlib": matplotlib.__version__, "primary": gain,
                  "artifacts_sha256": {path.name: digest(path) for path in sorted(args.out.iterdir())}}
    (args.out / "provenance.json").write_text(json.dumps(provenance, indent=2, allow_nan=False) + "\n")


if __name__ == "__main__":
    main()
