# Testnet terms

Version 4, November 11, 2026. It replaces version 3 of the same day (which
replaced version 2 of November 9, 2026, which replaced version 1, the
unnumbered terms published before it) and adds what SlopNinja's planned
distribution from its revenue pot to testnet miners is and is not. A
commitment made after version 4 is published accepts it; an earlier commitment
stays under the version in force when it was made, except that a legal removal
([Legal notices and removals](#legal-notices-and-removals)) can apply to
material published under any version.

These terms apply to taking part in SlopNinja on Bittensor testnet subnet
601: registering, committing, uploading an artifact, running a validator, and
using the testnet's published data. By committing or uploading an artifact you
accept them. They cover testnet only; mainnet terms will be published
separately.

## No monetary value

Testnet is a rehearsal. Test TAO, testnet alpha, scores, credits and weight
rows on subnet 601 have no monetary value. SlopNinja does not sell, buy,
exchange or redeem them, and taking part creates no claim to any payment,
token or mainnet allocation; the planned distribution below does not change
this. SlopNinja does not provide test TAO.

## The pot's planned distribution

SlopNinja plans to put the revenue of its detection products (a free detection
page with advertising on the site's content pages, a paid detection API and
other products) into a public pot, and to use it as section 8 of the
[whitepaper](../../whitepaper/slop_ninja.pdf) describes. Once the pot holds
about 1.5 times the cost of a mainnet subnet registration, SlopNinja registers
the mainnet subnet with that money. Once that subnet launches, the money left
from revenue earned before the launch buys the mainnet subnet's alpha: half of
it is burned, and half is distributed to testnet miners on mainnet through a
claim contract, in proportion to the testnet-era revenue earned by their
winning models. After the launch, all revenue buys and burns alpha.

What the distribution is:

- **A plan of the operator.** It is SlopNinja's planned policy for the pot's
  money, set out in the whitepaper and to be set out in the pot's charter. It
  is not a term of participation and gives no one an entitlement to any
  amount. Under the charter, a change to what the pot's money may be used for applies only to
  money received after the change is published. As of November 11, 2026 the
  pot has received no money.
- **For winning models only.** Revenue is credited only to a model that has
  beaten the reference and taken its place, under an attribution rule,
  published before it is used, that credits revenue the detection products
  earn while that model is the reference. Revenue earned while one of
  SlopNinja's own models is the reference is credited to no miner, and a model
  that earns emissions without becoming the reference is credited nothing.
- **Paid in mainnet alpha.** The alpha is bought on the mainnet subnet with
  the pot's TAO and claimed on mainnet through a claim contract whose rules
  are published before it is used.

What it is not:

- **Not a value for testnet tokens or scores.** Test TAO, testnet alpha,
  scores, credits and weight rows keep no monetary value. Nothing is exchanged
  or redeemed for them, and the distribution is not measured by them.
- **Not part of the reward mechanism.** It changes no score, gate, promotion,
  weight row or emission, and validators do not check it.
- **Not a promise of any amount or of mainnet.** What a miner receives
  depends on the revenue credited to its models and on what the pot's
  remaining money buys. If the pot never reaches the registration point, or no
  revenue is credited to your model, you receive nothing. If SlopNinja
  publicly abandons mainnet, the pot's TAO is burned and nothing is
  distributed.
- **Not a reward for traffic.** Do not visit, use, share or click anything,
  advertising included, to raise what anyone receives: SlopNinja watches
  traffic to pages with ads for invalid activity and switches the ads off
  when it appears.

The plan and its wording are reviewed by legal and tax advisers before the pot
takes its first deposit and may change before then.

## Your submission

- **Licence.** You license every artifact you commit and upload under the
  [Creative Commons Attribution-ShareAlike 4.0 International licence (CC BY-SA
  4.0)](https://creativecommons.org/licenses/by-sa/4.0/).
- **Your rights.** Submit only an artifact you have the right to license that
  way, including what it derives from: base models, training data and code.
- **Publication.** If your artifact is admitted to a cohort, the coordinator
  publishes it (its header and files) when that cohort's scoring starts, and
  publishes your opening when the cohort closes. A request alone, yours
  included, does not make SlopNinja withdraw a published artifact; it removes
  one only by a recorded legal removal ([below](#legal-notices-and-removals)). Anyone
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
(below). Two such decisions are planned. Before the first cohort that enforces
the span gate (a check of each entrant's AI spans against d24's on the same
batch; no cohort enforces it yet), one signed span-equivalent reference
decision passes `R_0`'s position to span package d24, SlopNinja's
span-serving package, after d24 runs once as an ordinary entry, so that it is
published and fingerprinted; this changes no reward input and is not the
planned replacement. That replacement is the other: replacing `R_0` by the
next reference SlopNinja trains, only if it passes every required gate,
announced in advance on the status page. Replacing the reference moves no
reward already earned.
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

## Legal notices and removals

Send legal notices about material SlopNinja publishes to
[legal@slop.ninja](mailto:legal@slop.ninja), in English: for example a
copyright notice about a published artifact, including a notice under the US
Digital Millennium Copyright Act (17 U.S.C. § 512(c)(3)), a counter-notice, a
court or authority order, or a request about personal data. For the EU Digital
Services Act, the same address is SlopNinja's point of contact for Member State
authorities, the Commission and the Board (Article 11) and for recipients of
the service (Article 12). Security reports go to the security contact
([Conduct](#conduct)), not to this address.

SlopNinja removes published material (an artifact's header or files, or a
reference package; never a result, opening or record) only on legal grounds: a
copyright notice it finds valid, a court or authority order, or a personal-data
request that applies to the material. A removal is a recorded operator action
and is never silent. When SlopNinja takes material down, it adds an entry to
the public [removal record](removals.md) giving the date, the material (by its
digests, and its cohort for an entrant artifact), what was taken down, and the
legal basis by type and jurisdiction. An entry holds no content of the material
or of the notice, and no personal data.

A removal changes nothing already decided. Commitments, openings, scores,
quantized results, bands, gate bits, clusters and signed rows stay as they are,
so the target in force and every reward paid or still due under it are
unchanged, and the digests stay in the public record. A removed artifact still
counts as published under [Copies](#your-submission): an entry with the same
inference content is still a copy. If the same material is published again, it
is removed the same way, with its own entry. A removal deletes only the copies
SlopNinja hosts and cannot reach copies already downloaded. These rules concern
particular material. SlopNinja still does not promise to host any artifact
indefinitely ([Publication](#your-submission)), and stopping the testnet or its
public host altogether is not a removal.

Removing a reference's public copies does not by itself change the reference,
and SlopNinja keeps its private evaluation copy. If the legal basis also bars
SlopNinja from running a removed reference, or the package its detection
service serves, SlopNinja stops using it through the [operator
decisions](#operator-decisions): a reference decision for later cohorts and, if
a running cohort cannot finish without it, abandoning that cohort; the
detection service stops serving that package.

If your artifact was removed and you think that was a mistake, write to the
same address.

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
([security policy](../../SECURITY.md)). Send legal notices to the address
[above](#legal-notices-and-removals). Report other problems through the
public repository's issue tracker.
