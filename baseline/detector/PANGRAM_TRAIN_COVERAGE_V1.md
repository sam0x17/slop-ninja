# Exact coverage of saved Pangram observations in V6 Train

Audit the five already collected local-generator Train cohorts against the four
original V6 shards. Bind each archive by its published SHA256 and expected row
count. Validate source families, provenance and ancestry. Match both record IDs
and exact text hashes; check the recorded origin, evidence, parent and split.
Also search the annotation hashes across all four shards, so a same-text alias
cannot silently cross the evaluation boundary.

Recheck successful provider stages, returned-text hashes, the declared echo
normalization and fraction ranges/sums. Retain the existing tolerance of
0.00001 for the sum of fractions and the 100% histogram bin. Preserve the raw
fractions in the local joined observations. Do not turn a content fraction into
a probability of model authorship or replace an independent origin label.

Count exact Train coverage and unannotated records by origin, human-root
collection, final generator, generation depth and prompt profile. Include the
denominator of all 3,833 Train rows, including the 471 human roots, and retain
families as the unit of independence. Report annotations in the six existing
score bins, plus exact-zero and below-10% counts. Keep cross-revision routes and
generator identities available for later diagnosis. Aggregate reports may be
committed; row-level provider observations remain in ignored storage.

Use the existing attribution identity tuple of requested model, declared
revision and reported model. The requested `default_model` alias alone groups
different Studio models together. Retain an identity lookup beside the generator
and route counts; preserve runtime and quantization in the joined attribution.
An absent structured prompt profile remains unrecorded, even when a prompt or
earlier experiment description suggests a style.

The five inputs are the pilot, second, third, fourth and fifth local-generator
cohorts. Hosted captures, Development annotations, V6 Test confirmations and
private B passages are outside this audit's supervision pool. This does not
claim a census of every historical provider call. There are no new calls,
new training weights, fitted models, changed split assignments or reads of
the still-running Train-fit diagnostic's quality metrics.

Use the result to assess how much varied teacher evidence we actually have.
These adaptively collected observations cannot establish prevalence or unseen
performance. Sparse or saturated labels may constrain a later provider-surrogate
experiment. A future teacher-supervised fit needs a separate declared protocol,
as in [the collection plan](PANGRAM_CORPUS_EXPANSION.md). This audit makes no
training or publication decision.

Run the Rust audit and an independent exact-match/count check on the Studio.
Keep the frozen larger fits and all six Train-fit runs unchanged. Read their
outcomes under their original protocols before choosing a training remedy.
