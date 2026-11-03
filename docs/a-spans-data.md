# A span training data: spliced rewrites

Generated October 30, 2026 under the
[A span localization protocol](a-spans-protocol.md) and amended for its
version spans-v1.1 the same day, before any fit used it. This note describes the
spliced-rewrite cohort that supplies most of *A*'s token-level span
supervision. Each document is mostly human text in which selected
sentences, clauses, paragraphs or runs were rewritten by a local open-weight
model, or new sentences inserted. The note reports aggregate counts only.
The documents, their parents, prompts, journals and per-document reports
stay in ignored private storage, together with a data amendment record that
lists every implementation choice and pilot change.

These rows are **source-conditioned mixed rewrites**. They are kept and
reported apart from writing generated from scratch (model-only rows), and
from the existing planner, frontier and full-rewrite mixed rows.

## What a spliced document is

A human-only parent keeps every character outside the edited regions. Each
edit is requested against the original parent, independently, and the
outputs are spliced back in. A record carries:

- the final text;
- the exact model-produced spans as UTF-8 byte offsets and as character
  (Unicode scalar) offsets;
- the realised share: words inside the spans over all words, where words
  are maximal runs of non-whitespace under the Python `str.isspace` rule
  that the trainer and runtime use;
- the edit history: for every edit the policy, operation, generator,
  model and configuration hashes, voice or brief id, template id, prompt
  and request hashes, decoding and seed, parent offsets, final offsets,
  before and after hashes, word counts and word-level change fraction.

The untouched parent is the paired human negative, with the same split and
source group. Every parent is already a row of the release trainer bundle,
so the pair reuses that row rather than duplicating it.

Requested fractions are never labels. The label is the realised share and
the exact spans.

## Parents

- **Train.** The Train humans of the release trainer bundle, each verified
  against the splits-v5 Train or Train-mining manifest by id, text hash and
  source group. These rows have already passed the assembler's rights
  admission and withdrawal rules. Excluded: the three RAID domains, the
  held learner-essay pool, 372 legacy rows absent from splits-v5, and any
  RAID-matched group. No parent exceeds 1,800 tokens. This leaves 42,605
  parents in 32 domains.
- **Dev-iter.** The 570 Dev-iter humans of the bundle. They are used only to
  build a span-evaluation set, which is never a training input.

No source group spans Train and Dev-iter. CAL, Dev-select and Test humans
are never parents, and Dev-iter humans are parents only of the evaluation
set.

## Plan

Everything is keyed by SHA-256 of the item identity, so a plan is
reproducible from its inputs.

**Allocation.** Train items per domain follow the square root of eligible
parents, scaled to 40,000, with a floor of 300 and a cap of 4,000. A parent
gets at most two variants, and 20% of parents get two where availability
allows (small domains get two for every parent). The plan holds 1.5 times
the allocation as a stratified reserve: 53,055 Train items. Items are
ordered by stratified position, so every prefix keeps the domain mix. The
Dev-iter set requests four variants of each human (2,280 items). It is
split 50/50 by source group into a selection half and a report half.

**Selection policies** (requested shares; a policy that the parent text
cannot support moves along a fixed cycle at plan time, never after any
generation):

| Policy | Requested | Edit units |
|---|---:|---|
| One sentence | 14% | one whole sentence |
| Scattered | 18% | 2–5 non-adjacent sentences |
| Paragraph | 14% | one paragraph, or a 3–8-sentence run |
| Most of the document | 8% | all but 1–3 sentences |
| Clause | 16% | 1–3 sub-sentence clauses of 3–25 words |
| Insertion | 12% | 1–3 new sentences after sentence ends |
| Condensation | 10% | 2–5 adjacent sentences replaced by a shorter passage |
| Compound | 8% | two of the above on disjoint, non-adjacent regions |

**Operations** per edit, by family:

| Family | Operations |
|---|---|
| Paraphrase | plain paraphrase (unvoiced) |
| Style or tone | voiced paraphrase, casual, formal, simplified |
| Polish | proofread, consistent spelling, the pinned fix-slop skill |
| Expansion | expand (1.4 times) |
| Condensation | condense (0.65 times) |
| Humanize | three humanize phrasings |
| New text | bridging sentence, elaborating sentence, continuation at the end |

- Style-capable edits take a voice or brief from the frozen style-library
  partition of their split: the Train partition for Train, the Dev-iter
  partition for the evaluation set. About a quarter of them are unstyled.
- One voice serves the whole document for 70% of documents; the other 30%
  pick per edit.
- Half of the sentence and clause edits also see the neighbouring
  sentences as read-only context.

**Generators.**

- **Train:** the eight Train instruct families: Qwen3.6-35B-A3B,
  Phi-4-mini, Mistral-7B-v0.3, Gemma-3-4b, Granite 4.0 H-Tiny, SmolLM3-3B,
  ERNIE-4.5-21B-A3B and gpt-oss-20b. They are assigned in a keyed uniform
  cycle, with pinned GGUF hashes and llama.cpp b11756.
- **Dev-iter set:** the seven Dev-iter families (no gpt-oss).
- **Second generator:** the plan gave 10% of multi-edit documents a second
  generator. Their attribution target is empty, which the trainer's row
  contract refuses, so they are excluded from the training snapshot.
- **Not used:** base checkpoints, hosted frontier models, and the CAL-only
  and Test-only families.

Decoding is the competitive plan's keyed sampled profile.

## Acceptance

Each edit has two attempts. A document is kept only when every edit is
accepted. Otherwise it is a retained terminal failure, with no substitution
of policy, generator or voice. An edit is rejected for:

- **Response faults:** an HTTP error, a missing or empty output,
  chat-template tokens, a `length` finish (truncation), or control
  characters.
- **Refusal or meta text:** refusal and preface openings ("Sure",
  "Here is", "Rewritten", "Note:"); meta phrases ("the passage", "the
  original", "here's a", "let me know"); prompt labels leaking into the
  output. The protocol's openings and phrases are exempt when the source
  or its shown context contains them; the added phrases and prompt labels
  are exempt when the parent document contains them. The v1.1 screen adds
  edit labels, assistant prefaces, prompt and skill echoes, persona labels,
  word-count notes and scripts the parent never uses (see below). It was
  applied after generation, so the counts in the next section predate it.
- **Added formatting:** headings, bullets, numbered lists, bold or code
  fences not in the source; enclosing quotation marks.
- **Length:**
  - rewrites 0.5–1.5 times the source words;
  - expansions 1.2–2.2 times;
  - condensations 0.3–0.8 times;
  - clauses 2–40 words;
  - insertions 6–60 words.
- **Seams:**
  - a missing sentence-final mark;
  - a changed clause boundary mark or first-letter class;
  - a new line break inside a sentence edit;
  - leading or doubled punctuation;
  - wrong capitalisation at a sentence start.
- **Near-copies:**
  - word change below 0.10 (0.02 for polishing);
  - byte-identical output;
  - a repeat of the read-only context, or of the rest of the sentence for
    clauses;
  - an insertion sharing a 12-word window with its parent.
- **Duplicates:** overlap with evaluation text or other accepted output.

**Assembly checks.** Assembly verifies, for every document:

- the human characters outside the spans are byte-identical to the parent
  with the replaced spans removed;
- every span edge sits on a whitespace boundary;
- no word straddles a span edge, and the share equals the whole-word
  fraction;
- the document has at most 2,048 tokens.

A separate check with the trainer's own label code re-derives the token
labels. It confirms that no token straddles an edge and rebuilds each
parent byte-exactly from the record and its edit history.

## Pilot

Three pilot rounds ran on the same 500 Train items and 918 edit requests
before the main run. Their outputs are quality evidence only and are not in
the data set.

| Round | Change | Documents accepted | Requests accepted |
|---|---|---:|---:|
| 1 | competitive plan prompts unchanged | 62.4% | 72.2% |
| 2 | sentence edits state their shape and length | 77.2% | 84.4% |
| 3 | word caps, clause boundary anchored on its own words, two stricter screens | 76.6% | 84.2% |

- **Round 1.** Whole-sentence rewrites came back as paragraphs: the median
  over-long output had 3.1 times the source words, and many added line
  breaks. The competitive plan's prompts give no word number and invite
  paragraph breaks.
- **Round 2.** Stating "a single sentence (or k sentences) of about N
  words, with no line breaks" raised sentence-edit acceptance from 64% to
  80%.
- **Remaining rejections.** These are mostly generator capability:
  clauses that must end with a comma, voiced single-sentence edits,
  over-long expansions and, for Phi-4-mini, condensation.
- **Round 3** kept overall acceptance the same. Its screens catch leaked
  prompt labels and assistant phrasing that round 2 accepted.

All accepted pilot documents passed every assembly and trainer-side check.

## Generated sets

Generation ran on the build-host RTX 4090, one generator at a time under the
host GPU lock, yielding to training jobs between steps. Each step served
the pinned model with four parallel slots. Short spliced requests ran at
about 150–260 requests per minute (Qwen3.6 148, Phi-4-mini 152, Granite 190,
Gemma-3 198, SmolLM3 217, ERNIE-4.5 241, Mistral 260) and 68 for
gpt-oss-20b, which reasons before answering. A chunk of 8,000 plan items
took about 1 hour 45 minutes including model loads. Generation took about
12 GPU hours: 0.6 for the Dev-iter set and the rest for 53,055 Train
items. The three pilot rounds took about 1.5 more. The protocol's ceiling
is 40 hours.

### Train

The protocol sets Phase A to the first 20,000 accepted documents in plan
order. Phase A acceptance was 78.7%, above the 70% gate for Phase B, so
generation continued to the 40,000 target. The Train set is the first
40,000 accepted documents in plan order. Its first 20,000 are exactly
Phase A, byte for byte. The protocol's per-domain cap of 4,000 applies to
accepted documents: later Wikipedia items were skipped (not failed) once
it reached the cap.

| | Phase A | Train set (Phase B) |
|---|---:|---:|
| Plan items processed (plan-order prefix) | 25,426 | 51,341 |
| Skipped at the domain cap | 0 | 535 |
| Accepted documents | 20,000 (78.7%) | 40,000 (78.7%) |
| Terminal failures (an edit failed twice) | 5,426 | 10,806 |
| Excluded at assembly | 0 | 0 |
| Edits in accepted documents | 33,687 | 67,872 |
| Words | 4,583,059 | 9,149,354 |
| Model-produced words | 1,236,205 (27.0%) | 2,491,533 (27.2%) |
| Mean realised share | 0.278 | 0.281 |
| Labelled tokens, AI / human (trainer label code) | 1.63M / 4.29M | 3.29M / 8.55M |
| Largest document (tokens) | 942 | 942 |

The Train set adds 8.6M human tokens that sit in the same documents as AI
spans, against 2.5M in the whole existing bundle. Of 113,252 attempts,
first attempts were accepted 77.9% of the time and second attempts 35.8%.
The rest of this section describes the 40,000-document set.

**Policies** (accepted documents and document acceptance):

| Policy | Requested | Accepted share | Documents | Document acceptance |
|---|---:|---:|---:|---:|
| One sentence | 14% | 18.1% | 7,228 | 88.0% |
| Scattered | 18% | 16.0% | 6,410 | 69.0% |
| Paragraph | 14% | 15.4% | 6,170 | 83.5% |
| Insertion | 12% | 12.7% | 5,078 | 92.3% |
| Condensation | 10% | 12.2% | 4,899 | 89.1% |
| Clause | 16% | 10.6% | 4,250 | 59.4% |
| Most of the document | 8% | 8.5% | 3,416 | 84.5% |
| Compound | 8% | 6.4% | 2,549 | 68.6% |

Requested shares are those of the protocol cycle; plan-time feasibility
moves some short documents to other policies before any generation.

**Operation families** (accepted edits): style 18.7%, paraphrase 17.4%,
new text 16.9%, condensation 15.1%, polish 14.3%, humanize 10.3%,
expansion 7.3%.

- Edit kinds: whole-sentence runs 61.4%, insertions 16.9%, clauses 13.1%,
  condensed runs 8.6%.
- 97.6% of the replacing edits change at least 30% of their words; 0.2%
  change less than 10%.
- Accepted edit lengths: 58.2% have 11–40 words, 17.6% have 10 or fewer,
  24.3% have more than 40.

**Shape.**

- Edits (and spans) per document: one 60.7%, two 18.2%, three 14.0%,
  four or more 7.1%.
- Realised share: below 10% for 24.7% of documents, 10–25% for 33.2%,
  25–50% for 25.4%, 50–75% for 9.0%, 75% or more for 7.6%.
- Style: voices 31.6% of edits, briefs 15.2%, unstyled style-capable
  edits 15.8%, operation-only edits 37.5%.
- 69.9% of documents use one voice throughout.
- 53.8% of edits saw read-only context.
- 33.1% of documents are a parent's second variant. Small domains give
  every parent two variants.
- Length: 29.0% of documents have fewer than 256 tokens, 69.3% have
  256–511, 1.6% have 512–1,023.

**Generators.**

- Documents: gpt-oss-20b 14.6%, Qwen3.6 14.1%, ERNIE-4.5 13.2%,
  Gemma-3 12.5%, Granite 11.7%, Phi-4-mini 10.1%, Mistral 10.0%,
  SmolLM3 10.0%; two generators 3.8%.
- The plan assigns them uniformly. The spread is acceptance per attempt:
  gpt-oss-20b 91.5%, Qwen3.6 87.2%, ERNIE-4.5 80.0%, Gemma-3 77.5%,
  Granite 65.0%, Phi-4-mini 58.6%, Mistral 58.3%, SmolLM3 53.8%.

**Domains.** All 32 eligible Train domains are present, and 29 have at
least 300 documents. Largest: Wikipedia 4,000 (the cap), PLOS 3,826,
Project Gutenberg 3,742, web blogs 2,705, the Federal Register 2,689,
Wikinews 2,503, FineWeb 2,221, Global Voices 1,993 and Reddit 1,712.
Smallest, where parents run out: American Stories 157, the Wikibooks
cookbook 156 and BL newspapers 23 (14 parents).

**Most frequent rejection reasons** (33,604 rejected attempts):

| Reason | Attempts |
|---|---:|
| sentence-edit length outside 0.5–1.5 times | 14,475 |
| clause boundary mark changed | 6,248 |
| new line break | 5,181 |
| missing sentence-final mark | 3,560 |
| clause first-letter class changed | 1,972 |
| enclosing quotes added | 1,556 |
| condensation length | 1,503 |
| repeated read-only context | 1,316 |
| repeated rest of sentence (clauses) | 1,249 |
| clause length | 1,049 |
| meta phrase "the passage" | 975 |
| truncated (`length` finish) | 949 |

Refusal, meta-text and prompt-label screens together caught 6,639
attempts. Examples are "SECOND:", "here's a", "here is the" and "(Note".

### Dev-iter span-evaluation set

| | Count |
|---|---:|
| Requested variants (4 per Dev-iter human, 7 families) | 2,280 |
| Accepted documents | 1,731 (75.9%) |
| Selection half / report half (by source group) | 867 / 864 |
| Words / of them model-produced | 385,717 / 110,625 (28.7%) |
| Labelled tokens, AI / human | 148,776 / 363,116 |

- Policies: scattered 287, one sentence 274, paragraph 266, insertion 255,
  condensation 205, clause 190, most 149, compound 105.
- Domains: Wikipedia 481, Project Gutenberg 263, PLOS 257,
  Global Voices 233, FineWeb 220, Federal Register 199, Wikinews 78.

The set is never a training input. Its voices come from the Dev-iter
partition of the style library.

### Checks on every accepted record

All 41,731 records (40,000 Train and 1,731 Dev-iter) pass these checks:

- the trainer's span validation;
- character spans equal to the byte spans;
- share equal to the whole-word fraction;
- no word and no labelled token straddling a span edge;
- at most 2,048 tokens;
- the parent rebuilt byte-exactly from the record's human text and edit
  history.

The no-op control reproduced every parent with replaced spans (47,309)
exactly.

## Training snapshot (spans-v1.1)

The generated sets above were amended before any fit used them, without new
generation or refill. The frozen training snapshot is:

| | Train | Dev-iter evaluation set |
|---|---:|---:|
| Generated and accepted | 40,000 | 1,731 |
| Excluded: two generators (empty attribution target) | 1,539 | 66 |
| Excluded: meta text or a foreign script (one generator) | 169 | 8 |
| **Snapshot** | **38,292** | **1,657** |
| Selection half / report half | | 834 / 823 |
| Typography normalized / raw | 18,985 / 19,307 | 837 / 820 |
| Model-produced words | 2,371,328 | 104,720 |
| Labelled tokens, AI / human (trainer label code) | 3.13M / 8.18M | 141k / 348k |

**Meta text.** An audit found generated spans that the first screens let
through: edit labels ("Passage edited:", "Edit:"), assistant prefaces
("Okay, here's your rewritten passage:"), echoes of the prompt and of the
fix-slop skill (most often a Gemma-3 polish opening "Remove AI tells, …"),
persona labels ("Elevator voice:"), word-count notes ("(37 words)") and words
switched into Chinese or Russian. The extended screen dropped 176 Train
documents, 169 of them with one generator (0.44%). By reason: skill echo 77,
Han script 31, an edit label 26, Greek script 13, a persona label 11, a meta
noun phrase 8, a word count 8, an assistant preface 5, CJK punctuation 5,
other 10. By generator, Gemma-3 had the most hits (76 edits), then ERNIE-4.5
(33), Granite (25) and Phi-4-mini (20). The same screen over the release
bundle's existing mixed rows flagged 177 of 21,482 Train, 8 of 1,185 Dev-iter
and 34 of 2,074 CAL rows (25 CAL rows with Chinese characters, 23 of them from
GLM-4). The Dev-iter and CAL rows are excluded from span metrics and from the
sentence and span confidence fits.

**Typography.** Several generators write characters that their human parent
never uses: curly apostrophes and quotes, em and en dashes, the non-breaking
hyphen and the narrow no-break space. Inside a parent that types straight
quotes and hyphens, such a character marks the AI span exactly. In the raw
stratum, the share of edits that introduce at least one such character
(including straight quotes in a parent that only uses curly ones) is:

| Generator | Edits with a marker |
|---|---:|
| gpt-oss-20b | 45.6% |
| Gemma-3 | 41.1% |
| ERNIE-4.5 | 37.1% |
| Qwen3.6 | 19.4% |
| Phi-4-mini | 19.0% |
| SmolLM3 | 13.0% |
| Mistral | 11.1% |
| Granite | 10.0% |
| All Train | 26.6% |

A keyed half of the documents is normalized to its parent's conventions.
Characters the parent never uses become the parent's form: straight quotes
and `-` in an ASCII parent, the parent's own dash, `...` for the ellipsis, a
space or the parent's no-break space. Straight quotes in a parent that only
uses curly ones become curly. Whitespace stays whitespace, so the words, the
span word boundaries and the share do not move. After normalization 0.1% of
edits keep a marker, all of them a variant within a curly class the parent
itself uses. The most frequent substitutions were the curly apostrophe to
`'` (7,558), the em dash to `-` (3,236) and the non-breaking hyphen to `-`
(2,366). Span metrics are reported for each stratum.

**Copied words.** Each edit records which output words a word-level LCS
aligns to the human text it replaced. 29.3% of Train AI words align (30.2%
in the Dev-iter set), from 48.6% for proofreading to 24.8% for expansion.
6.0% of edits keep a run of eight or more source words, and 3.0% keep a run
at least half as long as their output. These words are model-produced by
the share definition and stay labelled AI. Span metrics are reported with
and without them.

**Checks.** Every snapshot record passes the checks of the previous
section. Each record is also projected to the trainer's row format and
accepted by the trainer's own row parser (38,292 of 38,292 and 1,657 of
1,657). The same parser refuses exactly the 1,539 and 66 two-generator
records of the generated sets. The edit-history digests match, the raw half
is byte-identical to the generated text, and every copied-word span lies on
word edges inside its edit.

## Limits

- **Selection effect.** Documents are kept only when every edit passes.
  Accepted documents therefore under-represent many-edit scattered and
  clause documents, comma-ending clauses, and edits by the weakest
  instruction followers. Accepted shares are reported beside requested
  shares.
- **Generator balance.** Generators are assigned uniformly but accepted
  unevenly: gpt-oss-20b and Qwen3.6 documents outnumber SmolLM3 and
  Mistral documents by about 1.4–1.5 to 1.
- **Domain mix.** The stratified reserve refills failures in plan order,
  so a domain whose parents run out falls short of its allocation (BL
  newspapers: 23 documents from 14 parents) while large domains take up
  the slack. Large domains stop at the protocol cap of 4,000.
- **Low-change edits.** Polishing and spelling edits can leave many
  source words unchanged inside a span. Those words are AI by the share
  definition but cannot be told apart from text alone. Span metrics are
  therefore reported by change-fraction band and without copied words.
  The token loss still counts copied words as AI; down-weighting them
  would need its own protocol arm.
- **Typography.** Only the keyed half is normalized. The raw half keeps the
  generators' typography, so the shortcut remains in half the training data
  by design; the strata measure how much the head relies on it.
- **Single generator.** Every document now has one generator, so the data
  has no document that mixes two generators' styles.
- **Coverage.** Generators are local open-weight models only; hosted
  frontier rewrites are not spliced in this version. The Dev-iter set
  reuses humans already exposed through the baseline's early stopping,
  and Test is not used.
- **Length.** Every parent is under 1,800 tokens and every document under
  2,048. Book-length or windowed spans are out of scope for this version.
