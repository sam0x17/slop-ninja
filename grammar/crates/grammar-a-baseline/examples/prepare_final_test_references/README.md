# Frozen Test reference gallery

```text
prepare_final_test_references select SELECTION FINAL_DATASET_MANIFEST REFERENCES.jsonl NEW_INPUTS
prepare_final_test_references project SELECTION CANDIDATE NEW_INPUTS/records.jsonl NEW_INPUTS/manifest.json REF_CFG_EXPORT REF_EMBEDDINGS_OR_DASH NEW_NUMERIC
prepare_final_test_references encode SELECTION CANDIDATE SEED NEW_NUMERIC NEW_GALLERY.json
```

Each command validates selection-v3 before opening evaluation inputs. Outputs must be new and outside frozen numeric/fit directories. `select` admits the four exact Test-withheld reference IDs, two excerpts from each of two independent works. It preserves input bytes as `records.jsonl`, publishes a separate reference-only dataset `manifest.json`, and emits sorted label-free `{id,text}` `requests.jsonl` with `request-manifest.json`. These requests feed the existing CPU V6 extractor. CFG export must bind the unchanged reference records; query exports cannot enter this route.

`project` uses existing frozen CFG/ordered projection and text-input helpers. Pass `-` instead of embeddings for CFG candidates. Warm candidates preserve raw text activations; MLP candidates use their frozen normalization. Text candidates require V6 support and use their architecture's constant zero CFG block, retaining CFG failures diagnostically. Combined candidates require both inputs. No fitting or enrollment occurs, and all reference targets remain null. Exactly twelve known Train numeric reference rows are copied unchanged from the candidate's frozen parent. Additional Test rows appear separately in `partition_references.jsonl`; `reference_population.json` accounts for all four requests.

`encode` accepts fixed seeds 17/29/41, binds the selected weights/fit/contract and calls `NumericSharedModel::encode` on both sets of references. The result uses `slop_ninja_final_test_reference_encodings_v1`, accepted by the optional gallery argument of `evaluate_supervised --frozen-test`. It retains input numeric rows and hashes, requested/supported counts and exclusions. Only a 16-row gallery is complete; consumers must not silently substitute an incomplete gallery. Pooling remains the evaluator's existing arithmetic mean followed by L2 normalization. Test-withheld retrieval names do not expand classification heads.

The query assembler must supply `complete`, `selection_receipt_sha256`, `test_text_opened`, `test_used_for_selection`, `references_sha256`, `reference_ids`, `requested_reference_records`, `separate_reference_records`, `reference_exclusions`, `test_ids`, `split_assignments`, and `source_manifest_sha256`. It owns query/generation/extraction accounting and provenance validation. Its manifest is retained byte-for-byte as `query_dataset_manifest.json`.

Synthetic checks cover admission/request hashes, separate partition gates and masked targets, frozen CFG transformation including all-failed input, modality support, gallery isolation, both encoder schemas, fixed seeds and output boundaries. The reference example and affected query/assembly routes pass the focused offline checks; these checks execute no live models or provider calls.
