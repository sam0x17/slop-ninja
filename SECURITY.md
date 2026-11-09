# Security policy

Report a security problem privately by email to
[security@slop.ninja](mailto:security@slop.ninja). Do not open a public issue,
pull request or discussion for it.

## What to report

Anything that could let someone:

- obtain hidden evaluation data (texts, labels, per-document predictions),
  exact scores, another miner's artifact before its cohort's scoring starts
  or its opening before the cohort closes, or private model weights;
- change a score, a roster, a signed package, a weight row or a published
  record without the signing keys;
- upload or be admitted outside the admission rules (an unsigned or
  unregistered request, a second artifact for one hotkey in a cohort, an
  object larger than the artifact spec allows);
- stop or stall cohorts, the coordinator or the validators;
- run code on, or read secrets from, a machine running the miner, validator
  or local scorer tools.

This covers the testnet subnet 601 services (the coordinator and its upload
host `testnet-admission.slop.ninja`, the public status host), the miner and
validator clients, the local scorer and artifact checker, the published
trust roots and signed records, and the code in this repository.

## What to include

- what you found and where (host, route, command, file or commit);
- the steps to reproduce it, and what you expected instead;
- the impact as you understand it;
- your platform and the tool versions (`--version`), if a tool is involved.

Never include a mnemonic, keyfile or password, yours or anyone else's.

## Please do not

- test against other participants' hotkeys, artifacts or machines;
- try to read hidden evaluation data beyond what is needed to show the
  problem, or keep any you obtain;
- degrade the service for others (load tests, floods, denial of service);
- publish the problem before it is fixed or we have agreed a date with you.

Testnet runs on test tokens and may be reset; mainnet is not live. The
[terms](docs/testnet/terms.md) apply to every use of the testnet.
