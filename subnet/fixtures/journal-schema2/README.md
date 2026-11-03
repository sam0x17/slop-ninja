# Schema-2 journal migration fixture

`journal.json` is a native weight journal written by `slop-ninja-subnet` built
against rtao `aa8627ba8f3691ec1ecd14b11b612b0f86479e6c`, whose `OperationJournal`
used schema 2. It was produced through the real journal API on 2026-10-29 by a
one-off ignored test on a copy of the integration branch at `3b549bab`, built
with the then-existing `testnet_qualification` feature, and copied out
unchanged. The data is synthetic:

- chain identity: testnet genesis, netuid 4242, hotkey `AccountId([2; 32])`;
- one operation for cohort `synthetic-cohort` revision 1, marked
  `scheduler_handed_off`, with no attempts;
- one operation for revision 2 holding one `Unresolved` signed attempt whose
  48 bytes are all `0xA5` and whose hash is their BLAKE2b-256
  (`0x4441ae2f…41ee`), recorded at spec 481 as an observation;
- the retired testnet runtime-qualification extension
  `slopninja.testnet-runtime-qualification.v1`, which is retained as opaque
  evidence and has no effect.

`schema2_journal_migrates_in_place_and_retains_work` opens it with rtao
`b5553a76` (schema 3) and checks that the identity, both operations, the
attempt bytes, hash and outcome, and the legacy extension survive, that the
original cohort work is recognized as existing work, and that the file on disk
becomes schema 3. `unsupported_or_lossy_journals_are_refused_without_reset`
checks that a newer schema and a colliding legacy field are refused with the
original bytes unchanged.

SHA-256: `217756d6a2733f6a63625e710b2d844c7bf87bd5e125b06acfabdfd328737345`
