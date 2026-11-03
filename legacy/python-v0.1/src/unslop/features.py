"""Versioned, countable lexical and grammatical features for corpus comparison.

These are observable text features, not a detector's hidden embeddings. Sentence
length and vocabulary diversity are descriptive proxies, not quality scores.

Denominators (``totals``) are opportunities for each family:

* ``word`` counts Unicode words after NFC, apostrophe normalization, and lower-
  casing. Internal apostrophes remain, so ``don't`` is one lexical word. Digits,
  underscores, and hyphens separate words. Combining marks remain attached.
* ``word_bigram`` and ``word_trigram`` count adjacent lexical words within each
  heuristic sentence; their totals are the number of corresponding ngrams.
  The heuristic splits at terminal punctuation followed by whitespace/end, or
  blank lines. It does not resolve abbreviations or quotations linguistically.
* ``sentence_length`` counts heuristic sentences containing at least one word.
* Optional ``lemma``, ``pos``, and ``dependency`` count spaCy tokens containing a
  letter. These token counts can differ from ``word``, notably for contractions.
  Dependency features are ``HEAD_POS|relation|CHILD_POS``; roots use ``ROOT`` as
  the head. POS ngrams and ``function_pattern`` stay within parsed sentences.
  Function patterns are trigrams: function words remain lexical, while other
  tokens become their POS tags. All these totals equal their summed counts.
* ``construction`` counts sentences exhibiting a named parse pattern, at most
  once per sentence per pattern. Its denominator is parsed nonempty sentences.
  Patterns may co-occur, so their counts MUST NOT be summed as a denominator.

Grammar rules support English spaCy pipelines, with dependency-label variants
for spaCy and Universal Dependencies. They describe the parser's analysis and
can be wrong. Noun subjects do not imply inanimate referents or poor writing.
No grammar features are fabricated when a model is unavailable.

Parser API references: https://spacy.io/api/token,
https://spacy.io/api/doc, https://spacy.io/usage/linguistic-features.
"""

from __future__ import annotations

from collections import Counter
from functools import lru_cache
import math
import re
import statistics
import unicodedata
from typing import Any


LEXICAL_EXTRACTOR = (
    "unslop-features-v1;nfc-unicode-word-v1;sentence-heuristic-v1"
    f";unicode={unicodedata.unidata_version}"
)
_APOSTROPHES = str.maketrans({"\u2018": "'", "\u2019": "'", "\u02bc": "'", "\uff07": "'"})
_SENTENCE_BOUNDARY = re.compile(r"[.!?]+[\"'\u201d\u2019)\]]*(?:\s+|$)|\n\s*\n")
_FUNCTION_POS = frozenset({"ADP", "AUX", "CCONJ", "DET", "PART", "PRON", "SCONJ"})
_SUBJECT_DEPS = frozenset({"nsubj", "nsubjpass", "nsubj:pass", "csubj", "csubjpass", "csubj:pass"})
_PASSIVE_DEPS = frozenset({"nsubjpass", "csubjpass", "auxpass", "nsubj:pass", "csubj:pass", "aux:pass"})
_FIRST_PERSON = frozenset({"i", "me", "my", "mine", "myself", "we", "us", "our", "ours", "ourselves"})


def _normalized(text: str) -> str:
    return unicodedata.normalize("NFC", text).translate(_APOSTROPHES)


def words(text: str) -> list[str]:
    """Return the deterministic lexical tokens used by every extraction mode.

    A word starts with an alphabetic Unicode code point; combining marks can
    follow it. An apostrophe is internal only when followed by another letter.
    Lowercasing, unlike casefolding, does not turn German ``ß`` into ``ss``.
    """
    text = _normalized(text)
    tokens: list[str] = []
    current: list[str] = []
    for index, char in enumerate(text):
        is_mark = unicodedata.category(char).startswith("M")
        internal_apostrophe = char == "'" and bool(current) and index + 1 < len(text) and text[index + 1].isalpha()
        if char.isalpha() or (is_mark and current) or internal_apostrophe:
            current.append(char)
        elif current:
            tokens.append(unicodedata.normalize("NFC", "".join(current).lower()))
            current = []
    if current:
        tokens.append(unicodedata.normalize("NFC", "".join(current).lower()))
    return tokens


def _sentences(text: str) -> list[list[str]]:
    return [tokens for part in _SENTENCE_BOUNDARY.split(text) if (tokens := words(part))]


def _ngrams(sequences: list[list[str]], size: int) -> Counter[str]:
    counts: Counter[str] = Counter()
    for sequence in sequences:
        counts.update(" ".join(sequence[index:index + size]) for index in range(len(sequence) - size + 1))
    return counts


def _length_bucket(length: int) -> str:
    if length < 10:
        return "01-09"
    if length < 20:
        return "10-19"
    if length < 30:
        return "20-29"
    if length < 40:
        return "30-39"
    return "40+"


def _ratio(numerator: float, denominator: int) -> float:
    return numerator / denominator if denominator else 0.0


@lru_cache(maxsize=4)
def _load_parser(model: str) -> tuple[Any, str]:
    """Load lazily so the lexical profiler needs only the standard library."""
    try:
        import spacy
    except ImportError as exc:
        raise RuntimeError(
            "Grammar extraction requires spaCy and an English model. Install "
            "the grammar extra, then run: python -m spacy download en_core_web_sm"
        ) from exc
    try:
        nlp = spacy.load(model, exclude=["ner"])
    except (OSError, ImportError) as exc:
        raise RuntimeError(
            f"Cannot load spaCy model {model!r}. Install that model with "
            "python -m spacy download en_core_web_sm (or supply an installed model)."
        ) from exc
    if nlp.lang != "en":
        raise ValueError("Grammar rules v1 require an English spaCy model.")
    if "parser" not in nlp.pipe_names or "lemmatizer" not in nlp.pipe_names:
        raise ValueError("Grammar extraction requires an enabled dependency parser and lemmatizer.")
    model_name = nlp.meta.get("name", model)
    model_version = nlp.meta.get("version", "unknown")
    if model_version == "unknown":
        raise ValueError("The spaCy model must declare a version for reproducible features.")
    identity = f"spacy={spacy.__version__};model={nlp.lang}_{model_name}@{model_version};grammar-rules-v1"
    return nlp, identity


def _construction_flags(sentence: list[Any]) -> set[str]:
    """Binary sentence patterns; no inference about animacy, intent, or quality."""
    deps = {token.dep_ for token in sentence}
    flags: set[str] = set()
    rules = {
        "passive_sentence": bool(deps & _PASSIVE_DEPS) or any("Pass" in token.morph.get("Voice") for token in sentence),
        "relative_clause_sentence": bool(deps & {"relcl", "acl:relcl"}),
        "adverbial_clause_sentence": "advcl" in deps,
        "complement_clause_sentence": bool(deps & {"ccomp", "xcomp"}),
        "coordination_sentence": bool(deps & {"cc", "conj"}),
        "apposition_sentence": "appos" in deps,
        "negation_sentence": "neg" in deps or any("Neg" in token.morph.get("Polarity") for token in sentence),
        "noun_subject_sentence": any(token.dep_ in _SUBJECT_DEPS and token.pos_ in {"NOUN", "PROPN"} for token in sentence),
        "pronoun_subject_sentence": any(token.dep_ in _SUBJECT_DEPS and token.pos_ == "PRON" for token in sentence),
        "existential_there_sentence": any(token.dep_ == "expl" and token.lower_ == "there" for token in sentence),
        "first_person_reference_sentence": any(token.pos_ in {"PRON", "DET"} and token.lower_ in _FIRST_PERSON for token in sentence),
        "copular_sentence": "cop" in deps or any(
            token.lemma_ == "be" and any(child.dep_ in {"attr", "acomp", "oprd"} for child in token.children)
            for token in sentence
        ),
    }
    flags.update(name for name, present in rules.items() if present)
    return flags


def _grammar(text: str, model: str) -> tuple[str, dict[str, Counter[str]], dict[str, int], dict[str, int | float]]:
    nlp, identity = _load_parser(model)
    doc = nlp(_normalized(text))
    if len(doc) and any(not doc.has_annotation(attribute) for attribute in ("DEP", "POS", "LEMMA", "SENT_START")):
        raise RuntimeError("The spaCy pipeline did not produce all required dependency, POS, lemma, and sentence annotations.")
    sentences = [list(sentence) for sentence in doc.sents]
    sentences = [sentence for sentence in sentences if any(any(char.isalpha() for char in token.text) for token in sentence)]
    lexical_sentences = [[token for token in sentence if any(char.isalpha() for char in token.text)] for sentence in sentences]
    lexical_tokens = [token for sentence in lexical_sentences for token in sentence]
    pos_sequences = [[token.pos_ for token in sentence] for sentence in lexical_sentences]
    function_sequences = [
        ["word:" + token.lower_ if token.pos_ in _FUNCTION_POS else "pos:" + token.pos_ for token in sentence]
        for sentence in lexical_sentences
    ]
    families: dict[str, Counter[str]] = {
        "lemma": Counter(unicodedata.normalize("NFC", token.lemma_.lower()) for token in lexical_tokens),
        "pos": Counter(token.pos_ for token in lexical_tokens),
        "dependency": Counter(
            f"{'ROOT' if token.head.i == token.i else token.head.pos_}|{token.dep_}|{token.pos_}"
            for token in lexical_tokens
        ),
        "pos_bigram": _ngrams(pos_sequences, 2),
        "pos_trigram": _ngrams(pos_sequences, 3),
        "function_pattern": _ngrams(function_sequences, 3),
        "construction": Counter(),
    }
    for sentence in sentences:
        families["construction"].update(_construction_flags(sentence))
    totals = {name: sum(counts.values()) for name, counts in families.items()}
    totals["construction"] = len(sentences)
    distances = [abs(token.i - token.head.i) for token in lexical_tokens if token.head.i != token.i]
    metrics: dict[str, int | float] = {
        "parsed_sentence_count": len(sentences),
        "parsed_token_count": len(lexical_tokens),
        "parsed_mean_sentence_tokens": _ratio(len(lexical_tokens), len(sentences)),
        "parsed_mean_dependency_distance_tokens": _ratio(sum(distances), len(distances)),
    }
    return identity, families, totals, metrics


def extract(text: str, grammar: bool = False, model: str = "en_core_web_sm") -> dict[str, Any]:
    """Extract JSON-serializable counts, opportunity totals, and numeric metrics.

    Empty/punctuation-only input has zero lexical opportunities, no features,
    and finite zero-valued lexical metrics. Grammar mode still requires its
    declared dependencies for empty input. ``extractor`` identifies the rules
    and, when enabled, both spaCy and model versions. Do not pool different
    extractor identities without explicitly rebuilding compatible features.
    """
    if not isinstance(text, str):
        raise TypeError("text must be a string")
    tokens = words(text)
    sentences = _sentences(text)
    lengths = [len(sentence) for sentence in sentences]
    counts = Counter(tokens)
    families: dict[str, Counter[str]] = {
        "word": counts,
        "word_bigram": _ngrams(sentences, 2),
        "word_trigram": _ngrams(sentences, 3),
        "sentence_length": Counter(_length_bucket(length) for length in lengths),
    }
    totals = {name: sum(features.values()) for name, features in families.items()}
    word_lengths = [sum(char.isalpha() for char in token) for token in tokens]
    metrics: dict[str, int | float] = {
        "word_count": len(tokens),
        "distinct_word_count": len(counts),
        "type_token_ratio": _ratio(len(counts), len(tokens)),
        "mean_word_letters": _ratio(sum(word_lengths), len(tokens)),
        "long_word_fraction_ge7": _ratio(sum(length >= 7 for length in word_lengths), len(tokens)),
        "heuristic_sentence_count": len(sentences),
        "heuristic_mean_sentence_words": _ratio(sum(lengths), len(sentences)),
        "heuristic_sentence_words_stdev": statistics.pstdev(lengths) if lengths else 0.0,
        "heuristic_sentence_words_p90": sorted(lengths)[math.ceil(0.9 * len(lengths)) - 1] if lengths else 0,
        "paragraph_count": sum(bool(words(part)) for part in re.split(r"\n\s*\n", text)),
    }
    identity = LEXICAL_EXTRACTOR
    if grammar:
        grammar_identity, grammar_families, grammar_totals, grammar_metrics = _grammar(text, model)
        identity += ";" + grammar_identity
        families.update(grammar_families)
        totals.update(grammar_totals)
        metrics.update(grammar_metrics)
    return {
        "extractor": identity,
        "families": {name: dict(sorted(features.items())) for name, features in families.items()},
        "totals": totals,
        "metrics": metrics,
    }
