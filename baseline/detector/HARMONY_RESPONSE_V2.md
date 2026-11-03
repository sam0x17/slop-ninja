# Repeated analysis messages in saved generator responses

The fifth GPT-OSS pilot profile exposed a collector limitation: one raw response
contains three consecutive assistant analysis messages before its single final
message. `harmony-text-v1` rejects that sequence because it admits at most one
analysis message. The original rejection remains part of the frozen pilot.

Add an explicit `harmony-text-v2` response format for future model specifications.
Accept a final channel alone or consecutive assistant analysis messages followed
by exactly one final channel. Preserve final-body bytes, apart from the already
declared optional terminal return token. Require the observed channel and message
delimiters; reject other roles, recipient headers, tool markers, commentary,
missing final messages and any message after the final. Do not infer the format
from arbitrary prose. Iterate through the messages without recursion.

Keep V1 behavior and old ModelSpec serialization unchanged. Selecting V2 changes
the specification and therefore its generation cache identity. Do not change
the running pilot's format, regenerate its requests, replace a rejected record
or admit text retrospectively through this parser change.

Replay all 20 saved responses from the first five frozen profiles, including
the plain-profile source-copy exclusion and the anti-AI format exclusion. Bind
every response and the expected final-text hash before running the new parser.
Require identical final bytes wherever V1 already succeeds. The new parser's
acceptance of an existing response establishes format compatibility only; length,
copying, record validity, semantic fidelity and training admission remain separate.

Use synthetic variants and rejection contrasts for roles, channels, control
tokens, incomplete messages and duplicate finals. Run formatting, all-target
tests, Clippy with warnings denied, and saved-response replay on the Studio.
No model inference or detector call is needed. Record Linux execution separately.
