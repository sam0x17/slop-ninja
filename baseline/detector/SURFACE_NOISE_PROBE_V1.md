# Paired surface-noise sensitivity

The leading-space probe established a V6 failure on generated text. Test whether
other easily introduced surface mistakes have similar effects. Keep the current
capacity fits and reference models unchanged.

Use all 126 records from the same 16 exposed Train families as the boundary-space
probe, retaining every human, model-only and mixed descendant. The baseline is
the exact source with outer whitespace trimmed, preserving internal bytes. For
each record attempt these four interventions independently, never cumulatively:

1. Insert a second ASCII space at the first internal space between alphabetic
   characters. Leave existing paragraph boundaries intact.
2. Replace the first eligible common word with its fixed transposition:
   `the` with `teh`, `with` with `wiht`, `which` with `whcih`, or `because` with
   `becuase`. Match whole words in source order; preserve title/uppercase forms.
3. Duplicate the first whole-word `the`, with one separating space. This tests
   a repeated determiner as a bounded grammar error.
4. Repeat the final ASCII question or exclamation mark once. Periods, closing
   quotation marks and trailing other content are ineligible for this rule.

Record ineligible interventions and their denominators. Never substitute another
rule after seeing a score. Preserve exact before/after spans and hashes. These
rules apply across origins under identical eligibility criteria, but eligibility
rates can differ by collection or origin. Report those differences rather than
calling the resulting counts balanced automatically.

Before inference, manually inspect every edit in its containing sentence and
surrounding context, including possible quotation or code contexts. Retain
questionable cases with an explicit interpretation caveat. Check that the
baseline and candidate differ only at the declared span. The typo and repeated
determiner usually preserve recoverable meaning, but the protocol does not
declare semantic equivalence for every context. Repeated expressive punctuation
may change emphasis. This tests origin sensitivity, not acceptable B output.

Score baseline and candidate views under frozen V6, base control and base
consistency artifacts, using their original calibration and CPU float32 eager
runtime, batch one and one thread on the Studio. No truncation, generation,
Pangram calls or automatic retries. Require all three runs to finish before
interpreting quality results. Retain all three-class probabilities, signed and
absolute paired changes, class loss, original-cutoff counts and crossings in
both directions. Summarize separately by intervention, origin and collection,
with equal-source-family means alongside row means. Do not pool unmatched
eligibility subsets as if they were identical treatments.

Small deltas alone are insufficient: a detector that assigns every text a high
AI probability would be insensitive but wrong on human text. Preserve absolute
class performance alongside sensitivity and inspect source-family variation.
The panel is already exposed Train data; no result qualifies a replacement A
or establishes Pangram transfer. After the fixed capacity comparison, use these
findings to specify an independent training comparison with the same corruption
policy across origins, explicit consistency loss and source-disjoint evaluation.
Do not label surface errors as inherently human or penalize all grammatical
differences indiscriminately. Meaningful revisions need their own evaluation.
