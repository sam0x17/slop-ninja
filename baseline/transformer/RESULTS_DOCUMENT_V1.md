# Document coverage: first engineering checks

The [document protocol](PROTOCOL_DOCUMENT_V1.md) prepared a 70,200-word synthetic
input on the Mac Studio in 6.41 seconds. Both overlapping views covered all
430,591 source bytes. Every exported window fit the reference A artifact's
2,048-token limit. No detector inference ran on this synthetic book, so this is
coverage and preparation evidence only.

The fixture consists of 100 numbered chapters, each repeating the MIT-licensed
[pilot tasks](fixtures/pilot-v1.json) twice. It is deliberately repetitive and
does not represent a complete authored book or a long-form accuracy benchmark.
The [machine-readable result](results/document-v1/coverage.json) records the
construction, input and plan hashes, artifact, platform and coverage status.

| View | Windows | Tokens per window, including specials | Unsupported windows |
| --- | ---: | ---: | ---: |
| Primary | 109 | 1,058-1,541 | 0 |
| Shifted | 109 | 1,154-1,541 | 0 |
| Complete source | 1 | 84,001 | 1 |

The 218 window requests remained unscored. The report correctly retained the
entire source as missing detector evidence in each view, and left document
probability and threshold conclusions null. Planned coverage never counted as
successful inference. The complete source was explicitly unsupported by A and
was never truncated or sent through its encoder.

The preparation command used 218,251,264 bytes of maximum resident memory as
reported by macOS `time -l`. This is one observed run, including tokenizer
startup, with no repeated timing estimate or detector throughput measurement.
The tokenizer configuration also warns about inputs exceeding its upstream
8,192-token setting; the released detector's stricter 2,048-token contract still
governs exported inference requests. Counting the whole source does not execute
the encoder.

A separate 46,205-byte Unicode fixture retained exact bytes across 34 windows.
It included combining accents, emoji, curly quotes, nonbreaking spaces, CRLF,
tabs and leading/trailing whitespace. An independent byte-mask check confirmed
complete coverage in both views and valid UTF-8 boundaries for every slice.

Validation ran on macOS 26.5.2, arm64: the 12 B tests, formatting, Clippy and a
release build, plus the installed spaCy integration for raw-text preservation.
CLI checks rejected changed artifact/runtime identities, inconsistent token
counts and malformed probability evidence. Missing and unsupported results
remained unscored. Linux was not rerun for this change.

Exact contextual scoring and cache-reuse checks also ran on private material.
Those source texts, identifying manifests and results remain in ignored private
storage. They do not contribute to the synthetic-book coverage counts above.
No new Pangram requests were made. No B accuracy milestone or whitepaper strength
update follows from these engineering checks.

The next benchmark must test actual complete works and compare equal-budget
editing strategies. Version 1 can invalidate many later windows after a token
insertion near the beginning. Stable section identities and incremental parsing
will be needed to keep repeated book editing affordable without reusing scores
for changed contexts. Cross-chapter meaning and author-voice checks remain open.
