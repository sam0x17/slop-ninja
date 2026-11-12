# Evidence behind the detection page

SlopNinja's free detection page, planned at `slop.ninja/detect/`, scores one
pasted English text with *A*, SlopNinja's AI-text detector. For each text of
150 words or more the page shows:

- an **evidence band** (very strong, strong, unclear, or weak or none);
- *A*'s two document percentages: the **confidence** that AI was involved at
  all, a probability from a calibration map fitted for a mix in which about
  two in three documents involve AI (see "The base rate behind the
  confidence"), and the **estimated AI share**, the expected fraction of the
  final text's words in passages an AI model wrote or rewrote;
- in the strong and very strong bands only, **highlighted passages**: runs of
  24 words or more that *A* estimates AI wrote, each with its own confidence.

For a text of 24 to 149 words the page shows one result instead: **strong
signs of AI involvement**, with its highlighted passages, or **no strong
signs**. It shows no percentage, no share and no very strong band at these
lengths (see "Short texts").

The page scores English text of 24 words or more and withholds results for
text that fails its English screen. It scores at most the first 1,000 words
of a text: a longer text is cut back to the last sentence end or paragraph
break inside its first 1,000 words, never inside a sentence, and further if
dense text would exceed the 2,048 tokens *A* reads at once; the page says how
many of the text's words it scored. The four bands' human false-positive
rates were measured at 150-450 words and, for texts that end at a sentence
end, at 451-1,000 words, where a small AI-edited part is caught less often
(see "Long texts").

This document gives the measured rates that the page's copy cites. All figures
are aggregates, computed in November 2026 (the Common Crawl pilot's scores date
from October 30, 2026); per-document outputs are not published.

**Which model.** R0 is the released version of *A* ([baseline release](a-baseline-release.md)):
one training run (seed 17) with its calibration. The page serves SlopNinja's
first span package, span package d24 ([reference packages](testnet/reference-packages.md)),
which is R0 unchanged plus a component that finds AI-written passages (a span
head). Every document figure here is therefore R0's. The passage figures are
the package's under its 24-word floor: AI passages shorter than 24 words are
left out of passage scoring, and no highlighted passage is shorter than 24
words. Such short AI edits still count toward the share and the confidence.

## Populations

- **The sealed Test**, opened on October 30, 2026 and therefore consumed: these
  are retrospective statistics, not fresh held-out evidence. R0, its
  calibration map and its operating points were fixed before the Test was
  opened; the breakdowns here were chosen and computed after that, with the
  Test's results already known. R0 was trained on other documents from other
  source groups (no source group spans two splits;
  [competitive protocol](a-competitive-protocol.md), "Sources and source
  groups"). Its training data ([baseline release](a-baseline-release.md),
  "Training data") includes seven of the eight open-weight families below and
  hosted GPT and Claude generations; OLMo-2, about a third of the primary AI
  text and the base model of the robustness set, is not in it. The span
  package's decoder (the rule that turns per-word AI probabilities into
  highlighted passages) and its passage confidence map were frozen before its
  own evaluation read any held-out output, but after the Test had been opened,
  so the passage figures are retrospective too. The page's own
  rules (the length bins, the short-text rule, highlights only in the strong
  bands) were chosen afterwards, with these figures in view.
  - The primary population has 3,286 human documents, 3,256 mixed documents
    (human writing with passages an AI model wrote or rewrote) and 3,127
    model-only documents (wholly AI-written). Its AI text comes from eight
    open-weight model families run locally: OLMo-2 (about a third of it), Qwen 3.6, Granite, Mistral, Gemma 3,
    SmolLM3, ERNIE 4.5 and Phi-4.
  - 826 documents come from twelve hosted frontier model versions of two
    providers (420 mixed, 406 model-only;
    [frontier-model positives](a-frontier-positives.md)). Across the primary
    and frontier mixed documents, the true AI share has a median of 68%
    (interquartile range 37-90%).
  - 1,563 robustness documents are mixed documents in which a base model (a
    pretrained OLMo-2 model without instruction tuning, unlike the chat models
    above) continues human text or is spliced into it.
  - Two human sets are kept apart from the primary population and reported on
    their own: 1,980 learner essays (ELLIPSE, US grades 8-12) and 866
    transcripts of spoken conversations. ("Humans" in counts below means
    human-written documents.) The primary population's 150
    "student essays" are other essays from the same ELLIPSE corpus.
  - The human documents are public writing dated before November 30, 2022, the
    public release of ChatGPT; the 260 Project Gutenberg texts count as
    historical by their authors' public-domain status rather than by a date.
    Every Test human document is an excerpt of 150-450 whitespace words (median 233; 226 for the primary humans).
    The AI-involved documents run from 85 to 619 words (median 217 for mixed
    and 240 for model-only documents).
- **A report set of 545 spliced rewrites**, not part of the Test: human texts
  in which an AI model rewrote some sentences, used here only for one passage
  result. No model choice used these documents, though their human sources
  were used in development, so they do not share the Test's source separation.
- **The Calibration split (CAL)**: 5,379 documents, 1,140 of them human, kept
  apart from training. The calibration map and the band thresholds were fitted
  on it, so CAL figures are in sample.
- **The Common Crawl human pilot**, run on October 30, 2026
  ([sampler and measurements](../grammar/crates/grammar-cc-human/README.md);
  [whitepaper](../whitepaper/slop_ninja.pdf), Section 10, "Common Crawl human
  pilot"): 4,954 web documents from Common Crawl crawls of 2013 to 2022, before
  the November 30, 2022 cutoff, excerpted within the same 150-450-word bounds
  and scored by R0. The pilot's sample is drawn evenly across web genres
  (news, fiction, email and so on); "weighted to the pool" below reweights it
  to the genre mix of the 416,492 excerpts the pilot admitted.

Intervals, where shown, are Wilson 95% intervals, which treat documents as
independent although they come in source groups (a source group is one website,
book or other source). There is one training run, so run-to-run variation is
not measured, and no bootstrap intervals were computed.

## Evidence bands

The calibration map is an isotonic (step) function from R0's raw score to a
probability, fitted on CAL. It is clipped below at 1/(2 × 1,140) and above at
1 − 1/(2 × 4,239), where 1,140 and 4,239 are CAL's human and AI-involved
document counts, so it never reads 0% or 100%. Near the thresholds the
confidence takes only a few distinct values. The band boundaries are 50% and
the release's two CAL operating points:
0.9090909090909091, the lowest level of the calibration map that flags at most
1% of CAL humans (it flags 8 of 1,140; the next lower level would flag 17),
and 0.9998820476527482, the point for 0.1%, which is the map's top clipped
level (it flags none of the 1,140). A document is in a band when its
confidence is at least the band's lower bound.

| Band | Primary humans (3,286) | AI-involved, primary and frontier (7,209) | Of those, 150-450 words (6,860) | Ratio of rates, AI to human |
|---|---:|---:|---:|---:|
| Very strong: 99.988% or more | 0 (0.00%, interval 0.00-0.12%) | 5,310 (73.7%) | 5,021 (73.2%) | not defined (no human) |
| Strong: 90.9% to below 99.988% | 43 (1.31%) | 899 (12.5%) | 869 (12.7%) | 9.5 |
| Unclear: 50% to below 90.9% | 160 (4.87%) | 461 (6.4%) | 447 (6.5%) | 1.3 |
| Weak or none: below 50% | 3,083 (93.8%) | 539 (7.5%) | 523 (7.6%) | 0.08 |

The ratio uses the 7,209; on the 6,860 AI-involved documents of 150-450 words,
the length range the human rates were measured on, the ratios are 9.7, 1.3 and
0.08. Of the 7,209, 297 are under 150 words and 52 over 450.

The AI column pools two very different kinds of document. Of the 3,676 primary
and frontier mixed documents, 2,690 (73.2%) reach the strong band or above and
1,830 (49.8%) the very strong band; of the 3,533 model-only documents, 3,519
(99.6%) and 3,480 (98.5%). Its totals therefore depend on the Test's near-even
mix of the two.

- **The unclear band carries almost no evidence of AI involvement at the
  Test's mix.** Human and AI-involved documents land there at about the same
  rate, but only because the two kinds of AI document offset: 451 of the 3,676
  mixed documents (12.3%) and 10 of the 3,533 model-only documents (0.3%) land
  there, against 4.87% of humans. An unclear result also makes a wholly
  AI-written text from the tested models unlikely.
- **A weak result does not show that a text is human-written.** The ratio of
  0.08 covers the primary and frontier documents only. Mixed documents land in
  the weak band far more often than model-only ones: 535 of 3,676 (14.6%)
  against 4 of 3,533. Of the 1,563 robustness documents, which are not in the
  7,209, 1,376 (88%) score below 50%, close to the human rate, and 43 (2.8%)
  reach the strong band or above.
- **Hosted frontier models.** Of the 826 frontier documents, 677 (82%) reach
  the strong band or above and 553 (67%) the very strong band. Split by kind,
  275 of the 420 mixed (65%) and 402 of the 406 model-only (99%) reach the
  strong band or above.
- **Across all 6,132 Test humans** (primary, learner essays and spoken
  conversations), 49 (0.80%) reach the strong band and none the very strong
  band. The 1,980 learner essays have 5 (0.25%) in the strong band and 25
  (1.26%) at 50% or more; the 866 conversations have 1 in the strong band.

### Strong band or above, by domain

Human documents of the primary population, by source domain. The counts are
small, so the order is only a rough guide:

| Domain | Strong band or above |
|---|---:|
| Wikisource | 5 of 150 (3.3%) |
| FineWeb web text | 7 of 220 (3.2%) |
| Meeting transcripts | 4 of 150 (2.7%) |
| Global Voices | 5 of 213 (2.3%) |
| FineWeb blogs | 3 of 150 (2.0%) |
| Peer review | 3 of 150 (2.0%) |
| Wikinews | 3 of 160 (1.9%) |
| PLOS | 4 of 260 (1.5%) |
| Historical manuscripts | 2 of 150 (1.3%) |
| Instructional | 1 of 93 (1.1%) |
| Wikipedia | 3 of 380 (0.8%) |
| Wikipedia talk, Ubuntu IRC | 1 of 150 each (0.7%) |
| Project Gutenberg | 1 of 260 (0.4%) |
| Court opinions, Reddit, student essays (ELLIPSE), Federal Register | 0 of 150-200 each |

Web text runs higher outside the Test as well. In the Common Crawl human pilot,
R0 put 155 of 4,954 documents (3.13%, interval 2.68-3.65%; 2.20% weighted to
the pool) in the strong band or above. Fiction has the highest human-side rate
in this document: 55 of 475 pilot fiction documents (11.6%) in the strong band or
above and 9 (1.9%) in the very strong band, though the Test's Project Gutenberg
books sit at the low end (1 of 260). The pilot's excerpts lie within the same
150-450-word bounds as the Test's, so the difference does not come from texts
outside that range.

## The very strong band outside the Test

No Test human reached the very strong band. In the Common Crawl human pilot, 21
of 4,954 documents did: 0.42% (interval 0.28-0.65%), or 0.31% weighted to the
pool. Nine of the 21 are fiction (of 475 fiction documents). Seven of the 21
were in the pilot's hand-reviewed sample, and all seven were judged
human-written false positives. The other 14 were not reviewed. The pilot's web
pages are not documented as human-written, so some of the 21 may be machine
text; even so, the very strong band is not free of human writing outside the
Test.

## Length

Human false-positive rates in the band table were measured on texts of 150-450
words; "Long texts" below measures 451-1,000 words. Within the band table's
range the strong-band rate is not monotonic in length: 20 of 1,081 (1.85%) at
150-199 words, 15 of 1,846 (0.81%) at 200-299 and 8 of 359 (2.2%) at 300-450.

**Shorter texts.** To measure the human side below 150 words, every Test human
excerpt was re-excerpted from its own start: the leading run of whole units
(sentences, ending at ".", "?" or "!" followed by whitespace, or lines) up to
the first unit boundary at or after 50 words, kept if it has at most 99 words,
and the same at 100 words, kept if it has at most 149. These short excerpts come
from the same source documents as the Test excerpts. A document whose first unit
is already longer than the bin adds nothing to it. Each excerpt was scored with
the same scoring server build and model package the page uses, and counted
only if it passes the page's English screen; 27 of the 50-99-word excerpts and
14 of the 100-149-word ones did not. (Applied to the 6,132 Test excerpts
themselves, the screen would refuse 4; the 150-450 row includes them.)

| Words | Population | Documents | 50% or more | Strong band or above | Very strong | Strong or above with a highlighted passage |
|---|---|---:|---:|---:|---:|---:|
| 50-99 (median 60) | primary humans | 3,164 | 675 (21.3%) | 246 (7.8%, 6.9-8.8%) | 64 (2.0%) | 198 (6.3%) |
| 50-99 | learner essays | 1,847 | 36 | 13 (0.70%) | 2 | 12 |
| 50-99 | spoken conversations | 863 | 121 | 43 (5.0%) | 5 | 34 |
| 100-149 (median 110) | primary humans | 3,156 | 356 (11.3%) | 83 (2.6%, 2.1-3.2%) | 13 (0.41%) | 74 (2.3%) |
| 100-149 | learner essays | 1,856 | 20 | 4 (0.22%) | 0 | 3 |
| 100-149 | spoken conversations | 859 | 42 | 7 (0.81%) | 2 | 7 |
| 150-450 (the Test excerpts) | primary humans | 3,286 | 203 (6.2%) | 43 (1.31%, 0.97-1.76%) | 0 | 39 (1.19%) |

- Below 150 words, primary human texts reach the strong band two to six times
  as often as at 150-450 words, and some reach the very strong band. The
  spoken conversations rise far more (from 1 of 866 at full length to 5.0% at
  50-99 words); the learner essays stay below 1% (0.25% at full length). The page
  therefore does not use the four bands under 150 words; it applies the
  stricter rule of "Short texts" below.
- The short excerpts are leading parts of longer human texts, not texts written
  to be short (emails, posts, short answers), which may behave differently.
- This table measures the human side only. "Short texts" below adds a 24-49-word
  bin and the AI side.
- Longer texts are measured in "Long texts" below.

## Short texts

Below 150 words the four bands do not hold: their cuts flag far more human
text, and the confidence overstates the chance of AI involvement. The page
therefore gives texts of 24 to 149 words one result, under a rule that also
reads the confidence of a highlighted passage. Under 24 words, the shortest
passage the package can highlight, it gives no score.

**How the short texts were made.** Every Test and CAL document, human and
AI-involved, was cut into excerpts by the rule of "Length" above: a run of
whole units from a unit start to the first unit end at or after 24, 50 or 100
words, kept only if it stays inside its bin (24-49, 50-99 or 100-149 words).
Each document gives at most one excerpt per bin from its start; in the
24-49-word bin it also gives one from a second unit start inside the document,
chosen by a fixed hash of the document id. An excerpt's own label comes from
the AI passages that fall inside it: an excerpt of an AI-involved document
that holds no AI-written word counts as human text ("human parts of AI
documents" below). Excerpts that fail the page's English screen were left out.
Every excerpt was scored with the same scoring server build and model package
the page uses. The Test gave 27,471 excerpts at 24-49 words (12,961 from
document starts), 14,514 at 50-99 and 14,516 at 100-149; CAL gave 9,986, 5,286
and 5,254. The 50-99 and 100-149 human excerpts are those of the table in
"Length".

### The four bands below 150 words

| Words | Primary humans: strong or above | Primary humans: very strong | AI-involved, primary and frontier: strong or above | Very strong |
|---|---:|---:|---:|---:|
| 24-49 (median 32) | 854 of 5,909 (14.5%, 13.6-15.4%) | 148 (2.5%) | 9,408 of 11,461 (82.1%) | 6,670 (58.2%) |
| 50-99 (median 60) | 246 of 3,164 (7.8%) | 64 (2.0%) | 5,364 of 6,265 (85.6%) | 4,589 (73.2%) |
| 100-149 (median 110) | 83 of 3,156 (2.6%) | 13 (0.41%) | 5,641 of 6,593 (85.6%) | 4,824 (73.2%) |
| 150-450 (the band table) | 43 of 3,286 (1.31%) | 0 | 6,209 of 7,209 (86.1%) | 5,310 (73.7%) |

At 24-49 words the spoken conversations reach the strong band or above at
15.2% and the learner essays at 1.1%. The confidence is also miscalibrated at
these lengths. Under the balanced weights of "The base rate behind the
confidence", recomputed per bin over primary humans, the human parts of
primary and frontier AI documents and the primary and frontier AI-involved
excerpts, the expected calibration error (10 equal-width bins) is 0.150 at
24-49 words, 0.092 at 50-99 and 0.035 at 100-149, against 0.014 at 150-450.
In the top tenth of the scale the mean confidence is 0.987, 0.993 and 0.994,
while the share of AI-involved excerpts there is 0.872, 0.935 and 0.980 (at
150-450: 0.995 and 0.992). Calibration maps fitted per bin on CAL excerpts
bring the error to 0.018, 0.019 and 0.015, so the gap comes from length; the
page does not use such maps. The estimated share fares worse still: 36% of the
24-49-word primary human excerpts show a share of 25% or more (23% at 50-99,
11% at 100-149, 5.5% at 150-450).

### The short-text rule

A text of 24 to 149 words gets "strong signs of AI involvement" only when both
hold:

- its confidence reaches a level of the release's calibration map, and
- it has a highlighted passage whose own confidence (the package's passage
  confidence map, a separate calibrated output) reaches a level of that map.

| Words | Confidence at least | And a passage at least |
|---|---|---|
| 24-49 | 99.988% (the top level, 0.9998820476527482) | 96.38% (0.9637746785002717) |
| 50-99 | 99.988% (the top level) | 96.38% |
| 100-149 | 90.9% (0.9090909090909091) | 94.20% (0.9420289855072465) |

Otherwise the result is "no strong signs". Both levels are compared the way
the bands are: the printed percentage against the level times 100. Each bin's
pair was chosen on CAL by the release's 1% rule: among every release level of
90.9% or more, paired with every passage level of 90% or more or with no
passage condition, the pair that flags the most CAL AI-involved excerpts while
flagging at most 1% of CAL human-document excerpts (rounded down: 21 of 2,109
at 24-49 words, 11 of 1,123 at 50-99 and 11 of 1,112 at 100-149). The chosen
pairs flag 21, 9 and 9 of those human excerpts. No rule on the confidence alone
qualifies at 24-49 or 50-99: even the top level flags 83 and 21 of them. The
rule depends on passages, so when passages are switched off or fail, the page
treats a text under 150 words as too short.

On the Test (rule chosen on CAL, rates measured on the consumed Test):

| Words | Primary humans flagged | Learner essays | Spoken conversations | Human parts of AI documents | AI-involved, primary and frontier | Frontier | AI texts left at "no strong signs" |
|---|---:|---:|---:|---:|---:|---:|---:|
| 24-49 | 34 of 5,909 (0.58%, 0.41-0.80%) | 5 of 3,488 (0.14%) | 13 of 1,693 (0.77%) | 12 of 2,087 (0.58%) | 4,311 of 11,461 (37.6%) | 332 of 1,271 (26.1%) | 62.4% |
| 50-99 | 30 of 3,164 (0.95%, 0.67-1.35%) | 2 of 1,847 (0.11%) | 1 of 863 (0.12%) | 7 of 850 (0.82%) | 3,875 of 6,265 (61.9%) | 362 of 690 (52.5%) | 38.1% |
| 100-149 | 31 of 3,156 (0.98%, 0.69-1.39%) | 3 of 1,856 (0.16%) | 3 of 859 (0.35%) | 3 of 517 (0.58%) | 5,079 of 6,593 (77.0%) | 535 of 742 (72.1%) | 23.0% |

- **AI-involved excerpts.** Most are wholly AI-written: an excerpt of a mixed
  document is usually all human or all AI. Wholly AI-written excerpts are
  flagged far more often than excerpts that mix human and AI text: 39.1%
  against 7.0% at 24-49 words, 68.7% against 21.2% at 50-99, and 88.8% against
  45.3% at 100-149. Base-model continuations and splices (the robustness set)
  are almost never flagged: 7 of 967, 3 of 85 and 21 of 1,308.
- **Web text runs higher.** By domain, FineWeb web text has 7 of 409 (1.7%),
  4 of 215 (1.9%) and 8 of 218 (3.7%) flagged; Wikinews 4 of 159 (2.5%) at
  50-99 words and Wikisource 5 of 133 (3.8%) at 100-149 are higher still.
  Court opinions, the student essays, Ubuntu IRC and the Federal Register
  have none in any bin. The counts are small.
- **Where the excerpt starts.** At 24-49 words, excerpts from document starts
  were flagged somewhat more often than excerpts from inside documents: humans
  21 of 2,726 (0.77%) against 13 of 3,183 (0.41%), AI-involved 40.8% against
  34.7%. The 50-99 and 100-149 bins use document starts only.
- **Every flagged text has a highlighted passage**, so the human rates above
  are also the rates at which a human text gets a highlight. A text under 48
  words can hold at most one passage.
- **No very strong result.** A stricter pair chosen the same way for 0.1% would
  rest on one or two CAL human excerpts, too few to choose a rule, so the page
  has no second positive result under 150 words.

## Long texts

The page scores at most the first 1,000 words of a text. This section measures
the served package on texts of 451 to 1,000 words and on the cut that produces
a text's first 1,000 words: the consumed Test, with CAL beside it.

**How the long texts were made.** Every Test human document is an excerpt of
at most 450 words and no AI-involved document is longer than 619 words, so the
long texts were built from them, each starting at its source text's start:

- **Natural human texts**: the 260 Project Gutenberg Test excerpts (170 on CAL)
  continued in their own book files, and the ELLIPSE essays (student and
  learner essays) whose Test document is exactly an essay's leading
  paragraphs, re-rendered whole (1,059 of 2,130; few essays pass 700 words).
- **Extended mixed texts**: a mixed document (span or full rewrite) whose
  parent is one of those human texts, followed by the parent's own
  continuation. This is a natural long document with one AI-edited part,
  which always sits in its opening 150-450 words (Test: 388 sources, CAL: 169).
- **Joined texts** (not natural text): documents of one domain (humans, all 18
  primary domains), of one domain and one generator (model-only or mixed), or
  of one frontier generator, joined by a blank line, each document used once.
  A joined text changes topic at each join, which a real document does not, so
  joined and natural texts are reported apart.

Each source gave up to four texts: one of 450-699 words and one of 700-1,000
words (each to the first unit end at or after a hashed target length), the
first 1,000 words exactly, and the same first 1,000 words cut back to the last
unit end inside them (the page's cut, below; kept if at least 900 words). A
unit end is a sentence end (".", "?" or "!" followed by whitespace) or a blank
line. The texts of one source overlap, so the bins are reported apart, never
pooled. Texts that fail the page's English screen were left out. Every text
was scored with the scoring server build and model package the page uses; 32
texts over the 2,048-token limit (all from Project Gutenberg books) were left
out.

### Human texts of 451 to 1,000 words

Texts that end at a sentence end or blank line:

| Words | Population | Texts | 50% or more | Strong band or above | Very strong |
|---|---|---:|---:|---:|---:|
| 150-450 (the band table) | primary humans | 3,286 | 203 (6.2%) | 43 (1.31%, 0.97-1.76%) | 0 |
| 450-699 | natural: Gutenberg books, student essays | 259 | 1 | 1 (0.39%) | 0 |
| 450-699 | joined primary humans | 602 | 53 (8.8%) | 8 (1.33%, 0.67-2.60%) | 0 |
| 450-699 | learner essays, whole | 187 | 9 | 2 (1.07%) | 1 |
| 700-1,000 | natural | 239 | 2 | 1 (0.42%) | 0 |
| 700-1,000 | joined primary humans | 611 | 55 (9.0%) | 7 (1.15%, 0.56-2.35%) | 0 |
| 700-1,000 | learner essays, whole | 33 | 3 | 1 | 1 |
| first 1,000, the page's cut | natural | 257 | 2 | 0 (0-1.47%) | 0 |
| first 1,000, the page's cut | joined primary humans | 637 | 55 (8.6%) | 6 (0.94%, 0.43-2.04%) | 0 |
| first 1,000, the page's cut | learner essays, whole | 13 | 2 | 1 | 1 |

- Joined human texts from all 18 primary domains reach the strong band at the
  band table's rate (1.33%, 1.15% and 0.94% against 1.31%), and natural long
  human text at most once per bin. None of the 2,605 primary human texts
  reaches the very strong band (interval 0-0.15%). The 50%-or-more rate of
  joined texts is a little higher (8.6-9.0% against 6.2%), below the strong
  cut. By domain, FineWeb web text is the highest (2 of 40, 3 of 40 and 3 of
  42), as at 150-450 words.
- A long human text is not flagged as often as its worst part: at 450-699
  words, 26 of the 602 joined texts hold a document that reached the strong
  band on its own, but only 8 joined texts are flagged.
- Two whole learner essays reach the very strong band (one at 523 words, one
  at 911 words and in its first 1,000 words). At 150-450 words the 1,980
  learner essays had 5 in the strong band and none very strong. There are too
  few long essays for a rate, but the very strong band is not free of long
  learner essays.
- **CAL agrees.** In the three bins that end at a sentence end, the strong cut
  flags 1 of 359, 0 of 357 and 0 of 377 CAL human texts (Gutenberg books and
  joined texts), and the very strong cut none: within the 1% and 0.1% the
  release's operating points allow (3 and 0 texts). The page uses the same
  bands and cuts from 150 to 1,000 words, in two length bins (150-450 and
  451-1,000) so it can say which evidence applies.

### The cut: exactly 1,000 words or the last sentence end

The same first 1,000 words of each source, cut at exactly word 1,000 (inside
a sentence in 616 of the 637 joined human texts) and cut back to the last unit
end inside them:

| Population (Test) | Texts | Strong or above: exact cut / sentence cut | Very strong: exact / sentence | Mean confidence: exact / sentence |
|---|---:|---:|---:|---:|
| joined primary humans | 637 | **309 (48.5%) / 6 (0.94%)** | 129 (20.3%) / 0 | 0.674 / 0.245 |
| natural humans (Gutenberg, student essays) | 257 | 1 / 0 | 0 / 0 | 0.178 / 0.128 |
| extended mixed (one AI-edited part) | 294 | **118 (40.1%) / 199 (67.7%)** | 12 (4.1%) / 53 (18.0%) | 0.770 / 0.866 |
| joined model-only, primary and frontier | 691 | 691 / 691 | 691 / 690 | above 0.999 / above 0.999 |
| joined mixed, primary and frontier | 635 | 624 / 621 | 528 / 521 | 0.992 / 0.990 |

On CAL, the joined human texts reach the strong band 106 of 214 times (49.5%)
at the exact cut and never at the sentence cut; very strong 65 against 0.

- A text that stops mid-sentence is read as AI-like. On paragraph-formatted
  human text the confidence jumps (Wikipedia 58 of 68 joined texts flagged at
  the exact cut, Global Voices 36 of 39); where lines routinely end without
  punctuation it barely moves (Gutenberg 0 of 52, Ubuntu IRC 0 of 26). On
  hard-wrapped Gutenberg text the exact cut instead lowers the detection of
  AI-edited documents (67.7% to 40.1%): it makes the output unreliable in both
  directions rather than adding a constant. The human excerpts the model was
  trained and tested on were never cut inside a sentence (each ends at a
  sentence, line or paragraph end); that is one likely reason, not tested
  here.
- **The page's cut** is therefore the sentence cut: the last unit end at or
  before the start of word 1,001. It dropped a median of 10 words (90th
  percentile 29, 99th 62, largest 97); in every source a unit end lay within
  the last 300 words of the 1,000, and within the last 100 for 99.65%. Text
  written one line per utterance with no punctuation is cut at its last line
  end; no measured source needed that. The Worker's word cut reproduces the
  measurement's texts byte for byte on all 3,457 sources over 1,000 words (12
  dense ones are then cut further to fit the token limit, below).
- **The same effect at 150-450 words.** The 3,164 primary human Test documents
  with their last 1-8 words dropped, so that each ends mid-sentence, reach the
  strong band or above 721 times (22.8%, 21.4-24.3%) and the very strong band
  283 times (8.9%), against 42 (1.33%) and 0 for the same documents whole.
  Global Voices (50%), Wikipedia (46%), Wikisource and Wikinews (37-38%) rise
  the most; Ubuntu IRC and Project Gutenberg barely move (1 to 2 each, of 143
  and 260). The page scores a text it is sent as sent, so a text that already
  stops mid-sentence is not covered by the rates in this document, at any
  length.

### AI-involved texts of 451 to 1,000 words

| Words | Population | Texts | Strong band or above | Very strong |
|---|---|---:|---:|---:|
| 150-450 (the band table) | primary and frontier AI-involved documents | 7,209 | 6,209 (86.1%) | 5,310 (73.7%) |
| 450-699 | extended mixed | 296 | 254 (85.8%) | 142 (48.0%) |
| 450-699 | joined model-only | 726 | 726 (100%) | 723 (99.6%) |
| 450-699 | joined mixed | 665 | 610 (91.7%) | 487 (73.2%) |
| 700-1,000 | extended mixed | 286 | 208 (72.7%) | 73 (25.5%) |
| 700-1,000 | joined model-only | 697 | 697 (100%) | 695 (99.7%) |
| 700-1,000 | joined mixed | 625 | 601 (96.2%) | 501 (80.2%) |
| first 1,000, the page's cut | extended mixed | 294 | 199 (67.7%) | 53 (18.0%) |
| first 1,000, the page's cut | joined model-only | 691 | 691 (100%) | 690 (99.9%) |
| first 1,000, the page's cut | joined mixed | 635 | 621 (97.8%) | 521 (82.0%) |

The joined rows include frontier generations; on their own, frontier
model-only texts reach the strong band at every length and mixed ones 92.5%,
95.9% and 96.1%.

- **Mostly AI-written text stays caught** at every length.
- **A long document with a small AI-edited part is caught less often than the
  band table's AI column says.** The extended mixed texts' AI parts, on their
  own as the 150-450-word mixed documents they came from, reach the strong
  band about 92% of the time. Followed by their original continuation (mean
  AI share 25%, 17% and 14%), they reach it 85.8%, 72.7% and 67.7% of the
  time, and the very strong band 48.0%, 25.5% and 18.0%. AI parts of 100 words
  or more stay at 84-96% at every length; parts under 100 words fall to 34% at
  1,000 words (48% for the 150-450-word mixed documents with as few AI words).
  This is the evidence thinning, not a misplaced cut: the human rates are
  unchanged, and lower cuts would raise them. CAL repeats it (extended mixed
  80.5%, 73.1% and 69.8%).
- **Highlighted passages.** Every AI text in the strong bands has at least one,
  and none is under 24 words, in any band. Byte precision and recall against
  AI passages of 24 words or more: extended mixed 0.93 / 0.97, 0.91 / 0.99 and
  0.88 / 0.99; joined mixed 0.90-0.91 / 0.87-0.89; joined model-only 0.999 /
  0.997 (150-450-word mixed documents in the strong bands: 0.936 / 0.873).
  Every long human text in the strong bands also gets a passage.
- **Estimated share**, mean absolute error: extended mixed 14.0, 17.6 and 19.0
  percentage points; joined mixed 10.3, 8.9 and 8.5; joined model-only 0.7
  (150-450-word mixed documents: 15.5). Joined human texts show a share of 25%
  or more 2.9-3.5% of the time and natural ones 1.6-2.1% (150-450: 5.5%).

### Calibration, tokens and later parts of a book

- **Calibration.** The release's calibration map is used at every length.
  On joined human texts with joined model-only texts, the Brier score is
  0.043, 0.047 and 0.049 in the three sentence-ended bins, against 0.034 at
  150-450 words for the same composition (expected calibration error 0.100,
  0.111 and 0.118 against 0.088): the joined human texts' mean confidence rises
  from 0.179 to 0.221-0.245 while their strong-band rate does not. The natural
  set (Gutenberg and essay humans with extended mixed texts) has a Brier score
  of 0.029-0.040. The exact 1,000-word cut is the only badly calibrated set
  (0.286). CAL follows the Test.
- **Tokens.** Test primary human documents have a median of 1.28 content
  tokens per word (10th-90th percentile 1.17-1.50), so a 1,000-word text is
  about 1,300 tokens and fits the 2,048-token limit for about 99.9% of them
  by projected density. Dense prose does not always: projected, 1.0% of the
  Test's Federal Register documents and 0.38% of its Gutenberg ones, and 2.3%
  of CAL's Gutenberg and Federal Register documents; measured, 2 of 259 Test
  and 4 of 166 CAL Gutenberg texts at 1,000 words. For such text the page cuts
  further back, to the last unit end inside the limit, counted exactly with
  the scorer's own tokenizer.
- **Later parts of a book.** Three consecutive parts of at most 1,000 words,
  each cut at a sentence end, of 243 Test Gutenberg books: none reached the
  strong band in any part (interval 0-1.56%; 4 books had a part at 50% or
  more). Book prose has the lowest human rate of any domain here, so this shows
  only that later parts of a book behave like its start.

## Estimated share on human documents

Human documents can show a sizeable estimated AI share. Among the 3,286
primary humans, the 95th percentile is 26%; 181 (about 1 in 18) have a share of
25% or more and 61 (1.9%) a share of 50% or more. By construction, *A*'s share
never exceeds its confidence. The share is computed from the whole text, so a
human text can show a share without getting a highlighted passage.

## The base rate behind the confidence

The calibration map is fitted on CAL with balanced weights: each source group
(for example one website or one book) gets equal weight, and within a group
each origin (human, mixed, model-only) gets equal weight
([competitive protocol](a-competitive-protocol.md), "Calibration"). Under those
weights AI involvement holds 66.6% of CAL (0.6658), about two in three; the
Test's primary and frontier documents give 65.9% under the same weighting.

The displayed confidence is therefore roughly the chance of AI involvement in
a mix where two of three documents involve AI. Where AI use is rarer, the
chance is lower. Dividing the displayed odds by 0.6658 / 0.3342 = 1.99 and
applying another base rate gives:

| Displayed confidence | If 50% of texts involve AI | 10% | 1% |
|---:|---:|---:|---:|
| 61% (an unclear-band value) | 44% | 8% | 0.8% |
| 90.9% (the strong band's floor) | 83% | 36% | 4.8% |

This holds only for texts that resemble the CAL population, and the table is
an illustration, not a calibrated conversion.

The top level, 99.988%, is left out of the table. It is clipped, so the map's
own odds there are loosely identified: taken at face value (displayed odds of
8,477 divided by 1.99, against the 73.7% of AI-involved documents at that
level) they would imply that about 1 human document in 5,800 reaches it. The Test cannot check that:
no Test or CAL human reached the level, which only bounds the primary-human
rate below 0.12%. The pilot's measured rate, 0.42%, is higher. With 73.7% of the primary
and frontier AI-involved documents at that level, the chance of AI involvement
at a 1% base rate is about 64% at the pilot's rate (about 53% at the upper end
of its interval, 0.65%), and 86% or more at any rate the Test allows (about 98%
at the map's own 1 in 5,800). At a 10% base rate the chance is about 95% at
the pilot's rate (93% at 0.65%) and 98.6% or more at any rate the Test allows;
at 50%, above 99% in every case.

## Highlighted passages in the strong bands

The page highlights passages only when the document is in the strong or very
strong band. On the whole Test (the primary, frontier and robustness documents
and the human sets), under the 24-word floor:

| Documents in the row | AI documents in scope | Character precision | Character recall | Passage precision | Passage recall |
|---|---:|---:|---:|---:|---:|
| Strong or very strong, all AI-involved | 6,240 | 0.975 | 0.948 | 0.920 | 0.930 |
| Strong or very strong, mixed only | 2,721 | 0.936 | 0.873 | 0.809 | 0.829 |
| Very strong, mixed only | 1,831 | 0.943 | 0.934 | 0.856 | 0.890 |
| Strong only, mixed only | 890 | 0.914 | 0.749 | 0.716 | 0.712 |
| All Test mixed documents, highlights in every band (the band rule ignored; for comparison) | 5,173 | 0.922 | 0.534 | 0.771 | 0.505 |

- **Which documents.** "Mixed" here includes the robustness documents and the
  frontier mixed documents. The 6,252 AI-involved documents in the strong bands
  are the band table's 6,209 primary and frontier documents plus 43 robustness
  documents; 12 of them (all mixed) are out of scope because none of their AI
  passages has 24 words, leaving the first row's 6,240. The rows count
  documents of every length: of the 6,240, 271 are under 150 words (130 mixed,
  141 model-only), texts the page would not score, and 50 to 52 are over 450
  (all 50 such model-only documents and up to 2 mixed ones). Restricted to 150 words or more, the "strong or very strong, mixed only"
  row becomes 2,591
  documents with character precision 0.936 and recall 0.872, passage precision
  0.807 and recall 0.828: almost unchanged. The 2,721 mixed documents in scope are 2,406 primary,
  273 frontier and 42 robustness documents. The 5,173 of the every-band row are the 3,221
  primary, 412 frontier and 1,540 robustness mixed documents with an AI passage
  of 24 words or more.
- **How they are measured.** An AI passage is a maximal run of words an AI
  model wrote or rewrote. Words of AI passages shorter than 24 words are out of
  scope: they count neither as AI nor as human, in the numerator or the
  denominator. Character precision is the share of highlighted text that lies
  in AI passages of 24 words or more; character recall is the share of the text
  in those passages that is highlighted. A highlighted passage and an AI
  passage match when their character overlap is at least half of their union,
  each matched at most once; passage precision and recall count the matches.
  A highlight that touches no AI passage of 24 words or more and lies at least
  half on out-of-scope words is ignored; any other unmatched highlight counts
  against precision. Each figure is a mean of per-document ratios over AI
  documents (a document with no highlight is left out of precision and counts
  as zero recall), with the balanced weights above recomputed over the
  documents in the row, as in the [span localization results](a-spans-results.md). Model-only
  documents, wholly AI-written, raise the all-AI figures; the mixed rows are
  the harder case.
- **What a reader sees across all mixed documents.** Only the 2,721 in the
  strong bands can get a highlight, and 44 of them get none, so 2,677 of the
  5,173 in-scope mixed documents (52%) show a highlighted passage at all; for
  mixed documents of 150 words or more it is 2,548 of 4,888 (52%). Under the
  page's rule the other 48% count as zero recall, so the recall a reader sees
  across all mixed documents is roughly half the strong-band figure; the last
  table row instead scores the package's passages in every band.
- **Human documents with a highlighted passage.** Precision above counts AI
  documents only. Of the 6,132 Test humans, 49 are in the strong band (none in
  the very strong band), and 43 of those get at least one highlighted passage:
  39 of the 3,286 primary humans, 4 of the 1,980 learner essays and none of the
  866 spoken conversations. Across the 49, an average of 48% of their words is
  highlighted. If highlights were shown in every band, 151 Test humans would
  get a passage.
- **The confidence on each passage** estimates the chance that at least half
  of the passage's words are AI-written. Over every passage the package finds
  on the Test, in every band, it is well calibrated (expected calibration
  error 0.010), but it is overconfident on the report set of 545 spliced
  rewrites (0.056;
  [whitepaper](../whitepaper/slop_ninja.pdf), Section 10). These are one-run
  retrospective figures.

## Limits

- The Test is consumed: these are retrospective statistics, not fresh
  held-out evidence. CAL figures are in sample.
- The human test texts are historical public writing. Text written today,
  polished by grammar tools or passed through a rewriting tool was not
  measured.
- The AI-involved texts come from the open-weight models listed above and the
  hosted frontier models of the [frontier-model positives](a-frontier-positives.md)
  page, generated for the Test in October 2026. Other or newer models and
  deliberate evasion were not measured, and base-model continuations (the
  robustness set) are mostly missed.
- The short-length rates use parts of longer Test documents, not texts written
  to be short (emails, posts, short answers), and the short-text rule was
  chosen on CAL after the Test had been opened. CAL holds about 1,100 human
  documents, so each bin's rule was chosen with room for only 11 to 21
  flagged human excerpts. The rule belongs to span package d24: another package needs its
  own measurement.
- Long texts were built, not found: natural long human text covers Project
  Gutenberg books and ELLIPSE essays only, and the other domains rest on
  joined documents, which change topic at each join. No natural long AI text
  exists: the AI rows are joined generations, or extended mixed texts whose AI
  part always sits in the opening 150-450 words; an AI part later in a long
  document was not measured. The mid-sentence effect was found during the
  long-text measurement, not planned for; the sentence-cut texts and the
  150-450-word check were added after it was seen. The rule for judging
  whether the bands hold above 450 words was written before the long-bin
  results outside Gutenberg were read. The figures belong to span package
  d24.
- The Common Crawl pilot's documents are web pages from before the cutoff, not
  documented human writing; its hand review covered a sample of the flagged
  documents.
- Apart from the short-text comparison above, this document measures neither
  the document confidence's calibration nor the share's accuracy on
  AI-involved text. It quotes one summary figure for
  each from other documents: on the Test, R0's involvement Brier score
  (balanced weights, all documents) is 0.0645
  ([span localization results](a-spans-results.md)), and its mean absolute
  share error on mixed documents is 15.5 percentage points
  ([whitepaper](../whitepaper/slop_ninja.pdf), Section 10).
- *A* is English-only.
