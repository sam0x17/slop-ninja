# Scorer token table

- `scorer-tokenizer-v1.bin` (`slop_ninja_scorer_tokenizer_table_v1`, SHA-256
  `018a069d786d8ba964061117839e1c938c12b2abfb8c3985a52dcce6999a64c6`, 408,878 bytes):
  the merges, byte map, added tokens and pre-tokenizer classes of the pinned A
  `tokenizer.json` (SHA-256 `6c8aaa9a...c08d30`, the ModernBERT-base tokenizer
  shipped in the release `runtime-base/`, Apache-2.0), written by
  `../reference` (`table`). Layout: `Table::parse` in `../src/lib.rs`.
- `golden-v1.json`: synthetic edge cases only, with the scorer's counts from
  tokenizers 0.22.2 (onig) `encode(text, true)`, written by `../reference`
  (`golden`).

Rebuild both only when the pinned tokenizer changes, then update
`TABLE_SHA256` and `SOURCE_TOKENIZER_SHA256` and run `reference sweep` and
`reference parity` before the crate's tests.
