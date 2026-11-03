# Frozen final-Test assembly interface

`assemble_balanced_final_test PLAN.json NEW_OUTPUT_DIRECTORY`

The original non-Test assembler is unchanged except for one visibility-only helper export. This assembler does not acquire sources, execute models, or contact providers. Its implementation/tests must not be run on Test artifacts until separately authorized. Every input file pin below is an object with absolute `path` and exact `sha256`.

```json
{
  "schema": "slop_ninja_balanced_final_test_assembly_plan_v1",
  "recipe": "default_v2_narrative_scratch_20_60_100",
  "selection": {"path":"/absolute/selection-v3.json","sha256":"..."},
  "selection_receipt_sha256": "...",
  "source_plan": {"path":"/absolute/original-72-work.tsv","sha256":"..."},
  "source_manifest": {"path":"/absolute/acquired-Test/manifest.json","sha256":"..."},
  "query_manifest": {"path":"/absolute/extracted-Test/manifest.json","sha256":"..."},
  "humans": {"path":"/absolute/extracted-Test/humans.jsonl","sha256":"..."},
  "references": {"path":"/absolute/extracted-Test/references.jsonl","sha256":"..."},
  "overlap_ledger": {"path":"/absolute/overlap-ledger.json","sha256":"..."},
  "generator_source": {"path":"/absolute/frozen/a_balanced_generate.rs","sha256":"..."},
  "generator_binary": {"path":"/absolute/frozen/a_balanced_generate","sha256":"..."},
  "generation_protocol": {"path":"/absolute/generation-protocol.md","sha256":"..."},
  "generators": [
    {"slot":"known_model_1","identity":"<exact Qwen identity>","config":{"path":"/absolute/qwen.json","sha256":"..."},"server_properties":{"path":"/absolute/qwen-props.json","sha256":"..."},"run_directory":"/absolute/qwen-run","requested":128},
    {"slot":"known_model_2","identity":"<exact Phi identity>","config":{"path":"/absolute/phi.json","sha256":"..."},"server_properties":{"path":"/absolute/phi-props.json","sha256":"..."},"run_directory":"/absolute/phi-run","requested":128},
    {"slot":"heldout_Test_only","identity":"<exact Mistral identity>","config":{"path":"/absolute/mistral.json","sha256":"..."},"server_properties":{"path":"/absolute/mistral-props.json","sha256":"..."},"run_directory":"/absolute/mistral-run","requested":128}
  ],
  "requested_human_records":32,
  "requested_generation_records":384,
  "requested_records":416,
  "requested_reference_records":4
}
```

All generator identities are constants in the assembler. Known-model configs may retain the original four allowed splits or narrow to `["test"]`; Mistral must allow only Test. Actual rows and reports must be Test-only. No Gemma, v3/train-scale recipe, new sampler, or replacement source is admitted. Source reservations match the exact compiled TSV (SHA-256 `3271a46bf0b1dd70a2429ee5a9ce3002fba8c8235f7a629219162c46471f903f`) and the selection receipt.

The mandatory external pre-admission ledger is metadata-only:

```json
{
  "schema":"slop_ninja_final_test_overlap_ledger_v1",
  "complete":true,
  "test_text_read":false,
  "selection_receipt_sha256":"...",
  "coverage":["original_cohort","all_train_extensions","all_references","prior_admissions_and_exclusions","historical_v6_reservations"],
  "protected_files":[{"scope":"original_cohort","path":"/absolute/pinned-input","sha256":"..."}],
  "protected_ids":[],
  "protected_source_groups":[],
  "protected_work_aliases":[],
  "protected_text_sha256":[],
  "protected_normalized_text_sha256":[],
  "protected_scratch_prompt_sha256":[]
}
```

Every coverage scope must have protected files. Include every selected candidate's `population.json` in `protected_files`, even when several candidates share one file; one entry for that path suffices. Their hashes are independently matched to frozen candidate numeric manifests. Other files bind the cumulative reference/admission/exclusion/historical reservations audit. The external ledger producer is responsible for complete projections of those inputs; assembly verifies their byte hashes and applies all supplied collision sets, not an independent re-extraction of historical corpora. Report any unavailable historical text hashes in additional audit fields; never read reserved Test prose to fill them.

Each protected array contains unique nonempty strings. Digest arrays contain lowercase SHA-256. Work aliases are `work:<source_group>`, `pg:<decimal id>` and `title:<lowercase alphanumeric characters only>` (Unicode lowercase; discard every non-alphanumeric character). Title checks cover both the full title and the portion before the first `:`, `;`, or `(`, each with and without an initial `The `, `An `, or `A `. This matches the cumulative auditor. Hash exact UTF-8 text; normalize text only by splitting whitespace and joining with one ASCII space, retaining case/punctuation. Scratch hashes cover the exact original-v2 prompt string, without alias/model or JSON wrapper.

Protected sets contain conflicting prior populations/reservations, **excluding the intended eighteen current Test reservations**. Those eighteen are independently matched to the immutable TSV. Preserve reserved/consumed historical Test hashes as metadata only. Cross-split ID/work/alias/exact/normalized/prompt collisions stop assembly; they cannot be repaired by dropping only the Test copy.

Acquisition must supply all eighteen works. Extraction failures remain explicit: a missing query excerpt accounts for that human request plus its twelve unattempted generation requests; no fabricated provider attempts or terminal failures. Each run summary distinguishes planned128 from actual requested tasks. Reference failures stay separate from the416-query denominator. Actual accepted/terminal tasks must exactly cover the available query roots. Within-family duplicate/no-change generated rows may be excluded with full records retained; collisions across Test work families or with references stop assembly.

Outputs: `records.jsonl`, `references.jsonl`, `excluded-records.jsonl`, `exclusion-audit.jsonl`, immutable plan/selection snapshots, `REPORT.json`, and final `manifest.json`. The supervised manifest uses `slop_ninja_a_supervised_dataset_v1`, accepted query-only `test_ids`, empty `non_test_ids`, exact upstream accounting, and `selection_receipt_sha256`; it is consumed by the frozen projection and comparators. References are never merged into query records or generation inputs. Every generation evidence path/hash remains retained through the manifest; original attempt files are never modified or deleted.
