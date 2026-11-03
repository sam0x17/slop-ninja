"""Plot the frozen coordinate-weight experiment with the separate plot environment."""
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
    parser.add_argument("--report", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    if args.out.exists():
        raise ValueError("Choose a new figure directory")
    report = json.loads(args.report.read_text())
    if report["schema"] != "slopninja-coordinate-test-result-v1":
        raise ValueError("Unsupported coordinate report")
    names = sorted(report["models"]["baseline"]["galleries"])
    rows = []
    for index, name in enumerate(names + ["pooled"]):
        metrics = {model: (report["models"][model]["seed_mean"] if name == "pooled"
                           else report["models"][model]["galleries"][name])
                   for model in ["baseline", "coordinate"]}
        rows.append({"gallery": name, "label": "All galleries" if name == "pooled" else f"Gallery {index + 1}",
                     "authors": metrics["baseline"]["author_count"], "queries": metrics["baseline"]["query_count"],
                     "baseline_percent": 100 * metrics["baseline"]["macro_author"]["top1"],
                     "coordinate_percent": 100 * metrics["coordinate"]["macro_author"]["top1"]})
    args.out.mkdir(parents=True)
    with (args.out / "aggregate.csv").open("w", newline="") as handle:
        writer = csv.DictWriter(handle, fieldnames=list(rows[0]))
        writer.writeheader()
        writer.writerows(rows)

    plt.rcParams.update({"font.family": "DejaVu Sans", "font.size": 10,
                         "axes.spines.top": False, "axes.spines.right": False,
                         "svg.fonttype": "none", "pdf.fonttype": 42})
    fig, axes = plt.subplots(1, 2, figsize=(10.5, 4.8), gridspec_kw={"width_ratios": [1.6, 1]})
    baseline, coordinate = "#687383", "#087e83"
    left, right = axes
    y = np.arange(len(rows))
    for index, row in enumerate(rows):
        left.plot([row["baseline_percent"], row["coordinate_percent"]], [index, index], color="#ccd2d8", lw=2)
    left.scatter([row["baseline_percent"] for row in rows], y, color=baseline, s=45, label="Frozen baseline", zorder=3)
    left.scatter([row["coordinate_percent"] for row in rows], y, color=coordinate, s=45, marker="D", label="Learned coordinates", zorder=4)
    left.set_yticks(y, [row["label"] for row in rows])
    left.invert_yaxis()
    left.set_ylim(len(rows) - .5, -.5)
    left.set_xlabel("Macro-author top-1 accuracy (%)")
    left.set_title("Author identification", loc="left", fontweight="bold", pad=14)
    left.grid(axis="x", color="#e5e8eb", zorder=0)
    left.legend(loc="lower left", bbox_to_anchor=(0, -.38), frameon=False)

    primary = report["primary"]
    point = primary["mean_difference"] * 100
    low, high = [value * 100 for value in primary["percentile_95"]]
    right.axvline(0, color=baseline, lw=1, linestyle="--")
    right.plot([low, high], [0, 0], color=coordinate, lw=3)
    right.scatter([point], [0], color=coordinate, marker="D", s=65, zorder=3)
    right.set_ylim(-1, 1)
    right.set_yticks([])
    right.set_xlabel("Accuracy difference (percentage points)")
    right.set_title("Paired primary comparison", loc="left", fontweight="bold", pad=14)
    right.text(.5, .77, f"{point:+.2f} points\n95% interval [{low:+.2f}, {high:+.2f}]",
               transform=right.transAxes, ha="center", va="center")
    right.spines["left"].set_visible(False)
    right.grid(axis="x", color="#e5e8eb", zorder=0)
    span = max(high - low, 1.0)
    right.set_xlim(min(0, low) - span * .15, max(0, high) + span * .15)

    fig.suptitle("slopninja · Individual word and grammar weights", x=.06, ha="left", fontsize=15, fontweight="bold")
    fig.text(.06, .89, f"{report['coordinate_count']:,} learned coordinates · 3 matched baselines · "
             f"{rows[-1]['authors']} fresh authors · {rows[-1]['queries']} test posts · λ = {report['selected_lambda']:g}", color=baseline)
    fig.text(.06, .025, "Seed metrics averaged before author aggregation. Paired 95% interval: 2,000 author resamples within galleries.\n"
             "Validation was reused; the test galleries were new. Author retrieval does not measure rewriting or detector performance.",
             fontsize=8, color=baseline)
    fig.subplots_adjust(left=.15, right=.97, top=.78, bottom=.28, wspace=.42)
    for extension in ["png", "svg", "pdf"]:
        fig.savefig(args.out / f"coordinate-weights.{extension}", dpi=200, facecolor="white")
    plt.close(fig)
    receipt = {"report_sha256": digest(args.report), "plot_source_sha256": digest(Path(__file__)),
               "matplotlib": matplotlib.__version__, "numpy": np.__version__,
               "files_sha256": {path.name: digest(path) for path in sorted(args.out.iterdir()) if path.is_file()}}
    (args.out / "provenance.json").write_text(json.dumps(receipt, indent=2) + "\n")


if __name__ == "__main__":
    main()
