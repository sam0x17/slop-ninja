# Repeated analysis messages: parser replay

The Rust collector now supports the explicit `harmony-text-v2` format described
in the [protocol](HARMONY_RESPONSE_V2.md). It accepts consecutive assistant
analysis messages before one final answer. V1 behavior and the default path are
unchanged. Future users must select the new mode in their model specification;
that selection also changes the generation cache identity.

The [frozen replay packet](results/harmony-response-v2-packet.json) includes all
20 responses from the first five GPT-OSS profiles. V1 accepts 19 response formats;
V2 accepts all 20. The [replay](results/harmony-response-v2-replay.json) confirms
identical final bytes for the 19 previously accepted formats. For the remaining
response, V2 selects the same 223-word final answer identified during manual
review, following three analysis messages.

The parser still rejects other roles, recipient headers, commentary, tool and
unexpected control markers, missing finals and extra messages after the final.
Two focused tests cover repeated messages, Unicode/whitespace retention and
rejection contrasts. Existing V1 and cache-identity checks also passed.

Formatting, all-target tests, Clippy with warnings denied, the build and the
saved-response replay passed on the Studio. The
[validation record](results/harmony-response-v2-validation.json) binds the
compiled source, packet, executable and check logs. Linux execution is unverified.
No heavy work ran on the laptop and no new model request was needed.

Format acceptance is separate from the collector's later checks. The original
plain-profile copy exclusion and anti-AI format exclusion remain in the pilot;
this replay creates no replacement corpus record. All 20 final passages have
recorded fidelity defects. The change prepares future collection without
claiming improved A quality or semantic reliability.
