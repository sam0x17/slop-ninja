"""Plot frozen descriptive full-family ablations in the pinned plot environment."""
import argparse
import csv
import hashlib
import json
from pathlib import Path

import matplotlib

matplotlib.use("Agg")
import matplotlib.pyplot as plt


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--protocol", type=Path, required=True)
    parser.add_argument("--report", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    report = json.loads(args.report.read_text())
    if report["schema"] != "slopninja-full-family-ablation-result-v1":
        raise ValueError("Unsupported report schema")
    if report["protocol_sha256"] != digest(args.protocol):
        raise ValueError("Protocol binding differs")
    if args.out.exists():
        raise ValueError("Choose a fresh figure directory")
    groups = ["overall", "content_advantage", "content_disadvantage"]
    group_names = ["All evaluated posts", "True author closest by content", "Different author closer by content"]
    variants = [("full", "Words + grammar", "#23815a"),
                ("lexical_family_only", "Word families only", "#007fa3"),
                ("grammar_family_only", "Grammar families only", "#74808b")]
    summaries = {(row["variant"], row["stratum"]): row for row in report["summaries"]
                 if row["gallery"] == "pooled"}
    plt.rcParams.update({"font.family": "DejaVu Sans", "font.size": 10,
                         "axes.spines.top": False, "axes.spines.right": False,
                         "svg.fonttype": "none", "pdf.fonttype": 42})
    fig, axis = plt.subplots(figsize=(11, 5.8))
    rows = []
    width = .23
    for variant_index, (variant, label, color) in enumerate(variants):
        positions = [index + (variant_index - 1) * width for index in range(3)]
        values = [summaries[(variant, group)]["macro_author"]["top1"] * 100 for group in groups]
        bars = axis.bar(positions, values, width=width, color=color, label=label)
        axis.bar_label(bars, labels=[f"{value:.2f}" for value in values], padding=4, fontsize=9)
        for group, value in zip(groups, values):
            result = summaries[(variant, group)]
            rows.append({"variant": variant, "stratum": group, "authors": result["author_count"],
                         "queries": result["query_count"], "top1_percent": value,
                         "top5_percent": result["macro_author"]["top5"] * 100,
                         "mrr": result["macro_author"]["mrr"],
                         "nearest_content_impostor_concordance": result["macro_author"]["nearest_content_impostor_concordance"]})
    labels = []
    for group, label in zip(groups, group_names):
        result = summaries[("full", group)]
        labels.append(f"{label}\n{result['author_count']} authors · {result['query_count']} posts")
    axis.set_xticks(range(3), labels)
    axis.set_ylim(0, 100)
    axis.set_ylabel("Macro-author top-1 accuracy (%)")
    axis.grid(axis="y", color="#e5e8eb", lw=.7)
    axis.set_axisbelow(True)
    axis.legend(loc="upper left", bbox_to_anchor=(0, 1.16), ncol=3, frameon=False)
    fig.suptitle("slopninja · Complete word and grammar contributions", x=.08, y=.98,
                 ha="left", fontsize=15, fontweight="bold")
    fig.text(.08, .91, "Same fitted model and three inherited seeds; remove final family terms without refitting or renormalizing.",
             fontsize=10, color="#626f7c")
    fig.text(.08, .045, "Descriptive analysis on previously evaluated data. Content groups use frozen TF-IDF distances, not topic labels.\n"
             "Authors can contribute to both content groups; conditional author means do not recombine into the overall mean.",
             fontsize=9, color="#626f7c")
    fig.subplots_adjust(left=.08, right=.98, top=.76, bottom=.22)
    args.out.mkdir(parents=True)
    with (args.out / "aggregate.csv").open("w", newline="") as stream:
        writer = csv.DictWriter(stream, fieldnames=list(rows[0]))
        writer.writeheader()
        writer.writerows(rows)
    for extension in ["png", "svg", "pdf"]:
        fig.savefig(args.out / f"full-family-ablation.{extension}", dpi=200, facecolor="white")
    plt.close(fig)
    (args.out / "provenance.json").write_text(json.dumps({
        "protocol_sha256": digest(args.protocol), "report_sha256": digest(args.report),
        "plot_source_sha256": digest(Path(__file__)), "matplotlib": matplotlib.__version__,
        "files_sha256": {path.name: digest(path) for path in sorted(args.out.iterdir()) if path.is_file()},
    }, indent=2) + "\n")


if __name__ == "__main__":
    main()
