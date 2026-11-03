"""Plot frozen coordinate capacity results using the separate plot environment."""
import argparse
import csv
import hashlib
import json
from pathlib import Path

import matplotlib
matplotlib.use("Agg")
import matplotlib.pyplot as plt
import numpy as np


def read(path):
    return json.loads(path.read_text())


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--selection", type=Path, required=True)
    parser.add_argument("--report", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    if args.out.exists():
        raise ValueError("Choose a fresh figure directory")
    selection, report = read(args.selection), read(args.report)
    if selection["schema"] != "slopninja-frozen-coordinate-frontier-v1" or report["schema"] != "slopninja-coordinate-frontier-test-result-v1":
        raise ValueError("Unsupported frontier artifacts")
    if report["selection_sha256"] != digest(args.selection) or selection["winner"] != report["winner"]:
        raise ValueError("Report does not bind the frozen selection")
    rows = []
    for cell in selection["cells"]:
        config = cell["config"]
        model = report["models"][config["name"]]
        fits = [run for run in selection["runs"] if run["config"]["name"] == config["name"] and run["lambda"] == cell["selected_lambda"]]
        rows.append({"config": config["name"], "adjustable_subset": config["block"],
                     "coordinates": config["coordinate_count"], "lambda": cell["selected_lambda"],
                     "validation_top1_percent": cell["mean_seed_validation"]["top1"] * 100,
                     "test_top1_percent": model["seed_mean"]["macro_author"]["top1"] * 100,
                     "test_top5_percent": model["seed_mean"]["macro_author"]["top5"] * 100,
                     "test_mrr": model["seed_mean"]["macro_author"]["mrr"],
                     "test_seed_min_percent": model["seed_macro_top1_range"][0] * 100,
                     "test_seed_max_percent": model["seed_macro_top1_range"][1] * 100,
                     "mean_fit_seconds": float(np.mean([run["elapsed_seconds"] for run in fits])),
                     "validation_selected_winner": config["name"] == selection["winner"]})
    args.out.mkdir(parents=True)
    with (args.out / "aggregate.csv").open("w", newline="") as handle:
        writer = csv.DictWriter(handle, fieldnames=list(rows[0]))
        writer.writeheader()
        writer.writerows(rows)
    plt.rcParams.update({"font.family": "DejaVu Sans", "font.size": 9,
                         "axes.spines.top": False, "axes.spines.right": False,
                         "svg.fonttype": "none", "pdf.fonttype": 42})
    fig, axes = plt.subplots(1, 3, figsize=(14, 5), gridspec_kw={"width_ratios": [1, 1, 1.15]})
    colors = {"word": "#007fa3", "grammar": "#b7662d", "all": "#23815a"}
    labels = {"word": "Word updates", "grammar": "Grammar updates", "all": "Both"}
    winner = next(row for row in rows if row["validation_selected_winner"])
    for axis, metric, title in [(axes[0], "validation_top1_percent", "Validation: select the model"),
                                (axes[1], "test_top1_percent", "New test authors: frozen choices")]:
        for subset in ["word", "grammar", "all"]:
            values = sorted((row for row in rows if row["adjustable_subset"] == subset), key=lambda row: row["coordinates"])
            x = [row["coordinates"] for row in values]
            y = [row[metric] for row in values]
            axis.plot(x, y, "o-", color=colors[subset], ms=4, lw=1.5, label=labels[subset])
            if metric == "test_top1_percent":
                error = [[row[metric] - row["test_seed_min_percent"] for row in values],
                         [row["test_seed_max_percent"] - row[metric] for row in values]]
                axis.errorbar(x, y, yerr=np.maximum(error, 0), fmt="none", color=colors[subset], lw=.8, capsize=2)
        axis.scatter([winner["coordinates"]], [winner[metric]], marker="*", s=150,
                     facecolor=colors[winner["adjustable_subset"]], edgecolor="black", linewidth=.6, zorder=5)
        axis.set_xscale("log", base=2)
        axis.set_xticks([512, 1024, 2048, 4096, 8192], ["512", "1,024", "2,048", "4,096", "8,192"])
        axis.tick_params(axis="x", labelrotation=30)
        axis.set_xlabel("Trainable coordinate weights")
        axis.set_ylabel("Macro-author top-1 accuracy (%)")
        axis.set_title(title, loc="left", fontweight="bold", pad=12)
        axis.grid(color="#e5e8eb", lw=.6)
    baseline = report["models"]["incumbent"]["seed_mean"]["macro_author"]["top1"] * 100
    axes[1].axhline(baseline, color="#626f7c", linestyle="--", lw=1, label="Frozen incumbent")
    axes[0].legend(loc="upper left", bbox_to_anchor=(0, -.36), ncols=3, frameon=False, columnspacing=1)
    axes[1].legend(handles=[axes[1].lines[-1]], labels=["Frozen incumbent"], loc="upper left", bbox_to_anchor=(0, -.36), frameon=False)

    comparisons = [("winner_minus_incumbent", "Winner - incumbent"),
                   ("winner_minus_family_baseline", "Winner - family baseline"),
                   ("combined_minus_word_eta_only", "Combined - word updates"),
                   ("combined_minus_grammar_eta_only", "Combined - grammar updates")]
    contrast_rows = []
    for index, (key, label) in enumerate(comparisons):
        value = report["comparisons"][key]["metrics"]["top1"]
        point = value["mean_difference"] * 100
        low, high = [v * 100 for v in value["percentile_95"]]
        color = "#23815a" if index == 0 else "#718096"
        axes[2].plot([low, high], [index, index], color=color, lw=2)
        axes[2].scatter([point], [index], marker="D", color=color, s=32, zorder=3)
        contrast_rows.append({"comparison": key, "difference_points": point, "lower_95_points": low, "upper_95_points": high})
    axes[2].axvline(0, color="#626f7c", linestyle="--", lw=1)
    axes[2].set_yticks(range(len(comparisons)), [label for _, label in comparisons])
    axes[2].invert_yaxis()
    axes[2].set_ylim(len(comparisons) - .6, -.6)
    axes[2].set_title("Paired differences and 95% intervals", loc="left", fontweight="bold", pad=12)
    axes[2].set_xlabel("Accuracy difference (percentage points)")
    axes[2].grid(axis="x", color="#e5e8eb", lw=.6)
    with (args.out / "comparisons.csv").open("w", newline="") as handle:
        writer = csv.DictWriter(handle, fieldnames=list(contrast_rows[0]))
        writer.writeheader()
        writer.writerows(contrast_rows)
    test = report["models"]["incumbent"]["seed_mean"]
    fig.suptitle("slopninja · Coordinate capacity and adjustable feature groups", x=.055, ha="left", fontsize=15, fontweight="bold")
    fig.text(.055, .90, f"16 configurations · 192 fits · {test['author_count']} new test authors · "
             f"{test['query_count']} test posts · star: validation-selected {selection['winner']}", color="#626f7c")
    fig.text(.055, .025, "All models retain the original word and grammar distances. Test curve whiskers show three-seed ranges.\n"
             "Only winner minus incumbent is primary; other paired intervals are unadjusted diagnostics. Reset ablations remove learned corrections.",
             fontsize=8, color="#626f7c")
    fig.subplots_adjust(left=.055, right=.985, top=.79, bottom=.31, wspace=.85)
    for extension in ["png", "svg", "pdf"]:
        fig.savefig(args.out / f"coordinate-frontier.{extension}", dpi=200, facecolor="white")
    plt.close(fig)
    (args.out / "provenance.json").write_text(json.dumps({
        "selection_sha256": digest(args.selection), "report_sha256": digest(args.report),
        "plot_source_sha256": digest(Path(__file__)), "matplotlib": matplotlib.__version__, "numpy": np.__version__,
        "files_sha256": {path.name: digest(path) for path in sorted(args.out.iterdir()) if path.is_file()},
    }, indent=2) + "\n")


if __name__ == "__main__":
    main()
