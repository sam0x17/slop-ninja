# V6 execution placement amendment

On September 12, 2026, the user required all remaining model generation,
training and evaluation to run on the Mac Studio. The
[v6 comparison protocol](ENCODER_REVISION_V6.md) remains frozen at SHA256
`7063b6bfec139761768d8b1c71b9ba378652122c4cc570adca3e977e257f98e4`.
This amendment records the change of execution host. It changes no source
assignment, prompt, admission rule, fitting recipe or selection criterion.
No v6 detector fitting, calibration or predictions preceded this amendment.

Primary Mistral Fix Slop generation paused through the generator's supported
`PAUSE` file. The in-flight request finished and checkpointed before the
generator exited. Of 330 tasks, 159 were accepted, three were rejected and
168 were unattempted. The controller and model server were stopped after
that drain. The controller's final local runtime hash check was interrupted;
its exit status was 143, and that post-run check is not certified complete.

Resume only the 168 unattempted tasks on the Studio. Preserve all closed
cache entries, including rejections, and verify their hashes before and
after resuming. Keep the input, model weights, generator, adapter, model spec,
temperature, token limit and task identities unchanged. Do not retry failed
tasks or redraw accepted outputs. Keep the same loopback URL on the remote
host because the frozen generator includes that URL in run identity.

The model spec also includes the original host description in cache identity.
Retain its exact bytes to resume those tasks. For records produced after the
move, the execution receipt must identify the Studio host and bind its runtime
verification and task keys. That receipt supersedes the original spec's host,
hardware and OS description for those keys. The retained spec alone must not
be presented as an accurate description of the resumed execution environment.
Changing GPU hardware does not establish bitwise equivalence of generated
outputs, even with matching model and package files.

Before remote generation resumed, verification matched all 649 existing cache
and run files and all 6,312 files in the recorded Studio model/runtime inventory.
A zero-call dry run confirmed the same 330 tasks and 168 pending requests.
The resumed execution receipt has SHA256
`a3f14d8ffe8b19ccaa0e9a50932be73f1386ac3a0c67307d5a30175bce520294`.
The Studio uses an M3 Ultra with 256 GiB of memory and macOS 26.5.2.
The receipt identifies the 168 task keys assigned to the resumed segment.

Keep the complete original local segment and its pause receipt, the remote
verification, and the resumed segment in ignored experiment storage. Carry
their bindings into final assembly and results. Freeze the actual remote
training environment before fitting. Use dedicated project processes; do not
reboot the Studio, restart LM Studio or interrupt unrelated workloads.
