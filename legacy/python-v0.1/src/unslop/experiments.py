"""Exact local perturbations and paired feature observations."""

import difflib
import re
import unicodedata
from .features import words
from .store import digest


def substitute(text, old, new, occurrence=1):
    """Replace one explicitly selected whole word; never choose a synonym implicitly."""
    if not old or not new or any(c.isspace() for c in old + new):
        raise ValueError("Single-word experiments require nonempty words without spaces")
    for value in (old, new):
        normalized = unicodedata.normalize("NFC", value).translate(str.maketrans({"’": "'", "‘": "'", "ʼ": "'", "＇": "'"})).lower()
        if words(value) != [unicodedata.normalize("NFC", normalized)]:
            raise ValueError("Single-word experiments require one complete lexical word on each side")
    if occurrence < 1:
        raise ValueError("Occurrence is 1-based")
    matches = list(re.finditer(r"(?<![\w'’])" + re.escape(old) + r"(?![\w'’])", text))
    if len(matches) < occurrence:
        raise ValueError(f"Only {len(matches)} exact whole-word occurrences of {old!r}")
    match = matches[occurrence - 1]
    candidate = text[:match.start()] + new + text[match.end():]
    if candidate == text:
        raise ValueError("Perturbation does not change text")
    return candidate, {"kind": "single_word", "old": old, "new": new,
                       "occurrence": occurrence, "start": match.start(), "end": match.end(),
                       "source_sha256": digest(text), "candidate_sha256": digest(candidate)}


def compare_features(source_text, candidate_text, source, candidate):
    if source["extractor"] != candidate["extractor"]:
        raise ValueError("Feature comparison requires the same extractor")
    changes = {}
    for family in source["families"].keys() | candidate["families"].keys():
        a, b = source["families"].get(family, {}), candidate["families"].get(family, {})
        changes[family] = {key: b.get(key, 0) - a.get(key, 0) for key in sorted(a.keys() | b.keys()) if a.get(key, 0) != b.get(key, 0)}
    return {"source_sha256": digest(source_text), "candidate_sha256": digest(candidate_text),
            "extractor": source["extractor"], "feature_count_deltas": changes,
            "source_totals": source["totals"], "candidate_totals": candidate["totals"],
            "source_metrics": source["metrics"], "candidate_metrics": candidate["metrics"],
            "diff": "\n".join(difflib.unified_diff(source_text.splitlines(), candidate_text.splitlines(), fromfile="source", tofile="candidate", lineterm="")),
            "quality": "Descriptive features and text diffs cannot certify argument, detail, tone, or readability."}
