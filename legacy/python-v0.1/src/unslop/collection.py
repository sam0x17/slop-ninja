"""Collect attributable prose through explicit, single-request provider calls.

Only complete assistant prose enters a corpus. Failed or refused generations are
available as CollectionError.record for a caller's separate attempt log.
"""

from __future__ import annotations

import hashlib
import json
import math
import os
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Mapping
from urllib.error import HTTPError, URLError
from urllib.request import HTTPRedirectHandler, Request, build_opener


class CollectionError(RuntimeError):
    """A request failed or produced an unsuitable corpus sample."""

    def __init__(self, message: str, record: dict[str, Any] | None = None):
        super().__init__(message)
        self.record = record or {}


class _NoRedirect(HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


_opener = build_opener(_NoRedirect())
_PROVIDERS = {
    "openai": ("https://api.openai.com/v1/responses", "OPENAI_API_KEY"),
    "anthropic": ("https://api.anthropic.com/v1/messages", "ANTHROPIC_API_KEY"),
}
_PARAMETERS = {
    "openai": {"temperature", "top_p", "reasoning", "text", "instructions", "service_tier"},
    "anthropic": {"temperature", "top_p", "top_k", "thinking", "output_config", "system", "service_tier"},
}
_REQUIRED_PROMPT_FIELDS = ("id", "prompt", "domain", "register", "group_id")


def _validate_prompt(prompt: Mapping[str, Any]) -> None:
    if not isinstance(prompt, Mapping):
        raise ValueError("Each prompt must be a JSON object")
    for field in _REQUIRED_PROMPT_FIELDS:
        if not isinstance(prompt.get(field), str) or not prompt[field].strip():
            raise ValueError(f"Prompt requires a nonempty string: {field}")
    if prompt.get("split", "train") not in {"train", "dev", "test", "exploratory"}:
        raise ValueError("Prompt split must be train, dev, test, or exploratory")


def load_prompts(path: str | Path) -> list[dict[str, Any]]:
    """Validate the entire manifest before a caller starts paid generation."""
    prompts: list[dict[str, Any]] = []
    seen: set[str] = set()
    groups: dict[str, str] = {}
    with Path(path).open(encoding="utf-8") as handle:
        for line_number, line in enumerate(handle, 1):
            if not line.strip():
                continue
            try:
                prompt = json.loads(line)
                _validate_prompt(prompt)
                if prompt["id"] in seen:
                    raise ValueError(f"Duplicate prompt id: {prompt['id']}")
                split = prompt.get("split", "train")
                if groups.get(prompt["group_id"], split) != split:
                    raise ValueError("One source group cannot occur in different splits")
            except (ValueError, TypeError) as error:
                raise ValueError(f"{path}:{line_number}: {error}") from error
            seen.add(prompt["id"])
            groups[prompt["group_id"]] = split
            prompts.append(prompt)
    if not prompts:
        raise ValueError("Prompt manifest is empty")
    return prompts


def _post_json(url: str, payload: dict[str, Any], headers: dict[str, str], timeout: float):
    """Issue exactly one POST; do not follow redirects or automatically retry."""
    request = Request(url, data=json.dumps(payload, allow_nan=False).encode("utf-8"),
                      headers=headers, method="POST")
    try:
        with _opener.open(request, timeout=timeout) as response:
            body = json.load(response)
            response_headers = {key.lower(): value for key, value in response.headers.items()}
    except HTTPError as error:
        # Error bodies can echo request contents. Keep them out of normal logs.
        request_id = error.headers.get("x-request-id") or error.headers.get("request-id")
        error.close()
        raise CollectionError(f"Provider HTTP {error.code}; request was not retried",
                              {"http_status": error.code, "request_id": request_id}) from None
    except (URLError, TimeoutError, OSError) as error:
        raise CollectionError(f"Provider connection failed ({type(error).__name__}); "
                              "request may have been charged and was not retried") from None
    except (ValueError, UnicodeError):
        raise CollectionError("Provider returned invalid JSON; request was not retried") from None
    if not isinstance(body, dict):
        raise CollectionError("Provider response must be a JSON object")
    return body, response_headers


def _openai_text(response: dict[str, Any]) -> str:
    if response.get("error") or response.get("status") != "completed":
        raise CollectionError("OpenAI response did not complete")
    blocks: list[str] = []
    for item in response.get("output", []):
        if item.get("type") == "reasoning":
            continue
        if item.get("type") != "message" or item.get("role") != "assistant":
            raise CollectionError("OpenAI response contains a non-prose output item")
        if item.get("status") != "completed":
            raise CollectionError("OpenAI assistant message did not complete")
        for part in item.get("content", []):
            if part.get("type") == "refusal":
                raise CollectionError("OpenAI refused this prompt")
            if part.get("type") != "output_text" or not isinstance(part.get("text"), str):
                raise CollectionError("OpenAI response contains a non-text content block")
            blocks.append(part["text"])
    return "".join(blocks)


def _anthropic_text(response: dict[str, Any]) -> str:
    if response.get("stop_reason") != "end_turn":
        raise CollectionError("Anthropic response was refused, interrupted, or truncated")
    if response.get("type") != "message" or response.get("role") != "assistant":
        raise CollectionError("Anthropic response is not an assistant message")
    if (response.get("stop_details") or {}).get("type") == "refusal":
        raise CollectionError("Anthropic refused this prompt")
    blocks: list[str] = []
    for part in response.get("content", []):
        if part.get("type") in {"thinking", "redacted_thinking"}:
            continue
        if part.get("type") != "text" or not isinstance(part.get("text"), str):
            raise CollectionError("Anthropic response contains a non-text content block")
        blocks.append(part["text"])
    return "".join(blocks)


def collect_one(
    prompt: Mapping[str, Any], provider: str, model: str, corpus: str, *,
    max_output_tokens: int = 2048, parameters: Mapping[str, Any] | None = None,
    timeout: float = 120.0, api_key: str | None = None,
) -> dict[str, Any]:
    """Collect one sample, preserving prompt, model provenance, and exact output.

    ``parameters`` contains only provider generation controls from _PARAMETERS.
    Model IDs have no default. A product name such as Codex is not provenance for
    a particular underlying model. Caller controls attempt count and persistence.
    """
    _validate_prompt(prompt)
    if provider not in _PROVIDERS:
        raise ValueError("Provider must be openai or anthropic")
    if not isinstance(model, str) or not model.strip():
        raise ValueError("An explicit provider model ID is required")
    if not isinstance(corpus, str) or not corpus.strip():
        raise ValueError("A corpus name is required")
    if type(max_output_tokens) is not int or max_output_tokens <= 0:
        raise ValueError("max_output_tokens must be a positive integer")
    if not isinstance(timeout, (int, float)) or not math.isfinite(timeout) or timeout <= 0:
        raise ValueError("timeout must be finite and positive")
    if parameters is not None and not isinstance(parameters, Mapping):
        raise ValueError("parameters must be a mapping")
    params = dict(parameters or {})
    unknown = params.keys() - _PARAMETERS[provider]
    if unknown:
        raise ValueError("Unsupported generation parameters: " + ", ".join(sorted(unknown)))
    # Validate serializability before making a request, and detach caller objects.
    params = json.loads(json.dumps(params, allow_nan=False))
    prompt_record = json.loads(json.dumps(dict(prompt), allow_nan=False))
    endpoint, env_key = _PROVIDERS[provider]
    credential = api_key if api_key is not None else os.environ.get(env_key)
    if not credential or not credential.strip():
        raise CollectionError(f"Set {env_key} before collecting samples")
    headers = {"Content-Type": "application/json", "Authorization": f"Bearer {credential}",
               "User-Agent": "unslop/0.1"}
    if provider == "openai":
        payload = {"model": model, "input": prompt["prompt"], "store": False,
                   "max_output_tokens": max_output_tokens, **params}
    else:
        headers["anthropic-version"] = "2023-06-01"
        workspace_id = os.environ.get("ANTHROPIC_WORKSPACE_ID")
        if workspace_id:
            headers["anthropic-workspace-id"] = workspace_id
        payload = {"model": model, "messages": [{"role": "user", "content": prompt["prompt"]}],
                   "max_tokens": max_output_tokens, **params}
    metadata: dict[str, Any] = {
        "collector_version": "1", "endpoint": endpoint,
        "provider": provider, "model_requested": model, "prompt_id": prompt["id"],
        "prompt": prompt["prompt"], "prompt_record": prompt_record,
        "prompt_sha256": hashlib.sha256(prompt["prompt"].encode("utf-8")).hexdigest(),
        "request_parameters": payload,
        "requested_at": datetime.now(timezone.utc).isoformat(),
    }
    try:
        response, response_headers = _post_json(endpoint, payload, headers, timeout)
    except CollectionError as error:
        error.record = {**metadata, **error.record, "collection_status": "request_failed"}
        raise
    metadata.update({
        "received_at": datetime.now(timezone.utc).isoformat(),
        "request_id": response_headers.get("x-request-id") or response_headers.get("request-id"),
        "response_id": response.get("id"), "response_created_at": response.get("created_at"),
        "usage": response.get("usage"), "response": response,
    })
    try:
        if not isinstance(response.get("model"), str) or not response["model"].strip():
            raise CollectionError("Provider did not report the model used")
        if not isinstance(response.get("id"), str) or not response["id"].strip():
            raise CollectionError("Provider did not report a response ID")
        text = _openai_text(response) if provider == "openai" else _anthropic_text(response)
        if not text.strip():
            raise CollectionError("Provider returned no assistant prose")
    except (CollectionError, AttributeError, TypeError) as error:
        message = str(error) if isinstance(error, CollectionError) else "Malformed provider output"
        raise CollectionError(message, {**metadata, "collection_status": "rejected"}) from None
    metadata["collection_status"] = "accepted"
    metadata["text_sha256"] = hashlib.sha256(text.encode("utf-8")).hexdigest()
    return {
        "text": text, "corpus": corpus, "source_kind": "model", "provider": provider,
        "model": response["model"], "model_requested": model,
        "domain": prompt["domain"], "register": prompt["register"],
        "group_id": prompt["group_id"], "split": prompt.get("split", "train"),
        "metadata": metadata,
    }
