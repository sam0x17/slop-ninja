"""Reproduce the first feature/detector comparison without making API calls."""

import json
from pathlib import Path

from unslop.evaluation import summarize_runs
from unslop.experiments import compare_features
from unslop.features import extract
from unslop.store import digest

ROOT = Path(__file__).resolve().parents[2]
ARCHIVE = ROOT / "experiments/pangram-preface"


def read(path):
    return path.read_bytes().decode("utf-8")


def main():
    records = [json.loads(read(path)) for path in sorted((ARCHIVE / "results").glob("*.json"))]
    source = read(ARCHIVE / "sections/part_c/03.txt")
    candidate = read(ARCHIVE / "sections/part_c/08.txt")
    a, b = extract(source, grammar=True), extract(candidate, grammar=True)
    pair = compare_features(source, candidate, a, b)
    source_runs = [r for r in records if r["sha256"] == digest(source)]
    candidate_runs = [r for r in records if r["sha256"] == digest(candidate)]
    final = read(ROOT / "preface.txt")
    final_runs = [r for r in records if r["sha256"] == digest(final)]
    report = {
        "archive": "experiments/pangram-preface/results",
        "requests": len(records), "distinct_texts": len({r["sha256"] for r in records}),
        "source_families": 1,
        "submitted_whitespace_words": sum(len(r["submitted_text"].split()) for r in records),
        "word_edit": {
            "source": "experiments/pangram-preface/sections/part_c/03.txt",
            "candidate": "experiments/pangram-preface/sections/part_c/08.txt",
            "source_sha256": digest(source), "candidate_sha256": digest(candidate),
            "extractor": a["extractor"],
            "source_totals": a["totals"], "candidate_totals": b["totals"],
            "feature_count_deltas": pair["feature_count_deltas"],
            "unchanged_families": sorted(k for k, v in pair["feature_count_deltas"].items() if not v),
            "source_runs": summarize_runs(source_runs),
            "candidate_runs": summarize_runs(candidate_runs),
            "scope": "closing section only; not a full-document success",
        },
        "final_preface": {"text": "preface.txt", "sha256": digest(final), "runs": summarize_runs(final_runs)},
        "live_requests_made": 0,
        "conclusion": "A repeated local lexical effect is visible without changes in the extracted grammar features. Cross-source transfer and reliable full-document performance remain untested by this pilot.",
    }
    out = ROOT / "experiments/preface-profile/report.json"
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps(report, indent=2, ensure_ascii=False, allow_nan=False) + "\n", encoding="utf-8")
    print(json.dumps({"report": str(out.relative_to(ROOT)), "requests_reused": len(records),
                      "source_repeats": len(source_runs), "candidate_repeats": len(candidate_runs),
                      "final_repeats": len(final_runs), "unchanged_families": report["word_edit"]["unchanged_families"]}, indent=2))


if __name__ == "__main__":
    main()
