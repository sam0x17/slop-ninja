# Style-diverse detector pilot

Frozen before generation and fitting, 2026-09-11. This tests a new training recipe
against the released first encoder on fresh source families. It combines varied
styles, two training generators and matched formatting exposure. It cannot
attribute a difference to any one of those changes.

## Sources and partitions

Acquire up to 80 additional PLOS abstracts and 80 additional historical Wikinews
excerpts using the existing rights/extraction rules. Record PLOS DOI-list offset
350 and Wikinews title start `D`; keep the original bounded discovery limits.
The PLOS endpoint remains the [publisher search API](https://api.plos.org/solr/search-fields/).
Historical Wikinews text retains its period-specific license under the
[publisher policy](https://en.wikinews.org/wiki/Wikinews:Copyright).
Check each record's actual license evidence, not just the discovery source.

Use `style_pilot plan` to exclude source URLs/families and exact or normalized
text already present in the first source collection or either adopted generated
cohort. Record every exclusion without replacing it. Freeze partitions with seed
`slop-ninja-style-pilot-v2`. Never reroll the seed for a more favorable sample.
This controls source-family reuse; shared authors/events and other near-duplicates
remain possible. Historical human proxies and model-edited proxies remain weak
origin labels. Private correspondence, book text and the Blog corpus are excluded.

Assign each source family to Qwen or Mistral by the recorded hash-ranked
round-robin rule within split/register buckets. Keep every family whole. The two
teacher input files retain all four frozen splits, so profile assignment occurs
before any Test-only filtering. Split counts and teacher/profile assignments are
frozen before any generation; publish their text-free manifests and hashes.

## Generations

Use the exact native MLX checkpoints/runtime from the first pilot. Generate one
source-conditioned draft and one light/moderate copyedit for each source family.
Use `style-mix-v1` profiles `source-matched,plain,informal,direct,anti-ai,fix-slop`
for Train, Development, Calibration and the primary Test. Profiles are balanced
within each teacher's split/register buckets, and siblings share a profile.

For a secondary profile-transfer cohort, use the same teachers and frozen Test
sources with reserved profiles `editorial,formal`. These profiles do not enter
fitting, checkpoint choice or calibration. This is a paired source comparison,
not a second independent sample or an unseen-generator test. Both teachers occur
in training. Report exclusions and cohort composition in each comparison.

Keep temperature 0.7, output cap 1,800 tokens, no request seed and concurrency one
per producer. Record requests, responses, instruction/profile/runtime hashes and
all admission failures. Use the existing complete-family exclusion rule, with
no score-based retries or substitutions. Maximum calls are twice the number of
admitted roots, plus twice the number of frozen Test roots. Shortfall requires
visible reporting, not silent acquisition beyond the declared bounds.

## Fit

Train one new ModernBERT-base three-class candidate from the pinned upstream
checkpoint. Use the first pilot's three epochs, seed 17, batch size four, AdamW
learning rate 0.00002, weight decay 0.01 and 2,048-token cap. Audit every raw shard
and the paired formatting view before fitting; reject overflow rather than
truncate or filter. Keep vocabulary, architecture and optimizer settings fixed.

Derive the fixed Unicode whitespace-collapse view using Rust with `--all-splits`.
During each epoch expose each training row exactly once, alternating raw and
collapsed views by source-family hash parity and epoch. All three origin siblings
share the exposure choice. The ML runner checks exact IDs, families, labels and
whitespace-only text correspondence. This changes neither sample weights nor
steps per epoch. Record the alternate shard hash and exposure counts.

Select the epoch using uncalibrated log loss on original Development text. Fit
one temperature and both nominal 1%/5% human-FPR thresholds on original
Calibration text. Preserve the first released encoder unchanged, including its
original calibration probabilities and thresholds. Do not refit that reference
on this experiment's calibration set. This experiment makes one prespecified
new candidate, with no parameter sweep or automatic promotion.

## Evaluation and decision

After the new artifact, calibration and epoch selection are frozen, evaluate
both encoders on the primary and reserved-profile Test cohorts, original and
whitespace-collapsed views. Use the existing Rust evaluator for all eight reports.
Each model uses its own original calibration exports and training-prior controls;
thresholds remain identical across that model's four views.

Primary comparison: three-class log loss on the original primary Test, with
Brier, accuracy, confusion and per-class recall. Report the other three views as
prespecified transfer/formatting controls. Report each teacher and prompt profile
with its denominator where supported. For the model-or-assistance event, report
sensitivity and observed human false positives with source-family intervals.
The same human text can appear in paired cohorts; never count it as new evidence.
These small samples cannot establish rare population error rates or Pangram parity.

Publish outcomes and all failed controls. Improved point estimates alone do not
qualify a subnet detector or establish broad superiority. Keep the first release
available. Treat these tests as opened diagnostic material after this round; use
fresh confirmation sources for later tuning. No Pangram API calls are included.
