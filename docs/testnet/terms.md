# Testnet terms

Version 2, November 9, 2026. It replaces version 1, the unnumbered terms
published before this date. A commitment made on or after this date accepts
version 2; an earlier commitment stays under the version in force when it
was made.

These terms apply to taking part in SlopNinja on Bittensor testnet subnet
601: registering, committing, uploading an artifact, running a validator, and
using the testnet's published data. By committing or uploading an artifact you
accept them. They cover testnet only; mainnet terms will be published
separately.

## No monetary value

Testnet is a rehearsal. Test TAO, testnet alpha, scores, credits and weight
rows on subnet 601 have no monetary value. SlopNinja does not sell, buy,
exchange or redeem them, and taking part creates no claim to any payment,
token or mainnet allocation. SlopNinja does not provide test TAO.

## Your submission

- **Licence.** You license every artifact you commit and upload under the
  [Creative Commons Attribution-ShareAlike 4.0 International licence (CC BY-SA
  4.0)](https://creativecommons.org/licenses/by-sa/4.0/).
- **Your rights.** Submit only an artifact you have the right to license that
  way, including what it derives from: base models, training data and code.
- **Publication.** If your artifact is admitted to a cohort, the coordinator
  publishes it (its header and files) when that cohort's scoring starts, and
  never recalls it; it publishes your opening when the cohort closes. Anyone
  may download, run, copy and adapt a published artifact under the licence.
  SlopNinja may run, host, evaluate and redistribute published artifacts under
  the same licence, and does not promise to host any artifact indefinitely.
- **Before publication.** Until the cohort's scoring starts, your upload stays
  in the coordinator's private evaluation storage and is not readable through
  any public interface. Uploads that are refused, overflow the roster, are
  superseded by a newer commitment, or are aborted are deleted no later than
  the cohort's close and never published.
- **Copies.** Every published artifact counts as published for every later
  cohort, your own included: an entry whose inference content equals a
  published artifact's is not executed and earns nothing, and one within the
  near-copy distance of a published artifact or a reference fails the
  public-copy gate unless it scores more than half a band above it. Enter a
  new model in each cohort.
- **Your recipe.** You need not publish your training recipe or data.

## What is public

- Your hotkey, its UID and registration, and your on-chain commitment.
- When scoring starts, every admitted artifact.
- After the close: admitted entrants' openings and, for every executed entrant,
  its artifact digest, its quantized score (on the cohort's grid, with seeded
  noise) unless it is withheld, its band, one bit for whether it passed the
  gates and one for the public-copy gate, and its near-copy cluster, together
  with the coordinator's signed row that those values imply.

Exact scores, the reasons a gate failed, the hidden evaluation documents, their
labels and per-document predictions are never published; the signed score
package that holds the exact scores stays private to the coordinator. Every
text in the testnet human stock is public and can be recognized; until outside
miners have working artifacts, testnet cohorts reuse human documents that
earlier cohorts already used.

The coordinator keeps content-free records of upload sessions: digests,
lengths, states, refusal codes and commitment positions. They stay readable to
the signing hotkey at least until the cohort closes; after that the cohort's
published result is the record.

## The reference model

Testnet's reference is `R_0`. SlopNinja publishes its reference packages,
`R_0` among them, under the CC BY-SA 4.0 licence with their notices, once
public access opens ([reference packages](reference-packages.md)). The
SlopNinja name and logo are not covered by that licence. The reference
changes by the published promotion rule or by an operator reference decision
(below). One such decision is planned: replacing `R_0` by the next reference
SlopNinja trains, only if it passes every required gate, announced in advance
on the status page. Replacing the reference moves no reward already earned.
On testnet only, promotions run without the lookup canary, and the
leaderboard labels this; an operator drill of the blocking path, named in the
cohort's plan, can still block a candidate. The canary is required before
mainnet.

## Rules and changes

Each cohort runs under a signed manifest that freezes its rules, parameters and
gate thresholds; a cohort's rules do not change after it opens. Later cohorts
may run under new manifest versions. Testnet follows the whitepaper's reward
rule: no miner earns until an entrant beats the reference by the margin, and
until then the active target is the burn row, which pays no miner.
SlopNinja's validators commit that row on chain each tempo; the
[miner guide](miner-guide.md#13-rewards) says what that means.

## Operator decisions

SlopNinja, as the operator, may by a recorded decision:

- end a cohort before it closes (abandon it): it produces no target, so it pays
  no one, and its number is never reused;
- refuse to pay a finished cohort: its result never becomes the active target;
- restart the testnet record from `R_0`: the active target switches to the burn
  row, and winners of the earlier record are never paid again;
- change the reward rule, only through such a restarted record;
- set the current and previous references directly, with a recorded reason.

A decision changes only later work and payment: commitments already submitted
reveal under their original rules, and rewards already paid are never
reassigned. No outage triggers a decision. The testnet record was restarted
once, on November 6, 2026.

## No guarantee of service

The testnet service and software are provided as is, without warranty, as
their licences, listed in the Licence section of the repository README, state. SlopNinja does not
guarantee availability, timing, scoring or payout. Cohorts may be delayed or
paused, for example during a coordinator outage; a paused cohort resumes later
and is not skipped. Testnet may be reset, changed or stopped. A score or package
found to be wrong may be held for a signed correction.

## Conduct

Do not attack the coordinator, the status host, other participants or the
chain, and do not try to obtain hidden evaluation data. Report a security
problem privately to [security@slop.ninja](mailto:security@slop.ninja), not in a public issue
([security policy](../../SECURITY.md)). Report other problems through the
public repository's issue tracker.
