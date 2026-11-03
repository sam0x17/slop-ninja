"""Plot aggregate frontier results without importing the training environment."""

import argparse
import csv
import hashlib
import importlib.metadata
import json
import platform
from pathlib import Path
from statistics import mean

import matplotlib

matplotlib.use("Agg")
import matplotlib.pyplot as plt
from matplotlib.lines import Line2D
from matplotlib.ticker import NullLocator


STYLES = {
    "positive": ("Positive weights", "#2563a6", "o"),
    "spline": ("Monotone spline", "#16826c", "s"),
    "linear": ("Signed linear", "#8250aa", "D"),
    "mlp": ("Tanh network", "#c66023", "^"),
}


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def load_rows(training, evaluation):
    selection_path = training / "selection.json"
    report_path = evaluation / "report.json"
    selection = json.loads(selection_path.read_text())
    report = json.loads(report_path.read_text())
    if report["selection_sha256"] != digest(selection_path):
        raise ValueError("Test report does not match frozen selection")
    if report["selected_model"] != selection["selected_model"]:
        raise ValueError("Selected model mismatch")
    candidates = {row["name"]: row for row in selection["candidates"]}
    rows = []
    for model in report["models"]:
        if model["status"] != "complete":
            raise ValueError("Plot requires a complete frontier")
        config = model["config"]
        name = config["name"]
        runs = [run for run in selection["runs"] if run["config"]["name"] == name]
        if sorted(run["seed"] for run in runs) != sorted(selection["seeds"]):
            raise ValueError(f"Incomplete seeds for {name}")
        dev = [run["selected_development"]["macro_author"]["top1"] * 100 for run in runs]
        test = [seed["metrics"]["macro_author"]["top1"] * 100 for seed in model["seeds"]]
        ci = model["paired_vs_diagonal_15"]["metrics"]["top1"]
        rows.append({
            "name": name,
            "family": "positive" if config["kind"] in ("tied", "diagonal") else config["kind"],
            "parameters": config["parameters"],
            "ratio_to_15": config["parameters"] / 15,
            "selected_on_development": name == selection["selected_model"],
            "dev_top1_pct": candidates[name]["mean_seed_development"]["top1"] * 100,
            "dev_min_pct": min(dev),
            "dev_max_pct": max(dev),
            "test_top1_pct": model["seed_mean"]["macro_author"]["top1"] * 100,
            "test_min_pct": min(test),
            "test_max_pct": max(test),
            "test_delta_vs_15_pp": ci["mean_difference"] * 100,
            "test_delta_ci_low_pp": ci["percentile_95"][0] * 100,
            "test_delta_ci_high_pp": ci["percentile_95"][1] * 100,
            "training_seconds_per_fit": mean(run["training_seconds"] for run in runs),
            "warm_scoring_ms_per_168_queries": mean(run["scoring_seconds_median"] for run in runs) * 1000,
        })
    if len(rows) != len(candidates):
        raise ValueError("Frontier model coverage mismatch")
    return sorted(rows, key=lambda row: row["parameters"]), report


def style_axis(ax):
    ax.set_xscale("log", base=2)
    ax.set_xlim(6, 23000)
    ticks = [8, 16, 64, 256, 1024, 4096, 16384]
    ax.set_xticks(ticks, [f"{tick:,}" for tick in ticks])
    ax.xaxis.set_minor_locator(NullLocator())
    ax.grid(axis="y", color="#e1e5ea", linewidth=0.7)
    ax.set_axisbelow(True)
    ax.spines[["top", "right"]].set_visible(False)
    ax.spines[["left", "bottom"]].set_color("#b5bdc9")
    ax.set_xlabel("Stored trainable parameters (log scale)", labelpad=10)


def draw_rows(ax, rows, metric, interval=None):
    for family, (_, color, marker) in STYLES.items():
        selected = [row for row in rows if row["family"] == family]
        x = [row["parameters"] for row in selected]
        y = [row[metric] for row in selected]
        if interval:
            low, high = interval
            errors = [[max(0, row[metric] - row[low]) for row in selected],
                      [max(0, row[high] - row[metric]) for row in selected]]
            ax.errorbar(x, y, yerr=errors, color=color, marker=marker,
                        markersize=6, capsize=3, linewidth=1.4)
        else:
            ax.plot(x, y, color=color, marker=marker, markersize=6, linewidth=1.4)
    winner = next(row for row in rows if row["selected_on_development"])
    ax.scatter([winner["parameters"]], [winner[metric]], marker="*", s=190,
               color="#111827", edgecolor="white", linewidth=0.6, zorder=8)


def save_figure(fig, out, stem):
    for extension in ("png", "svg", "pdf"):
        fig.savefig(out / f"{stem}.{extension}", dpi=180, facecolor="white")
    plt.close(fig)


def make_figures(rows, report, out):
    plt.rcParams.update({"font.family": "DejaVu Sans", "font.size": 10,
                         "axes.titlesize": 12, "axes.labelsize": 10,
                         "svg.fonttype": "none", "pdf.fonttype": 42})
    handles = [Line2D([0], [0], color=color, marker=marker, label=label)
               for label, color, marker in STYLES.values()]
    handles.append(Line2D([0], [0], color="#111827", marker="*", markersize=12,
                          linestyle="none", label="Selected on development"))
    fig, axes = plt.subplots(1, 2, figsize=(13, 5.8))
    fig.subplots_adjust(left=0.065, right=0.98, bottom=0.23, top=0.76, wspace=0.23)
    fig.suptitle("Larger tanh networks did not beat the small model on new authors",
                 x=0.065, y=0.96, ha="left", fontsize=14, weight="bold")
    fig.text(0.065, 0.9, "Same 14 word and grammar distances · 15 architectures · 3 seeds · 200 Adam updates",
             color="#526074")
    for ax, split, title in zip(axes, ("dev", "test"),
                               ("Development: 100 authors, 168 posts", "Fresh test: 98 authors, 219 posts")):
        style_axis(ax)
        ax.set_title(title, loc="left", pad=12)
        ax.set_ylabel("Macro-author top-1 accuracy (%)")
        draw_rows(ax, rows, f"{split}_top1_pct", (f"{split}_min_pct", f"{split}_max_pct"))
    axes[0].set_ylim(55, 67)
    axes[1].set_ylim(38, 50)
    word = report["fixed_baselines"]["word"]["macro_author"]["top1"] * 100
    axes[1].axhline(word, linestyle="--", color="#777f8d", linewidth=1)
    axes[1].text(110, word - 0.55, f"Fixed words + bigrams: {word:.2f}%", fontsize=9, color="#616b79")
    fig.legend(handles=handles, loc="lower left", bbox_to_anchor=(0.055, 0.075),
               ncol=5, frameon=False, fontsize=9)
    fig.text(0.065, 0.055, "Points: mean of 3 independent fits. Whiskers: seed range, not confidence intervals. Panel y-ranges differ.",
             fontsize=9, color="#526074")
    fig.text(0.065, 0.018, "Architecture and preprocessing also vary; this is not an isolated test of parameter count. Test-leading spline results are exploratory.",
             fontsize=9, color="#526074")
    save_figure(fig, out, "accuracy-frontier")

    fig, axes = plt.subplots(1, 2, figsize=(13, 5.3))
    fig.subplots_adjust(left=0.07, right=0.98, bottom=0.24, top=0.74, wspace=0.23)
    fig.suptitle("Measured cost of the same model-size sweep", x=0.07, y=0.96,
                 ha="left", fontsize=14, weight="bold")
    fig.text(0.07, 0.89, "CPU float64 · 8 threads · scorer only; corpus parsing and feature extraction excluded", color="#526074")
    for ax, metric, title, ylabel in zip(
        axes,
        ("training_seconds_per_fit", "warm_scoring_ms_per_168_queries"),
        ("Training compute per fit", "Warm scoring: 168 queries × 100 authors"),
        ("Seconds (log scale)", "Milliseconds (log scale)"),
    ):
        style_axis(ax)
        ax.set_yscale("log")
        ax.set_title(title, loc="left", pad=12)
        ax.set_ylabel(ylabel)
        draw_rows(ax, rows, metric)
    fig.legend(handles=handles, loc="lower left", bbox_to_anchor=(0.06, 0.07),
               ncol=5, frameon=False, fontsize=9)
    fig.text(0.07, 0.03, "Mean across 3 fits. Training excludes development scoring and checkpoint I/O. Warm scoring uses each fit's median of 5 repeats.",
             fontsize=9, color="#526074")
    save_figure(fig, out, "cost-frontier")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--training", required=True, type=Path)
    parser.add_argument("--evaluation", required=True, type=Path)
    parser.add_argument("--out", required=True, type=Path)
    args = parser.parse_args()
    rows, report = load_rows(args.training, args.evaluation)
    args.out.mkdir(parents=True, exist_ok=False)
    with (args.out / "frontier.csv").open("w", newline="") as handle:
        writer = csv.DictWriter(handle, fieldnames=list(rows[0]))
        writer.writeheader()
        writer.writerows(rows)
    make_figures(rows, report, args.out)
    provenance = {
        "schema": "unslop-frontier-figures-v1",
        "selection_sha256": digest(args.training / "selection.json"),
        "report_sha256": digest(args.evaluation / "report.json"),
        "plot_script_sha256": digest(Path(__file__)),
        "python": platform.python_version(),
        "packages": {name: importlib.metadata.version(name) for name in ("matplotlib", "numpy")},
        "figure_intervals": "Range of three seeds, not sampling confidence intervals",
        "outputs": {path.name: digest(path) for path in sorted(args.out.iterdir())},
    }
    (args.out / "provenance.json").write_text(json.dumps(provenance, indent=2) + "\n")
    print(f"Wrote frontier figures and aggregate data to {args.out}")


if __name__ == "__main__":
    main()
