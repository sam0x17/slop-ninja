# Formatting distribution in the frozen Train archive

The completed Train-fit review found a large V6 score change when a single
leading space was removed. Audit formatting across the complete original Train
archive before changing preprocessing or choosing another fitting recipe.

Use all 3,833 original records and their existing collapsed-whitespace versions.
Verify exact hashes, source groups, labels, parent IDs and the declared Unicode
whitespace conversion. Keep historical human-proxy evidence unchanged. Do not
read other partitions or select examples by scores in this audit.

Count leading ASCII space, leading/trailing Unicode whitespace, LF, exact LF-LF,
CR, tab, nonbreaking space, narrow nonbreaking space, repeated ASCII spaces and
differences from the canonical view. Distinguish cases changed only by trimming
outer whitespace from cases whose only change is removal of one leading ASCII
space. Keep these definitions literal; LF-LF is not a claim about paragraph syntax.

Report counts and rates by origin, human-root collection/origin, and generating
model/operation/origin. Include source-family counts and equal-family/origin
rates, so repeated revisions do not silently dominate every comparison. Treat
the resulting differences as possible data confounds, not causal attribution
of a neural prediction or a new accuracy estimate.

Run this descriptive Rust audit on the Studio. Save aggregates, identities and
checks without source text. Change no labels, examples, fitted models, thresholds
or current training recipes. It requires no new inference or provider calls.
