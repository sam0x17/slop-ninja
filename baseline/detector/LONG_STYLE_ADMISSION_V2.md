# Long-output admission amendment

This amends generation admission in [the length/style comparison](LONG_STYLE_COMPARISON_V1.md).
Keep its source allocation, model specifications, exact prompts, output-token
cap and manual-review requirement. No detector scores have been opened.

The first completed pilot response had 1,887 lexical words, for a source with
1,443. The generator rejected it under the legacy absolute 800-word ceiling.
That ceiling was incompatible with the declared long-source experiment. The
pilot was paused after discovering the error, allowing the in-flight request
to finish. Preserve the original run and all its responses and errors.

Use the explicit `source_relative_length` campaign option, passed to
`generate_samples --source-relative-length`. Admission then requires at least
80 lexical words and between half and twice the source's word count. This
retains the existing relative-length bounds and removes only the absolute
800-word maximum. Unchanged controls still reject incomplete generation,
reasoning/formatting wrappers, unchanged text and excessive source copying.
Detector token limits are assessed separately on complete texts. They never
authorize truncating or redrawing a response.

The option is recorded as `output_length_policy: source-relative-v1` in the
generation run and summary. Default legacy behavior and legacy run identity
remain unchanged. Resume checks forbid changing admission within a run.
Prompts and response-cache keys stay unchanged because admission does not
alter the request sent to the model.

Use a new execution directory. Import every closed raw attempt from the paused
pilot, recording its old path, request/response/status hashes and original
failure. Preserve that old evidence; do not delete its error or regenerate its
text. Reassess complete recorded responses under this amended rule without a
network call. Failed or interrupted provider requests remain failures. Only
previously unattempted tasks may make new calls, within the original 36-call
pilot allowance. Record any further failure rather than redraw it.

The first response also failed manual fidelity and register review: it changed
the victim of a poisoning, added motives and philosophical claims, and replaced
parts of a factual summary with lyrical interpretation. Passing the corrected
length rule will not certify that response as a faithful transformation.
Retain that failure in the pilot denominator and inspect the remaining pilot
before deciding whether this generation design supports confirmation.
