# GPT-OSS Fix Slop profile and completed pilot

The last profile in the [frozen pilot](GPT_OSS_TRAIN_PILOT_V1.md) completed all
four requests. Two produced collector records, one exhausted its token budget,
and one returned an unsupported commentary message before the final. Manual
review read every final-channel body, including the partial and excluded bodies.
Full reasoning traces were not read for this profile.

| Source | Result |
| --- | --- |
| Novel summary | Invented betrayals, harsh winters and a culminating confrontation; omitted role definitions and added repeated interpretive conclusions. |
| Missing-data explanation | Retained much of the explanation but narrowed qualifications and added a mandatory methodological recommendation. |
| Cardiac-cell abstract | Stopped midphrase after 80 final-body words at the 4,096-token completion limit. The partial text already changes timing and qualifications. |
| Awards report | Preserved most awards but dropped two nationalities and the losing song's Grammy status; added evaluative commentary. An intervening commentary channel caused collector rejection. |

The commentary transition differs from the anti-AI profile's repeated analysis
messages. Both V1 and the new V2 format policy reject commentary; V2 does not
silently broaden acceptance to this case. The incomplete abstract remains an
execution failure, rather than a completed output with a measured fidelity rate.

The [review ledger](results/gpt-oss-train-pilot-fix-slop-review-v1.json) records
the source obligations, observed final bodies and limits of the reading. The
[binding check](results/gpt-oss-train-pilot-fix-slop-check-v1.json) verifies all
source/request/response identities, available records, excluded outcomes, final
hashes and word counts. No rejected attempt was repaired or retried.

All 24 planned attempts have now received manual final-body review. Across the
six requested profiles, 20 passed collector admission. One failed the source-copy
check, two failed response-format checks and one was incomplete. All 23 complete
final passages have strict-fidelity defects under the source obligations. These
are assistant judgments on four exposed roots, not independent human annotation
or a broad model-quality estimate.

This does not erase the recorded generation provenance. Future A datasets can
retain known model outputs with explicit fidelity, realized-style and execution
annotations under a declared admission policy. The pilot cannot establish that
the prompts changed style while preserving meaning. Requested style is not a
verified output attribute, and an instruction to remove AI writing habits does
not establish that those habits were removed.

Before expanding the source-conditioned collection, compare generation methods
that preserve the same factual obligations, including a condition without a word
count target and one that checks proposed changes against the source. Treat
these as hypotheses; this pilot did not isolate either intervention. Keep the
current A fits and evaluation unchanged. No pilot record enters active training
automatically, and the pilot made no detector or Pangram calls.
