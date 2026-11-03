# Recovery of the interrupted origin-consistency experiment

The original control fit stopped during epoch four. Its last flushed progress
record covers 2,368 of 3,833 rows in that epoch. On restoring Studio access,
the original supervisor and fitter were absent, no matching training process
was running, and the host uptime established a subsequent reboot. The precise
termination time and exit status were not captured. Neither candidate produced
a final artifact; the consistency fit had not started.

Preserve the original directory, logs, input bindings and temporary selected
weights. The temporary weights are from an earlier selected epoch, not the
interrupted training step. The training code saves neither optimizer state nor
random-state and row-position data sufficient for faithful continuation. Do
not package those partial-run weights as a completed four-epoch candidate.

Restart the full paired experiment once in a new directory,
`origin-consistency-recovery-v1`, from the original base checkpoint. Keep the
[original protocol](ORIGIN_CONSISTENCY_V1.md), source code, seed, four epochs,
optimizer, paired exposures, loss weights and selection/calibration rules
unchanged. Run control and consistency sequentially. This is recovery from a
verified interruption, not a search over seeds or a choice between attempts.
Retain both completed candidates and the interrupted attempt in the report.

Before launch, verify the original file bindings, copy the frozen code and
pairing manifest, and bind the recovery wrapper and record of the interruption.
Only execution/output paths change. Record new supervisor identity, logs and
terminal status. The original directory receives no writes. Do not restart or
alter any unrelated Studio service.

The original public/private comparison plans stay preserved. Prepare new
comparison plans against the recovery directory using the same scoring rules
and frozen data. Freeze both completed candidates' calibration and thresholds
before opening diagnostic scores. The recovery changes neither test membership
nor the meaning of previously inspected diagnostics. No new Pangram calls or
reference-detector promotion are authorized by this recovery record.
