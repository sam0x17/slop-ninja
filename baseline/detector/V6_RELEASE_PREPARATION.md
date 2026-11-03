# V6 release attribution

The two v6 learning-rate candidates use the same frozen primary corpus. Source
credits can therefore be prepared before selection finishes. This step does not
choose a model, evaluate Test or qualify a release.

Run the Rust extractor on the Studio from `baseline/detector/`. The output's
parent directory must exist, and the output file must be new:

```sh
cargo run --example v6_source_credits -- \
  --run-root ~/slop_ninja_runs/revision_expansion_v6/detector_v6_v1 \
  --protocol-sha256 3a90c59834fb733b377a709da0c26887b40fbfe8fdf99d5542077e9ec777f79b \
  --output /path/to/new/source-credits.json
```

The expected protocol hash is recorded in the frozen
[evaluation command plan](manifests/v6-postfit-commands-v1.json). The extractor
checks the protocol, rights manifest and all three fitting shard hashes. It
includes Train, Development and Calibration records with complete ancestry;
the selected artifact's training metadata identifies the actual fitting and
selection views. It never opens the Test shard.

Each credit retains the record identity, source URLs, attribution, license,
change notices, generation metadata and parent ID. The extractor omits text,
raw prompts and local acquisition paths. It checks declared training and model
release permissions, ShareAlike notices, record uniqueness, family separation
and parent presence. These checks verify consistency with the admitted corpus;
they do not establish rights independently of its source evidence.

The output records the input, compiled helper source and executable hashes.
Preserve it with the extraction log and exact helper when assembling the release.
The [model distribution policy](SHARE_ALIKE_POLICY.md) also requires the weight
license and upstream checkpoint notices. Bind the completed attribution file to
the selected artifact and its final evaluation report before publishing the
research bundle. Until that review finishes, v5 remains the released reference.

The first extraction completed on the Studio on 2026-09-13. It contains 4,903
records from 685 source families and checks 3,618 ShareAlike notices. The saved
`source-credits.json` has SHA256
`fb1c81a6b655c696bc18da175cc9f1dd04ab90912409d8d849b1baad30e9a493`.
Formatting, the detector crate's tests, all-targets Clippy and the example build
passed before extraction. The full attribution file remains in release staging
until the model and its evaluation are ready.

## Evaluation report

After the original evaluator completes, assemble its saved evidence on the
Studio with a new output directory:

```sh
cargo run --example report_v6_release -- \
  --run-root ~/slop_ninja_runs/revision_expansion_v6/detector_v6_v1 \
  --output-dir /path/to/new/release-report
```

The helper requires the original successful completion, pre-Test bindings and
opening receipt. It checks the exact programs, arguments and success receipts
for all 22 declared commands, then verifies the frozen artifacts, thresholds and
source coverage. It stops before reading Test reports or creating output if the
original evaluation is incomplete.

The resulting `report.json` retains fourteen evaluations and seven paired
comparisons: R0, R1 and R2 for both routes, plus the all-human-root diagnostic.
It recomputes overall three-class and human/model metrics from saved probability
rows and checks their denominators and operating-point bindings. It reuses the
original paired bootstrap outputs, checking their source report hashes and loss
deltas. All slices and comparison outcomes are retained. No model inference,
threshold fitting or Pangram calls occur during report assembly.

Public projections omit per-record probability rows and cutoff/calibration
identifiers. Declared run-root paths become `${RUN_ROOT}` references. Other raw
text fields or private paths cause failure. File hashes refer to the unchanged
source files; `original_content_sha256` binds the complete original threshold
content, including identifiers omitted from `content_projection`.

The separate `verification.json` identifies the output and the checks performed.
Report assembly leaves the result marked for scientific review. The fresh
[Pangram comparison](PANGRAM_REVISION_V6_REPORTING.md) must still be reviewed and
included alongside this evidence before the research bundle is published. The
outer archive packager will bind these completed reports to the selected encoder
and source credits; preparing this helper does not make that release complete.

The helper compiled and passed the required crate checks on the Studio on
2026-09-13. Its initial preflight rejected the incomplete evaluation without
creating output. After the original evaluator completed successfully, report
assembly checked all 22 commands and produced fourteen aggregate evaluations
and seven paired comparisons. The [report](results/encoder-revision-v6.json)
and [verification](results/encoder-revision-v6-verification.json) preserve that
evidence. Its report SHA256 is
`e3f110f0c51d91969e3fcdcf666d755d9a3ce0aa417ddaecb0314bc55b568206`.

## Research bundle

After reviewing the completed evaluation and fresh Pangram comparison, write
`README.md`, `NOTICE.md` and `RESULTS.md` in a release-notes directory. Include
the observed tradeoffs, failed-observation denominators, source overlap and
limits on transfer. Retain the component notices required above. Commit the
public results and prose so the bundle can identify their full source commit.

The Rust packager expects the completed outputs in `release-report-v1`,
`pangram-confirmation-v1/encoder-subsets-v1` and
`pangram-confirmation-v1/comparison-v1` under the declared run root:

```sh
cargo run --example package_v6_release -- \
  --run-root ~/slop_ninja_runs/revision_expansion_v6/detector_v6_v1 \
  --runtime-verification /path/to/selected-runtime/verification.json \
  --notes-dir /path/to/completed-release-notes \
  --source-commit FULL_40_CHARACTER_COMMIT \
  --output-dir /path/to/new/slop_ninja_detector_revision_v0.6.0
```

Use the selected encoder's successful synthetic-only CPU replay receipt. A
receipt for the other learning-rate candidate will be rejected. The packager
checks the completed evaluation, verified aggregate hash, original selection,
manifest, runtime receipt, all 162 assigned Pangram texts, six stage views and
six paired subset comparisons. It requires the same artifact and operating
points in the full evaluation and Pangram comparison. Failed Pangram results
remain in the aggregate; packaging does not require a favorable result.

The output preserves every manifest-bound encoder byte and adds the full
evaluation, Pangram aggregates, standalone operating points, source credits,
component notices, protocols, runtime receipt and release prose. JSON projections
replace paths under the declared run root with `${RUN_ROOT}`. Unexpected text,
individual score fields and private paths cause failure. Original input hashes
and output checksums remain separate. Corpus passages and individual Pangram
reports are not read by this step.

Verify `SHA256SUMS` on the Studio before archiving. Packaging creates a research
bundle; it performs no model/API calls and does not publish, promote the model
or assess whether the results prose is accurate. Review the complete bundle
before the GitHub release. An interrupted attempt retains partial output; use
a new directory for a corrected attempt.

The initial packager passed formatting, the detector crate's existing tests,
all-targets Clippy and compilation on the Studio on 2026-09-13. Its preflight
rejected the incomplete original evaluation without creating output. The final
packaging step requires the completed evidence and reviewed results prose.

Inspection of the selected artifact confirmed that its model card is
`MODEL_CARD.json`. The packager now requires that filename, matching the
manifest, instead of `MODEL_CARD.md`. The corrected helper passed formatting,
the existing tests, all-targets Clippy and compilation on the Studio; its
frozen binary is in `release-package-tools-v2`. Use that corrected helper for
the final bundle.
