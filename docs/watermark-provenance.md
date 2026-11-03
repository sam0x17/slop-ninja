# Watermark provenance in the corpus

Checked on 2026-09-08 after the user's correction. Anthropic documents text
watermarking for Fable 5.1 and Mythos 5.1 across its products and partner platforms.
Its current support article describes marking for older models as a rollout in
progress. This supports recording documented watermark deployment separately
from provider and model identity. It does not establish the watermark status of
our individual older Opus samples. [Current coverage](https://support.claude.com/en/articles/16266773-how-claude-marks-ai-generated-content).

Anthropic describes a keyed statistical pattern in token choices, using a version
of SynthID-Text, with no extra hidden characters. It explicitly distinguishes this
from Pangram's methods, which do not have Anthropic's key. A Pangram score cannot
by itself establish watermark presence, removal, or its causal contribution to
detection. [Technical explanation](https://www.anthropic.com/news/claude-text-watermark).

Keep raw generation records intact. Record documented deployment and unknown
sample status as supplementary provenance, rather than retroactively asserting
watermark detection. The current preface round uses Codex editors and does not
claim that its inputs or outputs are watermark-free.
