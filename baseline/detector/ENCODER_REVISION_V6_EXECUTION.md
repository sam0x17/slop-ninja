# V6 evaluation handoff

`revision_handoff` waits for the original Studio frontier to close, then runs
the already frozen calibration and evaluation commands. It does not change
the [v6 protocol](ENCODER_REVISION_V6.md), train another candidate, submit
Pangram requests or promote a research artifact to the released reference.

The [command manifest](manifests/v6-postfit-commands-v1.json) binds this specific
experiment's paths, tools, inputs and observation views. Its SHA256 is compiled
into the coordinator. The manifest contains 22 commands: one threshold freeze,
14 Test inference runs and seven paired comparisons. Each model is evaluated
on R0, R1 and R2 for both the primary and Phi-4 routes, plus the separate
117-human-root diagnostic. The complete and common-support views are identical
on both routes, as verified before fitting.

Before waiting, the coordinator checks the command manifest and static file
hashes. It records the original supervisor's PID, start time and command line.
It sends no signals and does not restart a missing or changed producer. A
successful completion receipt must identify the expected final frontier stage.

After fitting, it rechecks the frozen inputs and tools, then independently
checks all four epoch summaries for each declared rate. Selection requires
positive human and model recall and minimum uncalibrated Development binary
log loss. Exact ties favor the earlier epoch, then the lower learning rate.
Incomplete histories, inconsistent eligibility, a different winner or no
eligible candidate stop the handoff before inference.

The selected artifact's original Calibration archive and terminal view supply
its thresholds. Its fitted temperature remains unchanged. Before any Test
command, the coordinator records both artifact file sets, thresholds, input
views, tool hashes, selection, command manifest and execution-host attribution
in `pre-test-bindings.json` and `pre-test-bindings.sha256`. It records opening
intent and both binding-file hashes separately and verifies
those bindings again after all comparisons. Each command retains its exact
arguments, stdout, stderr and exit status. Failures retain partial outputs and
require inspection; the coordinator never retries an evaluation automatically.

Run only on the machine that holds the frozen archive and the producer. The
coordinator refuses to start unless `hostname -s` matches the short host name
given by `--required-host` or the `REVISION_HANDOFF_HOST` environment
variable; with neither set it refuses to run. Capture the identity of the
already verified frontier supervisor under `LC_ALL=C`, then use a new control
directory:

```sh
LC_ALL=C ps -p PRODUCER_PID -o lstart=,command= > producer-identity.txt
export REVISION_HANDOFF_HOST=EXECUTION_HOST
revision_handoff --plan commands.json --producer-pid PRODUCER_PID \
  --producer-identity producer-identity.txt --control-dir preflight-v1 --check-only
revision_handoff --plan commands.json --producer-pid PRODUCER_PID \
  --producer-identity producer-identity.txt --control-dir execution-v1
```

The check-only command verifies the live producer and static inputs without
waiting or inference. Use a detached, logged process for the execution command.
The checked-in manifest describes the actual Studio archive layout; it is not
a portable dataset download. A different experiment requires its own declared
plan and review.

The coordinator passed the Rust test suite, format checks and all-targets
Clippy on the Studio at 2026-09-13T00:17:57Z. Its check-only run then passed
against the original live supervisor and the frozen archive. The detached
handoff was started at 00:19:56Z and initially reported
`waiting_for_original_frontier`; this is a launch observation, not evidence
that calibration or Test evaluation has completed.

Completion means the frozen evaluation commands finished and their input
bindings still match. Review the resulting metrics and uncertainty, complete
the separate Pangram comparison, and publish the research artifact with its
attribution before making any claim of improved detector quality.
