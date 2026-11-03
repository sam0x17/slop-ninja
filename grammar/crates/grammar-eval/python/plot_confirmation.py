"""Plot a frozen spline confirmation report using the separate plot environment."""
import argparse
import hashlib
import importlib.metadata
import json
from pathlib import Path

import matplotlib

matplotlib.use("Agg")
import matplotlib.pyplot as plt


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def signed(value):
    # Display floating-point zero without suggesting a negative interval bound.
    return f"{0.0 if abs(value) < 0.005 else value:+.2f}"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--report", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    report = json.loads(args.report.read_text())
    if report["schema"] != "unslop-spline-confirmation-result-v1":
        raise ValueError("Expected a frozen spline confirmation result")
    models = {row["name"]: row for row in report["models"]}
    rows = [
        ("Spline · 45 parameters", models["spline_45"]["seed_mean"]["macro_author"]["top1"]),
        ("Positive weights · 15 parameters", models["diagonal_15"]["seed_mean"]["macro_author"]["top1"]),
        ("Tied weights · 8 parameters", models["tied_8"]["seed_mean"]["macro_author"]["top1"]),
        ("Fixed words + bigrams", report["fixed_word_baseline"]["macro_author"]["top1"]),
        ("Fixed combined weights", report["fixed_combined_baseline"]["macro_author"]["top1"]),
    ]
    comparisons = [
        ("Spline − 15 parameters\nPrimary", "spline_45_minus_diagonal_15"),
        ("Spline − fixed words\nSecondary", "spline_45_minus_word"),
        ("8 − 15 parameters\nSecondary", "tied_8_minus_diagonal_15"),
    ]
    plt.rcParams.update({"font.family": "DejaVu Sans", "font.size": 10,
                         "svg.fonttype": "none", "pdf.fonttype": 42})
    fig, axes = plt.subplots(1, 2, figsize=(13.5, 5.7), gridspec_kw={"width_ratios": [1, 1.05]})
    fig.subplots_adjust(left=0.22, right=0.97, bottom=0.25, top=0.73, wspace=0.55)
    primary = report["primary"]
    decision = "met" if primary["positive_lower_95_bound"] else "not met"
    fig.suptitle(f"Spline confirmation: confidence criterion {decision}",
                 x=0.03, y=0.96, ha="left", fontsize=16, weight="bold")
    fig.text(0.03, 0.89, f"{report['candidate_authors']} new authors · {report['test_posts']} held-out posts · "
             "9 frozen checkpoints · no retraining or reselection", color="#526074")
    labels, values = zip(*rows)
    colors = ["#16826c", "#2563a6", "#648eba", "#9299a4", "#b8bdc5"]
    axes[0].barh(range(len(rows)), [value * 100 for value in values], height=0.6, color=colors)
    axes[0].set_yticks(range(len(rows)), labels)
    axes[0].invert_yaxis()
    axes[0].set_xlim(0, 70)
    axes[0].set_xlabel("Macro-author top-1 accuracy (%)", labelpad=10)
    axes[0].set_title("Fresh author identification", loc="left", pad=18)
    for i, value in enumerate(values):
        axes[0].text(value * 100 + 1, i, f"{value * 100:.2f}%", va="center", fontsize=10)
    for i, (_, key) in enumerate(comparisons):
        comparison = report["comparisons"][key]["metrics"]["top1"]
        point = comparison["mean_difference"] * 100
        low, high = [value * 100 for value in comparison["percentile_95"]]
        color = "#16826c" if i == 0 else "#818998"
        axes[1].errorbar(point, i, xerr=[[point - low], [high - point]], marker="o",
                         color=color, capsize=5, markersize=7, linewidth=2)
        axes[1].text(point, i - 0.28, f"{signed(point)} [{signed(low)}, {signed(high)}]",
                     ha="center", color=color, fontsize=9)
    axes[1].set_yticks(range(len(comparisons)), [label for label, _ in comparisons])
    axes[1].set_ylim(2.6, -0.7)
    axes[1].set_xlim(-5, 15)
    axes[1].axvline(0, color="#9da5b0", linestyle="--", linewidth=1)
    axes[1].set_xlabel("Difference in percentage points", labelpad=10)
    axes[1].set_title("Paired 95% author-bootstrap intervals", loc="left", pad=18)
    for ax in axes:
        ax.grid(axis="x", color="#e5e7eb", linewidth=0.6)
        ax.set_axisbelow(True)
        ax.spines[["top", "right", "left"]].set_visible(False)
        ax.spines["bottom"].set_color("#b8bdc5")
        ax.tick_params(axis="y", length=0)
    fig.text(0.03, 0.13, "Accuracy averages per-query metrics across 3 independent fits, then gives each author equal weight.",
             fontsize=10, color="#526074")
    fig.text(0.03, 0.085, "Primary success required a positive lower 95% bound. Intervals use 2,000 paired author resamples and condition on the fixed gallery and seeds.",
             fontsize=9, color="#526074")
    fig.text(0.03, 0.04, "Earlier exploratory results are not pooled here. This measures author retrieval, not editing quality or detector performance.",
             fontsize=9, color="#526074")
    args.out.mkdir(parents=True, exist_ok=False)
    for extension in ["png", "svg", "pdf"]:
        fig.savefig(args.out / f"confirmation.{extension}", dpi=180, facecolor="white")
    plt.close(fig)
    provenance = {
        "schema": "unslop-confirmation-figure-v1",
        "report_sha256": sha(args.report), "plot_script_sha256": sha(Path(__file__)),
        "matplotlib": importlib.metadata.version("matplotlib"),
        "outputs": {path.name: sha(path) for path in sorted(args.out.iterdir())},
    }
    (args.out / "provenance.json").write_text(json.dumps(provenance, indent=2) + "\n")
    print(f"Wrote confirmation figures to {args.out}")


if __name__ == "__main__":
    main()
