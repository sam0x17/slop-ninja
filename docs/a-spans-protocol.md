# A span localization protocol

Version spans-v1.1, fixed on October 30, 2026, before the first protocol fit,
span evaluation, decoder choice or calibration fit. It pre-registers how *A*
gains AI span localization: the data, the arms, the selection rule, the gates
and what is reported. A private companion in ignored storage holds paths,
hashes and counts; its SHA-256 is
`cee6ed7a90a5ff30e5294e42e55fe4f5dcbad55943d1129d0351355b8996a955`. It
supersedes version spans-v1 (fixed before any spliced generation, SHA-256
`740b0b7d47a56a4b9a10e93199290ab495ca818b9e9dd51874b387036b03fc5a`); the
[amendment record](#amendment-record-v11) lists what changed and why. Any
later change needs a new version with an amendment record.

The starting point is the [baseline release candidate](a-baseline-release.md)
(`E1aux`, seed 17), trained under the [competitive A protocol](a-competitive-protocol.md)
v1.2. Nothing here changes that release. A follow-up protocol, spans-S2, is
pre-registered in [its own section](#follow-up-protocol-spans-s2).

## Goal

*A* gains a token-level AI-probability head on its shared encoder. From the
same probabilities it returns:

- **AI spans**, with exact character offsets (Unicode scalar index into the
  exact request text) and UTF-8 byte offsets, each with a confidence;
- **per-sentence** AI share and AI confidence percentages.

The document outputs keep their meaning. The emitted word probabilities are
tilted so that their word-weighted mean equals the emitted document share,
and so the sentence shares average to the document share. Sentence and span
confidences never exceed the document confidence.

Document Brier stays the reward. Span metrics are a reported qualification;
thresholds on them are set later, from these measurements.

## Populations

- **Test** of Tier C is consumed. It is used only for a labelled
  retrospective report, never for selection, fitting, calibration or the
  decoder choice.
- **Dev-select** was opened once for the baseline selection. It is not read
  or scored here.
- **Dev-iter** makes every choice. **CAL** is used only for calibration fits
  and the operating-point gate.
- Spliced training rows descend only from Train-side human documents.

## Existing span labels

Measured on the release trainer bundle with the trainer's own token-label
code (Dev-select skipped):

| Split | Rows | Mixed | Mixed with partial spans | Full rewrites | Mixed with two runs |
|---|---:|---:|---:|---:|---:|
| Train | 86,575 | 21,482 | 17,534 | 3,948 | 1,527 |
| Dev-iter | 2,873 | 1,185 | 976 | 209 | 214 |
| CAL | 5,379 | 2,074 | 1,731 | 343 | 389 |

- Every mixed row carries exact byte spans. None is missing, and none fails
  the label checks.
- Every span edge sits on a whitespace boundary. The realised share equals
  the whole-word fraction inside the spans in every row.
- No token straddles a span boundary. Where an endpoint falls inside a token,
  it falls in that token's leading whitespace. Decoding therefore maps tokens
  to characters by their non-whitespace extent.
- No row exceeds 2,048 tokens, and nothing is truncated. Longer inputs remain
  unsupported in this version.
- Train has 9.8M AI-labelled and 17.2M human-labelled tokens. Within-document
  contrast exists only in the partial-span rows, which hold 2.5M human tokens.

**Gaps.** Every span is a whole-sentence run, at most two per document, or a
whole text. There are no clause-level edits, no insertions and no scattered
edits of three or more sentences. Most of the 36 Train domains have fewer
than 250 mixed rows. Low-change operations (polish, spelling and slop fixes)
can leave source words unchanged inside a model-produced span. Those words
count as AI under the share definition, so their metrics are reported by
change-fraction band.

## Spliced-rewrite data

The new Train cohort rewrites selected parts of Train human documents with
the local open-weight generators and splices the rewrites back in.

**Parents.** Train and Train-mining humans only, keeping their split, source
group, writer and domain. Excluded:

- RAID humans and the source groups matched to evaluation pages;
- held pools, and the out-of-domain human pools kept as false-positive
  diagnostics;
- evaluation-only licences, and any source not admitted for commercial
  training.

**Paired original.** Each spliced document has its untouched parent as a
human-only row in the same split and source group. An existing bundle row is
reused rather than duplicated.

**Selection policies** are assigned by keyed ranks within split and domain:

| Policy | Share of documents | Edit units |
|---|---:|---|
| One sentence | 14% | one whole sentence |
| Scattered | 18% | 2–5 non-adjacent sentences |
| Paragraph | 14% | one paragraph, or a 3–8-sentence run |
| Most of the document | 8% | all but 1–3 sentences |
| Clause | 16% | 1–3 sub-sentence clauses of 3–25 words |
| Insertion | 12% | 1–3 new sentences at sentence boundaries |
| Condensation | 10% | 2–5 adjacent sentences replaced by a shorter passage |
| Compound | 8% | two of the above on disjoint regions |

AI-only deletions are not generated. They leave no model-produced words, so
they have no token label under the share definition.

**Operations** per edit: paraphrase 18%; style or tone shift 22%; polish or
grammar fix 16%; expansion 10%; condensation 10%; humanize-style edits 12%;
new text 12% (bridging, elaborating or continuing sentences, insertion only).

- About a quarter of style-capable edits are unstyled. The rest take voices
  from the frozen style-library partition of their split.
- Half of the edits also see the neighbouring sentences as read-only
  context.

**Generators.**

- **Train:** the eight Train instruct families of the competitive roster. Base
  checkpoints were allowed only for continuations at the end of a document
  and were not used.
- **One generator per document.** Every spliced row carries exactly one
  generator label, as the trainer's row contract requires. The plan gave 10%
  of documents a second generator; those documents are excluded from the
  training snapshot.
- **Dev-iter:** the seven Dev-iter families.
- **Never used:** the CAL-only and Test-only generators.
- **Hosted frontier (optional):** at most 7.5% of the cohort, only over
  public-domain, CC0, CC BY or CC BY-SA parents, checked per row, through the
  existing collector and the default client configuration. No private or
  non-redistributable text is ever sent.

**Acceptance.** The existing checks apply: empty output, chat-template
tokens, duplicates and overlap, the 2,048-token limit, and byte-identical
output. New checks reject:

- refusals, and meta text such as a preface naming the rewrite;
- added formatting or quotation marks;
- truncated or unterminated replacements, and per-policy length violations;
- seam faults, such as doubled punctuation, wrong capitalisation, or new
  line breaks inside a sentence edit;
- near-no-op rewrites (word change below 0.10, or below 0.02 for
  polishing);
- inserts that copy neighbouring human text;
- edit labels ("Passage edited:", "New sentence:"), assistant prefaces
  ("Okay, here's your rewritten passage:"), echoes of the prompt or of the
  pinned fix-slop skill, persona labels ("… voice:"), word-count notes, and
  words in a script the parent never uses. A matched phrase is exempt when
  the parent itself contains it.

After assembly, the human characters outside the spans must match the parent
byte for byte. A document is accepted only when every edit is accepted.
Failures stay in the accounting.

**Records.** Each accepted document records:

- the final text, with exact byte and character spans;
- the realised final-word share;
- the full edit history: parent, policy, operation, generator, voice and
  template ids, prompt and request hashes, decoding, before and after offsets,
  and the change fraction;
- a typography stratum. In a keyed half of the documents, generated quote,
  dash, hyphen, space and ellipsis characters that the parent never uses are
  mapped to the parent's conventions; the other half keeps the raw text.
  Whitespace stays whitespace, so words, spans and the share are unchanged;
- per edit, the output words copied verbatim from the replaced human text,
  from a word-level LCS alignment. They stay AI words: labels and the token
  loss are unchanged.

The rows are labelled as source-conditioned mixed rewrites and are reported
apart from writing generated from scratch.

**Sizes.**

- **Train:** 40,000 accepted documents, domain-stratified. A first stage of
  20,000 is followed by a continuation when acceptance and GPU time allow,
  with a hard cap of 60,000. The measured generator throughput puts 40,000
  at about 20–25 GPU hours, and the ceiling is 40.
- **Dev-iter evaluation set:** four variants per Dev-iter human. It is split
  by source group into a selection half and a report half, and it is never
  trained on.

The accepted snapshot is frozen with checksums before the first fit. All
text stays in ignored private storage. The training snapshot is 38,292
Train documents and 1,657 Dev-iter evaluation documents (selection half 834,
report half 823): the generated 40,000 and 1,731 minus the two-generator and
meta-text exclusions, with no refill.

## Model and decoding

- **Token head:** a two-layer MLP with hidden size 256 and tanh-form GELU on
  the final token states. It has no gate into the document involvement
  logit, so the document heads keep their meaning. The loss is per-document
  token BCE over the labelled tokens.
- **Words** are maximal non-whitespace runs, the words of the share
  definition. A word's probability is the mean of its tokens' probabilities.
- **Share tilt:** one additive logit shift per document, found by f64
  bisection, makes the mean word probability equal to the emitted document
  share.
- **Sentences** come from the sealed sentence splitter.
  - Sentence share is the mean tilted word probability.
  - Sentence confidence is a CAL isotonic map of the largest word
    probability in the sentence.
- **Spans** come from hysteresis decoding with minimum length, gap merging
  and an optional sentence snap.
  - The grid is 108 points: upper threshold 0.5–0.7, hysteresis 0–0.2,
    minimum 1–5 words, merge gap 0 or 2 words, and snap off or 0.8.
  - Span confidence is a CAL isotonic map of the mean word probability in
    the span.
- **Export** schema `slop_ninja_full_a_export_v3`: heads v2 with the token
  head; calibration v3 with the sentence and span maps and the decoder.
  - The Rust `full_a` runtime reads v3 only.
  - The release is re-exported with no token head as a regression check, and
    its document outputs must match the v2 replay bit for bit.

## Arms

Every arm starts from the `E1aux` fit of its own seed. Seed 17 is the
release.

| Arm | Data | Training |
|---|---|---|
| S0− | release Train bundle | encoder and document heads frozen; token head only, 1 epoch |
| S0+ | Train bundle plus spliced rows and paired parents | as S0− |
| S1− | as S0− | from S0−; joint fine-tune of encoder, all heads and token head, 1 epoch, encoder rate 5e-6 |
| S1+ | as S0+ | as S1−, from S0+ |

- The S0 arms leave every document output bit-identical to the starting fit.
  This is checked, not assumed.
- **Weights in the + arms:**
  - The spliced cohort is capped at 25% of positive document-loss mass.
  - It holds 60% of token-supervised mass, through a per-row token weight.
- **Token loss weight in S1:** chosen from {0.3, 1.0} by one seed-17 fit
  each on Dev-iter.
- **Seeds:** 17, 29 and 41. An arm is its three fits: never a best seed,
  never an ensemble.
- **Fit budget:** 20 GPU hours.

## Selection

1. **Checkpoints** (Dev-iter):
   - S0 takes the lowest raw mixed-document word Brier.
   - S1 takes the same, among evaluations whose document Brier stays within
     +0.002 of the starting fit. If none does, the start is kept.
2. **Eligibility.** An arm must pass every document gate and the replay gate
   on its seed-17 export. For S1 arms the CAL refit serves only this
   pass/fail check. No ranking reads CAL.
3. **Arm choice.** The lowest three-seed mean tilted word Brier on Dev-iter
   mixed documents wins, with documents balanced by source group and origin
   cell. The population is the Dev-iter bundle plus the selection half of the
   Dev-iter evaluation set.
4. **Tie rule.** It compares only the top two. If their paired 95% interval
   includes zero, the earlier arm in the order S0−, S0+, S1−, S1+ wins.
5. **Decoder.** On the selected seed-17 fit, the grid point with the highest
   balanced character F1 wins. At most 22 of the 570 Dev-iter humans (3.9%)
   may carry any decoded span.
6. **Then, in order:** the CAL fits, one evaluation of the report half, and
   the retrospective Test report. None of them changes a choice.

Intervals use the protocol v1.2 paired source-group bootstrap: 1,000 draws,
seed 20261026.

## Gates

| Gate | Rule |
|---|---|
| Document Brier (artifact) | Dev-iter uncalibrated balanced involvement Brier, seed 17 minus release seed 17 (0.0636): paired 95% upper bound ≤ +0.003 |
| Document Brier (recipe) | three-seed mean ≤ 0.0666 (release seed 17 + 0.003) |
| CAL operating point | at the refit 1% point, CAL humans flagged ≤ 8 of 1,140, and none at the 0.1% point; Dev-iter humans flagged at the CAL 1% threshold ≤ 11 of 570 |
| Share | Dev-iter share MAE no more than 0.5 points above the release |
| Attribution | Dev-iter AI attribution top-1 no more than 0.01 below the release |
| Rust replay | FP64 runtime against an FP64 Python reference of the trained function: the exported FP32 model widened exactly, with HF's own float32 rotary tables (widened, never recomputed in float64) and the attention softmax in float64. Max error ≤ 1e-5 on document outputs, token and word probabilities, and sentence and span values. Token, word, sentence and span offsets equal exactly; a case with a decision value within 1e-6 of a threshold is listed. Cases: 3 release synthetic cases, 12 synthetic span cases and 200 Dev-iter documents. The FP32 trainer function is compared with the same gates as a reported drift diagnostic |
| Tokenizer offsets | Rust offsets equal the Python offsets on the same cases |
| CPU latency | reported: one-thread FP64 reference at 512, 1,024 and 2,048 tokens, with head and decoder time separate |

**Reported span metrics:**

- token and word Brier, raw and tilted;
- character-level F1, with precision and recall;
- span F1 at character IoU ≥ 0.5, and mean IoU;
- start and end boundary error;
- sentence calibration (ECE and Brier) and sentence share error;
- span-confidence ECE;
- human documents with any decoded span;
- the pre-tilt gap between word mean and document share.

Each is broken down by domain, generator, policy, operation, change-fraction
band, share bin and cohort. Spliced rows are also reported by typography
stratum, and with and without copied words. Release-bundle mixed rows whose
spans carry meta text (Dev-iter 8, CAL 34) are excluded from span metrics and
from the sentence and span confidence fits; document metrics keep them.

## Consumed Test

After every choice is frozen, the selected artifact and its recipe are run
once on the consumed Test. They report the span metrics and the document
metrics, paired against the release. Every number is labelled retrospective
and is not selection evidence. No external detector is called.

## Execution

- All compute runs on the campaign host as bounded user units with exit
  files.
- GPU work holds the host GPU lock and yields to other jobs. Large-memory
  jobs hold the shared memory lock.
- Builds use one Cargo job.
- Python is limited to the trainer and the export. Data handling, decoding,
  metrics and calibration fits are in Rust.
- No live detector or hosted-model call runs in tests.

## Amendment record (v1.1)

Recorded after an audit of the implementation, the smoke run and the
generated data, and before the first protocol fit.

1. **Replay reference.** The Rust FP64 runtime had formed the rotary angles
   in float64, and the Python FP64 reference had been patched to match.
   The model was trained and evaluated with HF's float32 rotary tables, so
   that reference was a different function, about 1e-4 rad away at position
   2,047. The runtime now builds HF's float32 tables and widens them, and
   the reference carries no patch. On the smoke export the largest gated
   error is 4.4e-6 (200 Dev-iter documents) and 1.7e-6 (15 synthetic cases),
   with every offset equal. Against the FP32 trainer function the token
   probability drift is 1.7e-4, reported as a diagnostic.
2. **Two-generator documents** are excluded (1,539 Train, 66 Dev-iter).
   Their empty attribution target violates the trainer's row contract, and
   masking it would change the document training and evaluation code that
   the document gates rely on.
3. **Meta-text screen.** The extended screen above dropped 169 further Train
   documents (0.44%) and 8 Dev-iter documents, and flags release-bundle
   mixed rows for exclusion from span metrics.
4. **Typography strata** remove a shortcut: before normalization, 10–46% of
   edits per generator introduced a quote, dash, hyphen, space or ellipsis
   character that the parent never uses.
5. **Copied words.** 29% of spliced AI words align to the replaced human
   text, and 6% of edits keep a run of 8 or more words. The mask is recorded
   and reported; down-weighting those tokens would need its own arm and is
   deferred.
6. The data generator's documented implementation choices are adopted, and
   the realised policy mix is reported beside the requested one: one
   sentence 18.8%, paragraph 16.1%, scattered 14.9%, condensation 12.8%,
   insertion 12.4%, clause 10.5%, most 8.6%, compound 5.9%.

## Follow-up protocol spans-S2

Fixed on October 31, 2026, before any S2 fit, smoke fit or S2 metric. The
private companion's SHA-256 is
`c909ec7ad738a3189d85b15ec3cbff9529ed8fef28701b75b0f8648f63427f10`. The
shipped configuration (the release's document outputs plus the S0+ token
head) does not change. An S2 arm is only a candidate, and adopting one is a
separate decision.

**Why.** S1+'s joint fine-tune found spans best (primary span metric 0.1205
against 0.1412 for S0+), but its uncalibrated human-only Brier rose from
0.061 to 0.093 (three-seed means). The shift came with the spliced rows:
documents that are mostly unchanged human text but are labelled AI-involved,
with no human counterweight. D1 could not see it because it averages over
all documents.

**Arms.** All three train on the S1+ data from the S0+ fits, with token-loss
weight 1.0 and seeds 17, 29 and 41:

- **S2a, anchored document heads.** The S1+ joint fine-tune with every
  document label term replaced, on every Train row, by the same term against
  the frozen release fit's FP32 outputs. The document heads follow the
  release while the encoder adapts for the token head.
- **S2b, paired human contrast.** S1+ with each spliced row's untouched
  parent added to the same batch as a human entry carrying the child's
  document weight (document terms only). Human document weights are scaled
  so that the total human mass is unchanged, so the arm cannot gain through
  a prior shift.
- **S2c, separate span tower.** The encoder and document heads stay frozen,
  so the document outputs are bit-identical to the release. Copies of the
  top four encoder layers and the final norm, initialised from the frozen
  weights, read the frozen layer-18 input and feed the token head. Only the
  tower and the token head train. The tower learning rate (5e-6 or 3e-5) is
  chosen on seed 17. The tower adds about 18% to encoder time, and shipping
  it would need a runtime branch and its own replay qualification.

**New human gates (Dev-iter primary humans, 570).** These come in addition
to D1–D4 and R1:

- **H1.** The uncalibrated human-only Brier, with balanced weights over the
  humans (the release evaluator's measure). The paired source-group upper
  bound against the release (seed 17) and against the release recipe (three
  seeds) must be at most 0.00753, the standard deviation of the measure over
  the release recipe's three seeds.
- **H2.** Dev-iter humans flagged at each fit's own CAL 1% and 0.1% points.
  The seed-17 count and the three-seed mean may exceed the release's by at
  most the release recipe's seed standard deviation of that count: 1.73 at
  1% (counts 11, 8, 8) and 2.31 at 0.1% (counts 0, 4, 0). D2's limit of 11
  at the 1% point still applies.

The margins are reseed scales, not tuned values. The release seed-17
control reproduced the release's CAL map, thresholds and counts exactly.

**Selection.** Passing arms are ranked by the v1.1 primary span metric.
Within an interval tie, the order S2c, S2a, S2b decides, which puts
unchanged document outputs first. The winner must beat S0+ with a paired
interval above zero. It is labelled a success if it is also within 0.0036
of S1+ (S1+'s seed standard deviation of the metric), and partial
otherwise. R1, the decoder (the v1.3 rule), the CAL span maps, a single
re-read of `span_report` (it is no longer fresh) and a retrospective
consumed-Test report follow, for the selected arm only.

**Gate check.** Run on the existing records, the gate tool reproduces the
release evaluator's per-seed human-only Brier bit for bit, and S1+'s
three-seed difference of +0.0319 [+0.0250, +0.0392]. H1 therefore fails
S1+ (seed-17 upper bound 0.0246, recipe 0.0392). H2 alone would not have
caught it on Dev-iter: S1+ seed 17 flags 1 human at its 0.1% point and 7 at
its 1% point, within the margins.

**Smoke.** The smoke used 900 Train rows (S2b 1,175 with the added
parents), at most 1% of the 122,248, and a 30-row Dev-iter subset. One
amendment came before it completed: the subset strata gained the generator
label, so that every roster label keeps Train supervision. Every check
passed:

- Without S2 options, the S2 trainer reproduces the v1.1 trainer exactly
  (final tensors and per-evaluation training terms).
- S2a's teacher equals the release fit on the evaluated rows (error 0).
- S2b found every parent (279 pairs), and the human mass is unchanged.
- S2c starts with S0+'s token logits exactly and keeps the document path
  bitwise frozen.

The host CPU was saturated during the smoke, so its step times do not
refine the estimate. No smoke number is evidence.

**Budget.** About 8.4 GPU hours (10 fits, scoring, export and smoke), with
a 12-hour ceiling.

