# Testnet terms

These terms apply to taking part in Slop Ninja on Bittensor testnet subnet
601: registering, committing, uploading an artifact, running a validator, and
using the testnet's published data. By committing or uploading an artifact you
accept them. They cover testnet only; mainnet terms will be published
separately.

## No monetary value

Testnet is a rehearsal. Test TAO, testnet alpha, scores, credits and weight
rows on subnet 601 have no monetary value. Slop Ninja does not sell, buy,
exchange or redeem them, and taking part creates no claim to any payment,
token or mainnet allocation. Slop Ninja does not provide test TAO.

## Your submission

- **Licence.** You license every artifact you commit and upload under the
  [Creative Commons Attribution-ShareAlike 4.0 International licence (CC BY-SA
  4.0)](https://creativecommons.org/licenses/by-sa/4.0/).
- **Your rights.** Submit only an artifact you have the right to license that
  way, including what it derives from: base models, training data and code.
- **Publication.** If your artifact is admitted to a cohort, the coordinator
  publishes it and its opening after that cohort closes. Anyone may then
  download, run, copy and adapt it under the licence. Slop Ninja may run, host,
  evaluate and redistribute published artifacts under the same licence, and
  does not promise to host any artifact indefinitely.
- **Before publication.** Until the cohort closes, your upload stays in the
  coordinator's private evaluation storage and is not readable through any
  public interface. Uploads that are refused, overflow the roster, are
  superseded by a newer commitment, or are aborted are deleted no later than the
  cohort's close and never published.
- **Your recipe.** You need not publish your training recipe or data.

## What is public

- Your hotkey, its UID and registration, and your on-chain commitment.
- For every scored entrant: its artifact digest, exact score, each failed gate
  with its reason code, its cluster, credit and integer weight units, in the
  signed score package of its cohort.
- After the close, admitted artifacts and their openings.

The hidden evaluation documents, their labels and per-document predictions are
never published. Every text in the testnet human stock is public and can be
recognized; until outside miners have working artifacts, testnet cohorts reuse
human documents that earlier cohorts already used.

The coordinator keeps content-free records of upload sessions: digests,
lengths, states, refusal codes and commitment positions. They stay readable to
the signing hotkey at least until the cohort closes; after that the signed
package is the record.

## The reference model

Testnet's reference is `R_0`. Slop Ninja publishes its reference packages,
`R_0` among them, under the CC BY-SA 4.0 licence with their notices, once
public access opens ([reference packages](reference-packages.md)). The Slop
Ninja name and logo are not covered by that licence. The testnet reference may
be replaced at most once outside the
promotion rule, by a better qualified reference that passes every required
gate, with advance notice on the status page. Replacing it moves no reward
already earned. All other reference changes follow the published promotion
rule.

## Rules and changes

Each cohort runs under a signed manifest that freezes its rules, parameters and
gate thresholds; a cohort's rules do not change after it opens. Later cohorts
may run under new manifest versions. Testnet follows the whitepaper's reward
rule: no miner earns until an entrant beats the reference by the margin, and
until then the active target is the burn row, which pays no miner. Slop
Ninja's validators commit that row on chain each tempo; the
[miner guide](miner-guide.md#13-rewards) says what that means.

## No guarantee of service

The testnet service and software are provided as is, without warranty, as
their licences, listed in the Licence section of the repository README, state. Slop Ninja does not
guarantee availability, timing, scoring or payout. Cohorts may be delayed or
paused, for example during a coordinator outage; a paused cohort resumes later
and is not skipped. Testnet may be reset, changed or stopped. A score or package
found to be wrong may be held for a signed correction.

## Conduct

Do not attack the coordinator, the status host, other participants or the
chain, and do not try to obtain hidden evaluation data. Report a security
problem privately, not in a public issue; the private security contact will be
listed on the [landing page](README.md) before the testnet is announced. Report
other problems through the public repository's issue tracker.
