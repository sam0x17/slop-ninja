# Word and grammar development probe

Declared before fitting, 2026-09-11. The v2 encoder improved pooled test loss but
never chose the mixed-origin class on those tests. The earlier linear replay
identified excessive confidence at learning rate 0.03. This probe tests whether
smaller steps and preserved word coverage make the explicit features useful on
the style-diverse corpus.

Use only the v2 primary Training, Development and Calibration shards, with their
original family assignments and exact text. Combine them with the Rust merge
command and extract one combined spaCy feature cache. Do not load Test feature
rows, open further test predictions or generate replacement data in this probe.
All previous tests remain opened diagnostic material; this is development work.

Fit six deterministic full-batch Adam controls: word, grammar and combined at
learning rates 0.0003 and 0.001. Use the existing 300-epoch cap, patience 25,
L2 penalty 0.01, minimum document frequency two and RMS scale floor 0.01.
Keep an 8,192-coordinate cap for each standalone representation. The combined
model gets independent 8,192-coordinate word/bigram and grammar budgets, for a
total cap of 16,384. Unused capacity does not transfer between budgets. Verify
that both coordinate lists, document frequencies and scales exactly match the
standalone controls. This adds grammar without removing word coordinates.

Retain compact Training/Development observations for epoch zero and every fitted
epoch. Select each model's checkpoint by the original uncalibrated development
log loss and stopping rule. Report all candidates, including uniform selections
or failed classes. For a candidate to proceed toward confirmation, require a
trained checkpoint and positive development recall in every class, then rank
eligible candidates by development log loss. Also report the unrestricted loss
winner so that the effect of this minimal class-coverage gate is visible. The
gate is a diagnostic requirement, not proof of adequate class performance.

The existing trainer fits each candidate's temperature and thresholds on
Calibration after checkpoint choice. Those metrics do not enter candidate
selection. Preserve calibration outputs locally. No Pangram API calls or fresh
confirmation claims belong to this stage.

The Rust runner binds feature, executable, protocol and artifact hashes and
records the complete six-run result. Compare its development observations with
v2's selected encoder on the same Development families. Differences may justify
a later encoder/feature fusion experiment, but must first support a simpler
standalone model. Any further search or fusion needs its own recorded plan.
Fresh sources are required before promoting a revised reference.
