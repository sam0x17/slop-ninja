# Chain adapter rules for contributors

[`AGENTS.md`](../../AGENTS.md) states the runtime rules and the `rtao` pin
and links here for the detail. [`subnet/README.md`](../../subnet/README.md)
(sections "rtao pin" and "Runtime upgrades") holds the evidence; whitepaper
Section 14 states the implementation boundary. Update this page, the README
and the whitepaper together when the pin changes.

## The pin

The native chain adapter pins rtao `fa83052d37b39f002f7a4d3cc5c04f25503e1d11`
(immutable tag `snapshot-2026-11-05.2`; pin the full commit, never a branch) in
every crate that uses rtao (`slop_ninja_subnet::RTAO_PIN`; a test checks every
manifest against it). It uses rtao's durable journal (schema 3, migrating
schema 1 and 2 journals on open; schema 4 only after a compaction, which nothing
here performs) and its weight/reveal recovery APIs, with no spec-number
refusal; rtao records no validated runtime range, and nothing here requires one.

## What the adapter relies on at this pin

- rtao composes weight calls and reads the storage of its weight preflight,
  scheduler and reveal watcher through compiled descriptors while their live
  shapes match and by name otherwise. What it can read neither way is recorded
  as a deferral, never a refusal keyed on a runtime version.
- rtao follows the chain's stored commit-reveal version under any runtime code.
  The coordinator takes its payload version from rtao and follows the stored
  tag the same way, so after a tag change both commit the same row.
- Work can opt in to committing an owner burn row (`OwnerRowPolicy::Submit`,
  journaled with the work). The validator uses `Submit` only for the burn row
  under a valid launch authorization or a burn target, after checking
  `MinAllowedWeights`, `MaxWeightsLimit` and its own validator permit in rtao's
  weight preflight, and holds every other owner or self row.
- rtao reads the owner hotkey's UID at the block each attempt is prepared at.
  The validator retires work rtao holds as `OwnerRowHeld` and defers that
  tempo; the next start re-derives it against the finalized registrations.

## Rollback

A build at the previous pin `0c128160` opens `Submit` work, followed-tag state
and retired work. A pending reveal whose subnet reveal period changed blocks a
rollback until that reveal settles; a schema-4 journal or a retained finalized
failure allows only roll-forward. Before any rollback, stop the validator and
run its `rollback-check [--retire]`; it exits 0 only when an older build can
open the journal.

## Not qualified here

Journaled MEV-shield dispatch (rtao supports it from per-runtime evidence),
the v482 port (its upstream semantic review is still open) and any runtime
range remain unqualified here; the adapter submits unshielded and never
bypasses journaling to enable shielding.

## Live evidence

The native validator's live commits, per-tempo re-commits and fault drills
passed on testnet subnet 601 at the pin `b5553a76`. At `0c128160` both of
SlopNinja's validators committed, revealed and read back the launch burn row on
601 (2026-11-03). At `fa83052d` the native validator, rebuilt and reset by hand
at the testnet record restart (2026-11-06), verified the new launch
authorization and committed and revealed the burn row, each reveal equal to the
committed row.
