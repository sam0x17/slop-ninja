# Miner models and training data

The [whitepaper](../whitepaper/slop_ninja.pdf) and the
[Cloudflare design](cloudflare-subnet-draft.md) define the current proposal.
Launch includes *A* alone. *C* grammar contributions and *B* transformation
require separate activation decisions. This guide describes model and data
boundaries; the paper owns reward policy. To take part on testnet subnet 601,
start with the [testnet miner guide](testnet/miner-guide.md).

## A: detection and attribution

*A* is one text model with a shared learned encoder. A miner commits its
artifact's digest on chain and uploads the artifact privately; the coordinator
publishes it after the cohort ends. Submissions are licensed CC BY-SA 4.0.
Slop Ninja publishes its own reference packages, `R_0` among them, under the
same licence ([reference packages](testnet/reference-packages.md)), so testnet
miners can start from ModernBERT-base and public data or from `R_0`'s encoder.
Its outputs include:

- The estimated share of final words written or rewritten by AI.
- Calibrated confidence that any AI involvement occurred.
- Conditional human and model attribution, including each role's involvement
  probability and a named profile or `other_human` / `other_ai` fallback.
- AI spans with exact character offsets and per-span confidence, decoded from
  per-token AI probabilities, with per-sentence share and confidence derived
  from the same probabilities.

Involvement Brier is the reward; share, attribution, and spans are
qualification gates. The launch reference has document heads only, so span
outputs stay optional until a cohort manifest enforces the span gate.

Share and confidence are separate percentages. Binary production-history
labels train confidence; fractional share needs measured fractional evidence.
Keep expected share between model-only probability and involvement probability.
Both attribution roles are returned without a presence cutoff. The fallback
means no named profile qualified; it does not establish whether that role
participated. A completed result has no attribution-abstention label.
Execution failure is a separate result.

The shared encoder is a deliberate common-representation requirement. Test
whether origin and attribution improve each other using matched controls;
structural admission does not establish positive transfer. Harm would require
an explicit design reconsideration, not an exemption for individual entrants.

The coordinator evaluates frozen submissions on hidden data and signs the
results. Validators derive canonical native rows from the signed per-miner
scores and calculation inputs, then commit independently using native
commit/reveal. The coordinator's finalized row is a later audit; other validators receive
no held-out data. Slop Ninja serves published, qualified artifacts in its own infrastructure.
Miner submissions must bind the model, preprocessing, output schema,
calibration, and qualified execution profile. The
[model contract](../whitepaper/sections/03-models.tex) and
[scoring policy](../whitepaper/sections/07-rewards.tex) specify the complete
requirements. An accurate origin classifier alone does not qualify the shared
model's other outputs.

## Input contract and parsed-vector research

The launch input contract is raw text: an artifact is an ONNX graph with a fixed
tokenizer, heads, and calibration, and cohorts feed it canonical text. Beyond
its declared parameters it carries only the tokenizer, calibration, and contract
files; an embedded text index or lookup table fails admission. A model
that learns from CFG-derived word and grammar vectors remains a research
hypothesis. Its first real-data test failed, and adding CFG to text gave only a
small, uncertain gain ([viability](cfg-vector-viability.md),
[fusion](a-v6-cfg-fusion.md)). Parsing coverage, ambiguity resolution, and
trained predictive performance are separate measurements.

Freeze tokenization, parsing, resolution, feature meanings, and reference
pooling with each experiment. Fit vocabularies and learned transformations on
training data. Keep unsupported inputs visible; neither a partial parse nor a
zero-vector substitute establishes document coverage. A later CFG input
profile would begin a new comparison.

The main intended human-attribution workflow lets customers provide independent
samples of their own writing. Trusted preprocessing parses and projects each
sample into a compatible CFG vector. The proposed starting profile is a
normalized weighted mean characterizing that writer. Qualify and freeze pooling,
per-work weights, masks, grammar/schema, and projection; measure length, topic,
and register sensitivity and information lost when varied works are averaged.
The learned model receives the profile vector rather than raw reference prose.
The upstream parser still reads those references.

A qualifying named match to this private profile is `provided_writer`, displayed
as "Provided writer". It is scoped to the job or customer and follows the same
match rule as other named profiles. It establishes similarity to the supplied
references, not the customer's identity or authorship. When no human profile
qualifies, return `other_human`; both role probabilities and AI share/confidence retain
their meanings. Samples and profile vectors remain private query data, with no
automatic public-gallery enrollment or training reuse. Public/licensed human
profiles remain permitted under a documented provenance, identity-basis, and
permitted-use policy; public availability alone does not establish those rights.

## B and C

The [later B proposal](../whitepaper/sections/07a-adjudication.tex) combines
bounded edit proposals with ranking and whole-document review. Private rankers
would run in Slop Ninja's runtime, with private adapters served through direct
Workers AI requests to miners' Cloudflare accounts. Its scoring authority,
semantic review, privacy qualification, and emissions remain activation work.
For the intended personal-voice workflow, *B* iteratively proposes and ranks edits
using continuous author-fit feedback from *A* against the fixed writer vector.
The categorical match alone cannot rank improvements. Freeze the profile,
evaluator, and calibration throughout the job. Editing still requires the source
prose; trusted preprocessing sees the references, and serving miners receive
neither samples nor derived vectors. Higher author fit supplies no preservation,
readability, or tone verdict and cannot change the work's production history.
The existing [style utility contract](reward-adjudication.md) records candidate
research formulas without defining a live service.

*C* would credit accepted reusable grammar contributions once. Contributions
must improve the shared parser without hard-coding a document's text. The
[mechanism policy](../whitepaper/sections/07c-grammar.tex) governs prospective
credit; development campaign admission alone creates no emission entitlement.

## Existing research evidence

The [V6 public detector](../baseline/detector/releases/v0.6.0/README.md) is a raw-text ModernBERT
research comparison. It estimates three origin classes, not AI-written word
share, and implements neither the proposed shared *A* outputs nor author
identification. Its released runtime and dependencies remain useful controls.

The [author-projection experiment](author-projection.md) compared
`3557 -> 128` and `3557 -> 256 -> 128` models with the full word/grammar metric.
Neither replaced that reference. The
[Global Voices transfer evaluation](global-voices-transfer.md) also found that
frozen grammar contributions could reduce accuracy on a new source. These
results motivate comparisons across sources, including word-only,
grammar-only, and combined inputs. They do not establish that the native CFG
vector is ready for production.

Raw-text decoder baselines remain useful for authorized revision research.
Record the checkpoint, tokenizer, license, runtime, decoding settings, and cost
for each experiment. A proposer is distinct from the proposed learned vector
ranker; its results cannot validate that ranker without a matched comparison.

## Training and evaluation data

Use documented human-only, model-only, and mixed production histories. Preserve
source ancestry, exact text, author identities where authorized, and generation
or editing records. A model rewrite of human text retains human ancestry and
has a mixed production history. A low external-detector score supplies no
production-history label or semantic approval.

Keep source families, reference works, and evaluation queries separated across
fitting and held-out comparisons. Author evaluation needs independent works
and topic/genre controls. Fractional share supervision needs justified span or
word contributions; coarse workflow labels cannot invent them. Gallery
construction and attribution calibration must include contributors that have
no named gallery profile.
Personal-profile tests require independent reference/query works and check
sparse or heterogeneous samples, wrong identities, and adaptive attempts to
game the author-fit score. An enrolled profile does not make its source works
available for unrelated training.

[Training-source acquisition](miner-training-sources.md),
[public datasets](public-datasets.md), and [corpus design](corpus.md) document
available material and its permitted uses. The Blog corpus and private
reference writing remain research data under their own terms. Public artifacts
record the provenance and permitted uses of every training source and upstream
model. Customer material enters training only through separate opt-in.

Run substantial experiments as bounded, durable jobs. The
[platform guide](platform-support.md) separates target configurations from
tested research runtimes.
