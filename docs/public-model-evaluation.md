# Public-model evaluation

[Whitepaper draft 0.64](../whitepaper/slop_ninja.pdf) and the
[Cloudflare design](cloudflare-subnet-draft.md) define the current proposal.
Launch admits public *A* artifacts and uses coordinator-led hidden evaluation.
The coordinator signs the weight row its private scores imply, and every
validator verifies that row and commits it through native commit/reveal. *B* and *C* activate
separately. This guide summarizes evaluation policy; the
production runner still requires implementation and qualification.

## What publication provides

An admitted *A* model publishes its weights and everything needed to execute
its declared calculation when its cohort's scoring starts. Its terms let anyone obtain it for
free, run it locally, and reuse it to train a private *B* model. A published
artifact stays public. SlopNinja removes one only on legal grounds, by a
recorded operator action with a content-free public record of the removal and
its legal basis, and the removal changes no score, row or payment
([testnet terms](testnet/terms.md#legal-notices-and-removals)). The coordinator deletes its
private admission copy once the cohort closes and publication is recorded, and
keeps the current and previous reference artifacts hosted for evaluation while
they are references. Training data and recipes need not be
published where the applicable rights permit them to remain private. Optional
hosted service has separate costs and terms; publishing a model does not
require the miner to operate an inference endpoint.

The coordinator caches qualified artifacts and executes them on common hidden
inputs under equal limits. This avoids sending benchmark jobs through miner
endpoints. It retains predictions, failures, input identities, and execution
settings for signed result records. The score package and receipts stay
private to the coordinator, which derives the reward allocation and the
canonical native row and publishes a quantized result from which anyone can
recompute that row. Validators verify the coordinator's signed row and commit
it independently; the coordinator's finalized row is a later consistency
audit. Other validators receive no held-out documents,
labels, or per-document predictions and cannot reproduce hidden evaluation. Independent users can run public
artifacts on their own inputs.
Honest coordinator scoring and curation remain trust assumptions.

Reproducing an artifact establishes its calculation, not detection accuracy.
A public model can reproduce a deliberately permissive response to a trigger.
Qualification therefore includes independently labelled holdouts, hidden
production histories, source-family separation, formatting controls, targeted
triggers, and common-owner attacks. Retire benchmark material before authorized
release. Public weights establish neither original training nor the absence
of collusion or memorized examples.

## The artifact contract

One shared learned encoder supports origin, AI-share, AI-span and attribution
outputs. The [model contract](../whitepaper/sections/03-models.tex) fixes their
meanings and qualification. A weight file alone is insufficient. An *A*
artifact is a raw-text model: an ONNX graph within one of the manifest's three
allow-listed graph classes and its parameter bound, a fixed tokenizer that must
match the allow-listed one byte for byte, the output heads, and their
calibration. Its token-level head, optional until a manifest enforces the span
gate, gives per-token AI probabilities, decoded into AI spans with exact
character offsets and per-span confidence. Under the 24-word floor no span
under 24 words is trained, evaluated or decoded, and artifact spec v5 refuses an
artifact without a token-level head or with a decoder minimum under 24 words. Its
execution manifest binds the canonical text profile, feature meanings, reference
pooling, calibration, numerical precision and tolerances, dependencies, allowed
operators, resource limits, and reference outputs. *A* uses no CFG-derived
inputs; the dated [viability experiment](cfg-vector-viability.md) and
[text/CFG fusion](a-v6-cfg-fusion.md) results record why. The native grammar is
a later *B* input.

One shared learned encoding interface is required; useful feature sharing and
its benefit remain hypotheses.
Matched controls must measure positive and negative transfer; graph inspection
does not show whether heads use common useful features. Adverse results inform
an explicit design revision rather than an entrant-specific exception.

The main intended human profile comes from a customer's independent writing
samples. Trusted preprocessing represents each sample by the *A* encoder's
output and stylometric features (function-word frequencies and character
n-grams) and pools them into one private writer profile under a qualified,
versioned pooling contract. A normalized weighted mean is the proposed starting
point; the contract fixes feature scaling, per-work weights, missing-channel
handling, and the encoder, feature, and pooling versions. Test sample length,
topic, register, and variation lost in averaging. CFG writer vectors scored
below the encoder and are not a profile channel
([writer-profile comparison](cfg-vs-encoder-results.md)). No profile type
qualifies yet: a learned matcher trained on fresh authors, with thresholds
measured on withheld writers and real style-transfer data, must pass before
`provided_writer` is offered. The named `provided_writer` result ("Provided
writer") follows the ordinary match rule and leaves `other_human` available when
no human profile qualifies. It identifies a supplied profile, not a verified
person. AI share/confidence and both role probabilities remain separate outputs.
Keep these references and profiles private to their job or customer; publication
of *A* does not publish customer profiles. Secondary human galleries may use
documented public/licensed references under a provenance and permitted-use policy.

The admitted scorer has no network access, online learning, undeclared mutable
state, or cross-job prompt cache. Query inputs exclude explicit author IDs,
miner UIDs, assignments, hidden labels, and source/candidate-role flags.
Interface restrictions do not establish that the text itself contains no
benchmark cues. Qualify numerical behavior and resource bounds on the actual
execution runtime; another backend requires its own equivalence evidence.

Inference-semantic changes create a new artifact version. Registration binds
inference content separately from signatures and owner metadata; changing a
wrapper does not establish a new model. Within a cohort, identical inference
content, or predictions within the near-copy distance on the batch, forms one
cluster that shares one credit equally, and the finalized commitment order
breaks ties between equal quantized scores. Detected copies of earlier
published artifacts earn nothing. Admission order does not establish who
originally trained a model.

## Scoring and external comparisons

Every entrant receives the frozen comparison population. Missing predictions
remain in coverage and failure accounting. AI-involvement Brier utility is the
only reward quantity; share, attribution and spans are qualification gates with
their own measurements. The span gate compares each entrant with SlopNinja's
first span package on the same batch under the 24-word floor, with margins
fixed after measurement on closed testnet cohorts, and runs in report mode on
live cohorts before any manifest enforces it; report mode and artifact spec v5
are built but not yet deployed, so no cohort has yet evaluated spans. See the
[scoring rules](../whitepaper/sections/07-rewards.tex).

Compare the incumbent anchor, eligible *A* models, and Pangram on the same
independently labelled hidden set under declared operating points and
uncertainty analysis. The anchor provides a reference for quality claims and
the coordinator's paid-service activation decision; it supplies neither a
reward weight nor an automatic activation gate. The operator funds shared
provider observations once per set. A superior result needs fresh confirmation before a future anchor switch.
Pangram fractions cannot directly replace *A* involvement probabilities, and
provider agreement supplies no ground-truth label. The
[external-comparison guide](pangram-oracle.md) records the policy and evidence.

Cohorts retain frozen semantics across compatible operational repairs. A runner
failure, coordinator outage, or missed wakeup cannot justify selecting a new
favorable attempt or discarding assigned work. Each validator handles weight
submission and recovery independently under the
[cohort recovery rules](../whitepaper/sections/07b-cohorts.tex); adoption never
gates cohort progress or service.
A public artifact and successful local replay alone do not qualify a live subnet.

## Existing evidence and later B

The [V6 release](../baseline/detector/releases/v0.6.0/README.md) is a raw-text
origin detector used as a research comparison. It implements neither the
proposed shared *A* model nor its AI-share and attribution outputs. The
[platform guide](platform-support.md) records its tested runtime boundary;
the [paper's evidence section](../whitepaper/sections/10-evidence.tex) separates
those results from future model qualification.

Later private *B* would use Cloudflare-hosted adapters and operator-controlled
ranking, with weights private from the public and customer content protected
from miners under a qualified provider configuration. Public *A* lets *B*
optimize against the detector at its own compute cost. Held-out generators,
source families, and later detector versions measure transfer; low detector
scores cannot certify preservation or author fit. The
[later B proposal](../whitepaper/sections/07a-adjudication.tex) defines the
candidate objectives and remaining activation choices without requiring TEE
execution evidence.

For personal-voice editing, *B* uses the same fixed writer profile and continuous
*A* author-fit signal to rank successive revisions. Freeze the evaluator,
profile, and calibration for each comparison. Qualify any mapping to the public
style distance; its calibrated rank is not an authorship probability. Test
adaptive metric gaming against independent author-fit judgments and separate
preservation, readability, and tone review. The learned scorer receives the
profile; trusted preprocessing sees the samples and the editing
pipeline still needs the source prose. None is disclosed to serving miners.
