"""Pangram requests and conservative summaries of repeated observations.

This module performs no requests on import. Callers persist a submission record
before polling, so a timeout never requires another potentially billable POST.
"""

from __future__ import annotations

from collections import defaultdict
from datetime import datetime, timezone
import hashlib
import json
import math
import os
import statistics
import time
from typing import Any
import urllib.error
import urllib.parse
import urllib.request


API_BASE = "https://text.external-api.pangram.com"
QUALITY_DIMENSIONS = ("readability", "argumentation", "detail", "tone")


class PangramError(RuntimeError):
    """A request failed; POST requests are never automatically retried."""


class PangramTaskFailed(PangramError):
    def __init__(self, task_id: str, result: dict):
        self.task_id = task_id
        self.result = result
        super().__init__(f"Pangram task {task_id} failed; preserve its response.")


class PangramTimeout(TimeoutError):
    def __init__(self, task_id: str, last_response: dict | None):
        self.task_id = task_id
        self.last_response = last_response
        super().__init__(f"Polling timed out for {task_id}; resume this task ID.")


def text_hash(text: str) -> str:
    """Hash the UTF-8 encoding without trimming or normalizing the input."""
    return hashlib.sha256(text.encode("utf-8")).hexdigest()


def _positive(value: float, name: str) -> float:
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        raise ValueError(f"{name} must be a finite positive number")
    if not math.isfinite(value) or value <= 0:
        raise ValueError(f"{name} must be a finite positive number")
    return float(value)


def _reject_nonfinite_json(value: str) -> None:
    raise ValueError(f"Non-finite JSON value: {value}")


class PangramClient:
    """Small stdlib adapter with explicit submission and resumable polling."""

    def __init__(self, api_key: str | None = None, request_timeout: float = 60):
        self._key = api_key or os.environ.get("PANGRAM_API_KEY")
        if not self._key:
            raise ValueError("Set PANGRAM_API_KEY in the environment")
        self.request_timeout = _positive(request_timeout, "request_timeout")

    def _request(self, route: str, payload: dict | None = None,
                 timeout: float | None = None) -> dict:
        request = urllib.request.Request(
            API_BASE + route,
            data=(None if payload is None else
                  json.dumps(payload, ensure_ascii=False, allow_nan=False).encode("utf-8")),
            headers={"x-api-key": self._key, "Content-Type": "application/json"},
            method="GET" if payload is None else "POST",
        )
        try:
            with urllib.request.urlopen(
                request, timeout=self.request_timeout if timeout is None else timeout,
            ) as response:
                raw = response.read()
        except urllib.error.HTTPError as exc:
            # Do not echo an upstream body: it can contain credentials or input.
            exc.close()
            raise PangramError(f"Pangram HTTP {exc.code}; request was not retried") from None
        except (urllib.error.URLError, TimeoutError, OSError):
            raise PangramError(
                "Pangram transport error; request was not retried. "
                "A submitted task might still have been charged."
            ) from None
        try:
            result = json.loads(raw, parse_constant=_reject_nonfinite_json)
        except (ValueError, UnicodeDecodeError):
            raise PangramError("Pangram returned invalid JSON; request was not retried") from None
        if not isinstance(result, dict):
            raise PangramError("Pangram returned a non-object JSON response")
        return result

    def models(self) -> dict:
        """Return the account's available selectors, preserving server order."""
        return self._request("/models")

    def submit(self, text: str, model: str = "pangram-4") -> dict:
        """Submit once and return a record to save before calling ``poll``."""
        if not isinstance(text, str) or len(text.split()) < 50:
            raise ValueError("Pangram requires natural-language prose of at least 50 words")
        if not isinstance(model, str) or not model.strip():
            raise ValueError("An explicit model selector is required")
        submitted_at = datetime.now(timezone.utc).isoformat()
        response = self._request("/task", {
            "text": text, "model": model, "public_dashboard_link": False,
        })
        task_id = response.get("task_id")
        if not isinstance(task_id, str) or not task_id:
            raise PangramError("Submission response omitted task_id; do not automatically resubmit")
        return {
            "detector": "pangram", "model": model, "task_id": task_id,
            "submitted_text": text, "sha256": text_hash(text),
            "submitted_at": submitted_at, "submission_response": response,
        }

    def poll(self, task_id: str, timeout: float = 600, interval: float = 2) -> dict:
        """Poll an existing task; failures preserve its ID for later inspection."""
        if not isinstance(task_id, str) or not task_id:
            raise ValueError("task_id must be a nonempty string")
        timeout = _positive(timeout, "timeout")
        interval = _positive(interval, "interval")
        deadline = time.monotonic() + timeout
        last_response = None
        while True:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise PangramTimeout(task_id, last_response)
            last_response = self._request(
                "/task/" + urllib.parse.quote(task_id, safe=""),
                timeout=min(self.request_timeout, remaining),
            )
            returned_id = last_response.get("task_id")
            if returned_id is not None and returned_id != task_id:
                raise PangramError("Pangram returned a different task ID")
            stage = last_response.get("stage")
            if stage == "STAGE_SUCCESS":
                return last_response
            if stage == "STAGE_FAILED":
                raise PangramTaskFailed(task_id, last_response)
            if not isinstance(stage, str) or not stage.startswith("STAGE_"):
                raise PangramError("Pangram returned an invalid task stage")
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise PangramTimeout(task_id, last_response)
            time.sleep(min(interval, remaining))


def _fraction(result: dict, key: str) -> float:
    value = result.get(key)
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        raise ValueError(f"{key} must be a finite number in [0, 1]")
    if not math.isfinite(value) or not 0 <= value <= 1:
        raise ValueError(f"{key} must be a finite number in [0, 1]")
    return float(value)


def _validate_record(record: dict) -> tuple[tuple[str, str, str, str], dict]:
    text = record.get("submitted_text")
    if not isinstance(text, str) or not text:
        raise ValueError("Each run needs nonempty submitted_text")
    digest = text_hash(text)
    if record.get("sha256", digest) != digest:
        raise ValueError("Stored sha256 does not match submitted_text")
    result = record.get("result")
    if not isinstance(result, dict) or result.get("stage") != "STAGE_SUCCESS":
        raise ValueError("Only completed successful runs can be summarized")
    detector = record.get("detector", "pangram")
    model = record.get("model")
    version = result.get("version")
    task_id = record.get("task_id")
    if not all(isinstance(value, str) and value for value in
               (detector, model, version, task_id)):
        raise ValueError("Each run needs detector, model, response version, and task_id")
    if result.get("task_id", task_id) != task_id:
        raise ValueError("Result task_id does not match submitted task_id")
    ai = _fraction(result, "fraction_ai")
    assisted = _fraction(result, "fraction_ai_assisted")
    human = _fraction(result, "fraction_human")
    # Real API fractions are float32; tolerate rounding but reject broken data.
    if not math.isclose(ai + assisted + human, 1.0, rel_tol=0, abs_tol=1e-6):
        raise ValueError("The three fractions must sum to 1 within rounding tolerance")
    return (detector, model, version, digest), {
        "task_id": task_id, "ai": ai, "assisted": assisted,
        "ai_plus_assisted": min(1.0, ai + assisted), "human": human,
        "full_document": record.get("full_document") is True,
        "returned_text_differs": isinstance(result.get("text"), str)
        and result["text"] != text,
    }


def _settings(threshold: float, min_repeats: int) -> None:
    if isinstance(threshold, bool) or not isinstance(threshold, (int, float)):
        raise ValueError("threshold must be finite and in (0, 1]")
    if not math.isfinite(threshold) or not 0 < threshold <= 1:
        raise ValueError("threshold must be finite and in (0, 1]")
    if isinstance(min_repeats, bool) or not isinstance(min_repeats, int) or min_repeats < 1:
        raise ValueError("min_repeats must be a positive integer")


def _stats(values: list[float]) -> dict:
    return {"min": min(values), "median": statistics.median(values),
            "mean": statistics.mean(values), "max": max(values),
            "values": values}


def summarize_runs(records: list[dict], threshold: float = .1,
                   min_repeats: int = 3) -> dict:
    """Group exact-input repeats; never count a repeated task result twice.

    ``observed_success`` requires every unique observation in a group to have
    AI + assisted strictly below the threshold, with enough distinct tasks.
    This is evidence about those observations, not population reliability.
    """
    _settings(threshold, min_repeats)
    grouped: dict[tuple, list] = defaultdict(list)
    seen: dict[tuple, tuple] = {}
    duplicates = 0
    for record in records:
        key, observation = _validate_record(record)
        task_key = (key[0], observation["task_id"])
        if task_key in seen:
            if seen[task_key] != (key, observation):
                raise ValueError("The same task ID has conflicting observations")
            duplicates += 1
            continue
        seen[task_key] = (key, observation)
        grouped[key].append(observation)
    groups = []
    for (detector, model, version, digest), observations in sorted(grouped.items()):
        enough = len(observations) >= min_repeats
        ai = [r["ai"] for r in observations]
        combined = [r["ai_plus_assisted"] for r in observations]
        groups.append({
            "detector": detector, "model": model, "version": version,
            "sha256": digest, "repeat_count": len(observations),
            "ai_fraction": _stats(ai), "ai_plus_assisted_fraction": _stats(combined),
            "sufficient_repeats": enough,
            "ai_only_observed_success": enough and all(value < threshold for value in ai),
            "observed_success": enough and all(value < threshold for value in combined),
            "full_document": all(r["full_document"] for r in observations),
            "normalization_observed": any(r["returned_text_differs"] for r in observations),
            "task_ids": [r["task_id"] for r in observations],
        })
    return {
        "threshold": threshold, "comparison": "strictly_less_than",
        "min_repeats": min_repeats, "record_count": len(records),
        "unique_runs": len(seen), "duplicate_records_ignored": duplicates,
        "groups": groups,
        "all_groups_observed_success": bool(groups) and all(g["observed_success"] for g in groups),
        "reliability_established": False,
        "interpretation": (
            "These are repeated observations of particular inputs. They do not establish "
            "reliability on unseen documents or determine a text's authorship."
        ),
    }


def paired_study(baseline_runs: list[dict], candidate_runs: list[dict],
                 quality_audit: dict[str, Any], threshold: float = .1,
                 min_repeats: int = 3) -> dict:
    """Compare ordered repeat pairs for one full document per detector/model.

    Callers supply temporally matched baseline/candidate observations in the
    same order. Pairing by list position is explicit and cannot be reconstructed
    from independently selected best scores. Quality flags are human attestations;
    this function cannot verify the reviewer's identity or the review itself.
    """
    if not baseline_runs or len(baseline_runs) != len(candidate_runs):
        raise ValueError("Provide equally sized, nonempty lists of matched repeat pairs")
    if not isinstance(quality_audit, dict):
        raise ValueError("quality_audit must be an object of human review attestations")
    baseline = summarize_runs(baseline_runs, threshold, min_repeats)
    candidate = summarize_runs(candidate_runs, threshold, min_repeats)
    if baseline["duplicate_records_ignored"] or candidate["duplicate_records_ignored"]:
        raise ValueError("Paired studies require distinct submitted task IDs in each arm")
    if any(record.get("full_document") is not True for record in baseline_runs + candidate_runs):
        raise ValueError("Every paired run must explicitly have full_document=true")
    hashes_by_arm: list[set] = []
    for summary in (baseline, candidate):
        hashes_by_arm.append({g["sha256"] for g in summary["groups"]})
    if any(len(hashes) != 1 for hashes in hashes_by_arm):
        raise ValueError("Each arm must contain repeated observations of one exact document")
    if hashes_by_arm[0] == hashes_by_arm[1]:
        raise ValueError("Baseline and candidate must be different documents")
    deltas: dict[tuple, list] = defaultdict(list)
    for original, edited in zip(baseline_runs, candidate_runs):
        base_key, base_obs = _validate_record(original)
        edit_key, edit_obs = _validate_record(edited)
        if base_key[:3] != edit_key[:3]:
            raise ValueError("Each pair must use the same detector, model selector, and version")
        if base_obs["task_id"] == edit_obs["task_id"]:
            raise ValueError("Baseline and candidate cannot share a submitted task")
        deltas[base_key[:3]].append(edit_obs["ai_plus_assisted"] - base_obs["ai_plus_assisted"])
    reviewed = quality_audit.get("reviewed_by_human") is True
    quality_passed = reviewed and all(quality_audit.get(k) is True for k in QUALITY_DIMENSIONS)
    sufficient = all(g["sufficient_repeats"] for s in (baseline, candidate) for g in s["groups"])
    return {
        "baseline": baseline, "candidate": candidate,
        "paired_deltas": [
            {"detector": key[0], "model": key[1], "version": key[2],
             "candidate_minus_baseline_ai_plus_assisted": _stats(values)}
            for key, values in sorted(deltas.items())
        ],
        "quality_audit": dict(quality_audit), "quality_passed": quality_passed,
        "sufficient_repeats": sufficient,
        "observed_success": quality_passed and sufficient
        and candidate["all_groups_observed_success"],
        "reliability_established": False,
        "interpretation": (
            "Passing this document requires all quality checks and every repeated candidate "
            "score below threshold. Estimate general reliability separately on untouched "
            "source-group holdouts across domains, generators, and detectors."
        ),
    }
