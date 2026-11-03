# Phi-4 generator compatibility

Measured 2026-09-11. A pinned MLX Phi-4 conversion completed one synthetic local
HTTP request, returning 140 tokens in 3.1964 seconds with a normal stop and zero
cached prompt tokens. This checks runtime compatibility; no corpus text or
detector evaluation was used. The output included stock phrasing and unrequested
elaboration, so it does not establish fidelity.

Phi-4 is a candidate for a withheld-generator comparison against the detector
trained on Qwen, Mistral and OLMo. Its outputs have not entered Train,
Development, Calibration or Test. Any subsequent corpus experiment must declare
its role before generation and preserve whole source families and prompt-profile
assignments. Generation and dataset admission now accept the exact reviewed
Phi-4 revision, license URL and grant hash under MIT. Other MIT-labeled generator
specifications still require separate review. The original Apache 2.0 lane is
unchanged; a license string alone does not admit an arbitrary new model here.

Microsoft's [model card](https://huggingface.co/microsoft/phi-4/blob/2db69c1c3e91a05d2c64a3185acfbaf36f744e25/README.md)
describes a 14B-parameter model under MIT. The pinned
[license file](https://huggingface.co/microsoft/phi-4/blob/2db69c1c3e91a05d2c64a3185acfbaf36f744e25/LICENSE)
has SHA256 `c49419617a6070bcb197cfe272f7007fdec3e790dbb529cb995473bd69c0bd51`.
Retain the Microsoft copyright and permission notice. The MLX conversion is
`mlx-community/phi-4-4bit@fc0f8f23d369dc29b55cad1d65cb5bf0dcbee910`.
All 11 repository files passed size and hash checks against that revision's
Hub tree: SHA256 for the two LFS weight shards and Git blob identities for the
smaller files. The quantization uses four bits and group size 64.

The [probe report](../results/phi4-generator-probe-v1.json) records exact file
hashes and runtime versions. Serving used native MLX-LM 0.31.3 on the M5 Max,
with local files, remote tokenizer code disabled, prompt caching disabled and
one request at a time. The synthetic prompt requested a roughly 120-word
community-garden explanation from five supplied facts, at temperature zero and
a 384-token output limit. Raw request, response and HTTP timing remain in ignored
local storage. The owned server was stopped after the request.

The probe made no Pangram calls. Runtime compatibility and a permissive weight
license do not independently certify generated facts or qualify a detector.
