"""Plot validation selection and fresh-author results in a separate environment."""
import argparse
import csv
import hashlib
import json
from pathlib import Path
from statistics import mean

import matplotlib

matplotlib.use("Agg")
import matplotlib.pyplot as plt
from matplotlib.lines import Line2D


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--selection", type=Path, required=True)
    parser.add_argument("--report", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    selection = json.loads(args.selection.read_text())
    report = json.loads(args.report.read_text())
    if selection["schema"] != "slopninja-frozen-author-selection-v1" or report["schema"] != "slopninja-author-disjoint-selection-result-v1":
        raise ValueError("Expected author-disjoint selection and results")
    if report["selection_sha256"] != sha(args.selection) or report["selected_architectures"] != selection["selected_architectures"]:
        raise ValueError("Report differs from frozen selection")
    models = {row["config"]["name"]: row for row in report["models"]}
    rows = []
    labels = []
    subset_names = {"words": "Words", "grammar": "Grammar", "combined": "Combined"}
    colors = {"words": "#2563a6", "grammar": "#c66023", "combined": "#16826c"}
    for config in selection["configs"]:
        name, subset = config["name"], config["subset"]
        runs = [row for row in selection["runs"] if row["config"] == config]
        if len(runs) != 3 or sorted(row["seed"] for row in runs) != [0, 17, 29]:
            raise ValueError("Missing training seeds")
        model = models[name]
        val = [row["selected_validation"]["macro_author"]["top1"] * 100 for row in runs]
        test = [row["pooled"]["macro_author"]["top1"] * 100 for row in model["seeds"]]
        rows.append({
            "name": name, "subset": subset, "kind": config["kind"], "parameters": config["parameters"],
            "selected": selection["selected_architectures"][subset] == name,
            "validation_pct": mean(val), "validation_min_pct": min(val), "validation_max_pct": max(val),
            "test_pct": model["seed_mean"]["macro_author"]["top1"] * 100,
            "test_min_pct": min(test), "test_max_pct": max(test),
            "test_top5_pct": model["seed_mean"]["macro_author"]["top5"] * 100,
            "test_content_pair_pct": model["seed_mean"]["macro_author"]["own_vs_content_impostor"] * 100,
        })
        labels.append(f"{subset_names[subset]}\n{config['kind'].capitalize()}\n{config['parameters']} parameters")
    plt.rcParams.update({"font.family": "DejaVu Sans", "font.size": 10, "svg.fonttype": "none", "pdf.fonttype": 42})
    fig, axes = plt.subplots(1, 2, figsize=(14, 6.5), sharey=True)
    fig.subplots_adjust(left=0.06, right=0.98, top=0.74, bottom=0.32, wspace=0.1)
    fig.suptitle("Word and grammar models selected on unseen authors", x=0.06, y=0.96,
                 ha="left", fontsize=16, weight="bold")
    fig.text(0.06, 0.90, "100 fitting authors · 6 configurations × 3 seeds · architecture and checkpoints selected before final test",
             color="#526074")
    val_example = selection["runs"][0]["selected_validation"]
    val_title = f"Validation: {val_example['author_count']} authors, {val_example['query_count']} posts"
    test_title = f"Test: {sum(report['gallery_author_counts'].values())} authors, {sum(report['gallery_query_counts'].values())} posts across 3 galleries"
    for ax, split, title in zip(axes, ["validation", "test"], [val_title, test_title]):
        ax.set_title(title, loc="left", pad=16, fontsize=11)
        for i, row in enumerate(rows):
            value = row[f"{split}_pct"]
            low, high = row[f"{split}_min_pct"], row[f"{split}_max_pct"]
            ax.errorbar(i, value, yerr=[[max(0, value - low)], [max(0, high - value)]],
                        fmt="o", color=colors[row["subset"]], markersize=7, capsize=4, linewidth=1.8)
            if row["selected"]:
                ax.scatter([i], [value], s=180, marker="*", color="#111827", edgecolor="white", linewidth=0.5, zorder=5)
            ax.text(i, high + 3, f"{value:.2f}%", ha="center", fontsize=9, color=colors[row["subset"]])
        ax.set_xticks(range(len(rows)), labels, fontsize=9)
        ax.set_xlim(-0.5, len(rows) - 0.5)
        ax.set_ylim(0, 100)
        ax.set_yticks(range(0, 101, 20))
        ax.grid(axis="y", color="#e1e5ea", linewidth=0.7)
        ax.set_axisbelow(True)
        ax.spines[["top", "right"]].set_visible(False)
        ax.spines[["left", "bottom"]].set_color("#b5bdc9")
    axes[0].set_ylabel("Macro-author top-1 accuracy (%)")
    handles = [Line2D([0], [0], color=colors[name], marker="o", linestyle="none", label=subset_names[name]) for name in colors]
    handles.append(Line2D([0], [0], color="#111827", marker="*", markersize=12, linestyle="none", label="Selected on validation"))
    fig.legend(handles=handles, loc="lower left", bbox_to_anchor=(0.05, 0.17), ncol=4, frameon=False)
    primary = report["primary"]
    low, high = [value * 100 for value in primary["percentile_95"]]
    fig.text(0.06, 0.135, f"Primary selected combined − selected words: {primary['mean_difference'] * 100:+.2f} points; "
             f"paired 95% interval [{low:+.2f}, {high:+.2f}].", fontsize=11, weight="bold")
    fig.text(0.06, 0.085, "Points average 3 independent fits; whiskers show seed range, not confidence intervals. Authors receive equal weight.",
             fontsize=9, color="#526074")
    fig.text(0.06, 0.04, "Primary interval resamples authors within each fixed test gallery. Subsets differ in feature inputs, parameter count and training-only calibration grids.",
             fontsize=9, color="#526074")
    args.out.mkdir(parents=True, exist_ok=False)
    for extension in ["png", "svg", "pdf"]:
        fig.savefig(args.out / f"author-selection.{extension}", dpi=180, facecolor="white")
    plt.close(fig)
    with (args.out / "aggregate.csv").open("w", newline="") as handle:
        writer = csv.DictWriter(handle, fieldnames=list(rows[0]))
        writer.writeheader()
        writer.writerows(rows)
    provenance = {"schema": "slopninja-author-selection-figures-v1", "selection_sha256": sha(args.selection),
                  "report_sha256": sha(args.report), "script_sha256": sha(Path(__file__)),
                  "outputs": {path.name: sha(path) for path in sorted(args.out.iterdir())}}
    (args.out / "provenance.json").write_text(json.dumps(provenance, indent=2) + "\n")
    print(f"Wrote author-selection figures to {args.out}")


if __name__ == "__main__":
    main()
