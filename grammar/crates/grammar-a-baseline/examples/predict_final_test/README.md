# Frozen Test prediction and evaluation bridge

```text
predict_final_test NUMERIC FIT SELECTION NEW_OUTPUT_DIRECTORY
evaluate_supervised --frozen-test NUMERIC FIT PREDICTION_RECEIPT SELECTION NEW_REPORT [TEST_GALLERY]
```

Both routes validate selection-v3, an eligible candidate/run, frozen weights/fit/contract, the Test numeric projection and full population accounting. Joint weights require positive origin/share/fractional-share/human/AI supervision. Role labels and masks are checked against retained population metadata. No output can be inside any frozen fit/numeric directory or the supplied Test numeric bundle. Legacy evaluation behavior is unchanged.

The predictor writes `predictions.json` (the existing `[{id,prediction}]` array) and `receipt.json` (`slop_ninja_final_test_predictions_v1`, accepted by the Pangram comparator). Successful predictions include `encoding` from the same frozen numeric dispatcher. Numeric/model-load failures retain every affected ID in `excluded`; an all-failed run still publishes complete accounting with zero supported rows. Failure to export an encoding is separate from all-head prediction failure. Publication writes the complete receipt after its prediction array. This is numeric CPU inference only; no full text encoder, parser, provider or prose input is invoked.

The evaluator reads the receipt and actual frozen fit without placing predictions in FIT. It uses existing origin/share, conditional/open-set attribution, balanced and source-family bootstrap helpers. Metrics condition on successful predictions; upstream/projection/inference exclusions remain explicit. Empty support produces null metrics. The diagnostic Train prior uses the original frozen Train numeric shard, not Test. Thresholds/calibration and weights remain unchanged.

Optional Carson gallery handoff:

```json
{
  "schema":"slop_ninja_final_test_reference_encodings_v1",
  "complete":true,
  "selection_receipt_sha256":"...",
  "frozen_manifest_sha256":"<original candidate numeric manifest>",
  "contract_sha256":"...",
  "weights_sha256":"...",
  "fit_sha256":"...",
  "fit_performed":false,
  "normalization_fitted":false,
  "profiles_changed":false,
  "test_used_for_selection":false,
  "known_reference_sha256":"<original reference.jsonl byte digest>",
  "reference_records_sha256":"<Test dataset manifest references_sha256>",
  "query_dataset_manifest_sha256":"<query Test dataset manifest byte digest>",
  "partition_reference_sha256":"<Test numeric reference rows: sorted by id, encoding removed, serde compact JSONL with final newlines>",
  "rows":["<each complete reference numeric row plus encoding>"]
}
```

The consumer requires twelve original known Train reference rows unchanged except added encoding, plus the four exact Test reference IDs/families/writers from selection's source TSV. All loss targets must be null. Reference encodings must have the frozen model's encoding dimension. The four raw reference IDs/digest must agree with the Test dataset manifest's separate references fields. No query work/ID or Dev/CAL reference may enter. Each of eight writers has two excerpts. An incomplete gallery can be omitted; it cannot silently claim full coverage. The gallery producer owns exact reference transformation, input/encoding provenance, and the same frozen encoder qualification.

Pooling and retrieval call the evaluator's existing normalized-mean and cosine-ranking helpers. They remain diagnostics; classification scores are unchanged, and no generator-reference gallery is inferred.
