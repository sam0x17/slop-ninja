"""spaCy-dependent grammatical annotations for the Rust feature extractor.

Rust owns lexical tokenization, SQLite, statistics, and orchestration. This
module only loads the trained parser and derives features from its annotations.
Parser API: https://spacy.io/usage/linguistic-features
"""

from __future__ import annotations

from collections import Counter
from functools import lru_cache
import unicodedata
from typing import Any


_APOSTROPHES = str.maketrans({"\u2018": "'", "\u2019": "'", "\u02bc": "'", "\uff07": "'"})
_FUNCTION_POS = frozenset({"ADP", "AUX", "CCONJ", "DET", "PART", "PRON", "SCONJ"})
_SUBJECT_DEPS = frozenset({"nsubj", "nsubjpass", "nsubj:pass", "csubj", "csubjpass", "csubj:pass"})
_PASSIVE_DEPS = frozenset({"nsubjpass", "csubjpass", "auxpass", "nsubj:pass", "csubj:pass", "aux:pass"})
_FIRST_PERSON = frozenset({"i", "me", "my", "mine", "myself", "we", "us", "our", "ours", "ourselves"})


def _normalized(text: str) -> str:
    return unicodedata.normalize("NFC", text).translate(_APOSTROPHES)






def _ngrams(sequences: list[list[str]], size: int) -> Counter[str]:
    counts: Counter[str] = Counter()
    for sequence in sequences:
        counts.update(" ".join(sequence[index:index + size]) for index in range(len(sequence) - size + 1))
    return counts




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
