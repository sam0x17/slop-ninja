"""Render a frozen training-author scale report in the pinned plot environment."""
import argparse
import csv
import hashlib
import json
from pathlib import Path

import matplotlib
matplotlib.use("Agg")
import matplotlib.pyplot as plt


def read(path):
    return json.loads(path.read_text())


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def save_csv(path, rows):
    with path.open("w", newline="") as stream:
        writer = csv.DictWriter(stream, fieldnames=list(rows[0]))
        writer.writeheader()
        writer.writerows(rows)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--selection", type=Path, required=True)
    parser.add_argument("--report", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    selection, report = read(args.selection), read(args.report)
    if selection["schema"] != "slopninja-frozen-training-author-scale-v1" or report["schema"] != "slopninja-training-author-scale-test-result-v1":
        raise ValueError("Unsupported training-author scale artifacts")
    if report["selection_sha256"] != digest(args.selection):
        raise ValueError("Report does not bind the frozen selection")
    if args.out.exists():
        raise ValueError("Choose a fresh figure directory")
    args.out.mkdir(parents=True)
    rows = []
    for name, model in report["models"].items():
        result = model["seed_mean"]
        rows.append({"model": name, "authors": result["author_count"], "queries": result["query_count"],
                     "coordinate_count": model["coordinate_count"], "top1_percent": result["macro_author"]["top1"] * 100,
                     "top5_percent": result["macro_author"]["top5"] * 100, "mrr": result["macro_author"]["mrr"],
                     "seed_min_top1_percent": model["seed_macro_top1_range"][0] * 100,
                     "seed_max_top1_percent": model["seed_macro_top1_range"][1] * 100})
    save_csv(args.out / "aggregate.csv", rows)
    plt.rcParams.update({"font.family": "DejaVu Sans", "font.size": 10,
                         "axes.spines.top": False, "axes.spines.right": False,
                         "svg.fonttype": "none", "pdf.fonttype": 42})
    fig, axes = plt.subplots(1, 2, figsize=(13, 5.5), gridspec_kw={"width_ratios": [1.2, 1]})
    galleries = sorted(report["models"]["control_100"]["galleries"])
    gallery_rows = []
    for name, label, color, style in [("control_100", "100 training authors", "#007fa3", "o-"),
                                     ("treatment_300", "300 training authors", "#23815a", "o-"),
                                     ("incumbent", "1,864-weight incumbent", "#74808b", "x--")]:
        model = report["models"][name]
        values = [model["galleries"][key]["macro_author"]["top1"] * 100 for key in galleries]
        values.append(model["seed_mean"]["macro_author"]["top1"] * 100)
        axes[0].plot(range(1, 8), values, style, color=color, lw=1.5, ms=5, label=label)
        for index, key in enumerate(galleries):
            gallery_rows.append({"model": name, "gallery": key,
                                 "authors": model["galleries"][key]["author_count"],
                                 "queries": model["galleries"][key]["query_count"], "top1_percent": values[index]})
    save_csv(args.out / "galleries.csv", gallery_rows)
    axes[0].axvline(6.5, color="#d6dce1", lw=.8)
    axes[0].set_xticks(range(1, 8), [str(i) for i in range(1, 7)] + ["Pooled"])
    axes[0].set_xlabel("Fresh test gallery")
    axes[0].set_ylabel("Macro-author top-1 accuracy (%)")
    axes[0].set_title("Same coordinates, more training authors", loc="left", fontweight="bold", pad=14)
    axes[0].grid(axis="y", color="#e5e8eb", lw=.7)
    axes[0].legend(loc="upper left", bbox_to_anchor=(0, -.21), frameon=False, fontsize=9)
    comparisons = [("treatment_minus_control", "300 minus 100 authors"),
                   ("treatment_minus_incumbent", "300 minus incumbent"),
                   ("control_minus_incumbent", "100 minus incumbent"),
                   ("treatment_minus_family_baseline", "300 minus family baseline"),
                   ("treatment_minus_word_eta_only", "Grammar corrections (conditional)"),
                   ("treatment_minus_grammar_eta_only", "Word corrections (conditional)")]
    differences = []
    for index, (key, label) in enumerate(comparisons):
        result = report["comparisons"][key]["metrics"]["top1"]
        point = result["mean_difference"] * 100
        low, high = [value * 100 for value in result["percentile_95"]]
        color = "#23815a" if index == 0 else "#74808b"
        axes[1].plot([low, high], [index, index], color=color, lw=2)
        axes[1].scatter([point], [index], marker="D", s=32, color=color, zorder=3)
        differences.append({"comparison": key, "difference_points": point,
                            "lower_95_points": low, "upper_95_points": high})
    save_csv(args.out / "comparisons.csv", differences)
    axes[1].set_yticks(range(len(comparisons)), [label for _, label in comparisons])
    axes[1].set_ylim(len(comparisons) - .5, -.5)
    axes[1].axvline(0, color="#74808b", ls="--", lw=1)
    axes[1].set_xlabel("Accuracy difference (percentage points)")
    axes[1].set_title("Paired differences and 95% intervals", loc="left", fontweight="bold", pad=14)
    axes[1].grid(axis="x", color="#e5e8eb", lw=.7)
    treatment = report["models"]["treatment_300"]["seed_mean"]
    fig.suptitle("slopninja · Training-author scale", x=.065, ha="left", fontsize=16, fontweight="bold")
    fig.text(.065, .90, f"3,557 fixed coordinate weights · 12 new fits · {treatment['author_count']} new test authors · "
             f"{treatment['query_count']} test posts", color="#626f7c")
    fig.text(.065, .02, "Three-seed mean metrics; no prediction ensemble. Only 300 minus 100 authors is primary.\n"
             "Secondary intervals are unadjusted; conditional diagnostics reset learned corrections while retaining all original families.",
             fontsize=8, color="#626f7c")
    fig.subplots_adjust(left=.065, right=.975, top=.78, bottom=.30, wspace=1.0)
    for extension in ["png", "svg", "pdf"]:
        fig.savefig(args.out / f"training-author-scale.{extension}", dpi=200, facecolor="white")
    plt.close(fig)
    (args.out / "provenance.json").write_text(json.dumps({
        "selection_sha256": digest(args.selection), "report_sha256": digest(args.report),
        "plot_source_sha256": digest(Path(__file__)), "matplotlib": matplotlib.__version__,
        "files_sha256": {path.name: digest(path) for path in sorted(args.out.iterdir()) if path.is_file()},
    }, indent=2) + "\n")


if __name__ == "__main__":
    main()
