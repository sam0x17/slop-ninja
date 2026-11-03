# GPT-OSS generator compatibility probe

A's existing Train archive contains Qwen, Mistral and OLMo generations. Prepare
another locally runnable generator family for a future data expansion. Keep the
live capacity fits, six saved-model Train checks and saved hosted captures
unchanged. This probe does not select a training remedy from unfinished
fit diagnostics or establish detector improvement.

Use `mlx-community/gpt-oss-20b-mxfp4-bf16` revision
`f8c03dc936dbf66db17d3411a177f498fcdd9a87`. Verify every downloaded file's size
and upstream Git blob or LFS SHA256. Retain the model API response and per-file
SHA256 manifest. The [conversion card](https://huggingface.co/mlx-community/gpt-oss-20b-mxfp4-bf16/blob/f8c03dc936dbf66db17d3411a177f498fcdd9a87/README.md)
declares Apache 2.0 and OpenAI GPT-OSS 20B
as its base; it does not identify the exact upstream weight revision. Preserve
the [official license](https://huggingface.co/openai/gpt-oss-20b/blob/6cee5e81ee83917806bbde320786a8fb61efebee/LICENSE), usage policy and card at upstream revision
`6cee5e81ee83917806bbde320786a8fb61efebee` as license evidence, without claiming
that revision is the conversion's weight source.

Run the existing pinned MLX environment in a dedicated loopback process on the
Studio, with one prompt/decode at a time and no prompt cache. Do not use or
restart LM Studio. Bind the server code, model implementation, tokenizer code,
chat template and runtime packages. Retain invocation arguments and UTC dates:
the upstream template inserts a current date. Fix and record that template
context before a multi-day generation campaign.

The installed MLX tokenizer reports no supported reasoning channel for this
checkpoint. Its HTTP content may therefore contain serialized Harmony messages.
The explicit `harmony-text-v1` collector mode accepts a final channel alone or
an analysis message followed by one final channel, under this tokenizer's exact
control-token spelling. Preserve the final prose bytes and the whole raw HTTP
response separately. Reject missing final channels, tool/control transitions,
extra messages and unexpected markers. Do not infer the format from the text.
Omitting the mode preserves old ModelSpec serialization and generation keys.

After formatting, Rust tests and Clippy pass on Studio, issue exactly one
synthetic garden request through the Rust probe, at temperature zero and a
2,048-token completion limit. Retain failures and uncertain HTTP outcomes; do
not retry a POST automatically. Read the raw response and selected prose, check
channel boundaries, completion status and every supplied fact. Record added
claims and tonal/length limitations independently from execution success.
No corpus text or detector scores choose this request. No Pangram calls,
training admission or source-family split changes are part of the probe.

## Observed result

The [probe](results/gpt-oss-generator-probe-v1.json) completed one HTTP request
and produced a 120-word final paragraph. The raw content contained both analysis
and final Harmony messages. The Rust parser selected exactly the final body;
the full raw response remains archived on the Studio. Every downloaded file
matched the pinned manifest.

The [manual review](results/gpt-oss-generator-review-v1.json) found all six
supplied factual constraints preserved, alongside unsupported additions,
including a compost bin and path layout. The paragraph fails strict fidelity.
The compatibility prompt's specific prohibition on invented fees, organizers
and dates was weaker than the corpus generator's general prohibition on added
details. Future collection should use that existing stricter instruction and
still inspect outputs; neither wording nor a completed request proves fidelity.

The request used temperature zero and a 2,048-token limit. The saved model
specification contains prospective corpus defaults of 0.7 and 4,096 tokens;
those settings were not tested here. The response used 1,068 completion tokens
and took 277.69 seconds including prompt processing and initial loading, while
A training was active. This is not a throughput benchmark.

Rust formatting, all-target tests, the final collector tests and Clippy with
warnings denied passed on Studio. The final collector tests also verify exact
whitespace retention for both drafts and revisions, and hashing of the complete
raw response. The dedicated inference process exited after the request. No
training records, paid detector calls or A/B quality milestone resulted.
