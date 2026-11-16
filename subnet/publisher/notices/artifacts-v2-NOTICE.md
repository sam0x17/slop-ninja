# SlopNinja testnet entrant artifacts

This notice covers the artifacts published on this host: each admitted
entrant's artifact header at `artifacts/v2/<header sha256>/header` and the
files it lists at `blobs/sha256/<file sha256>`. An artifact is published when
its cohort's scoring starts, and `v1/cohorts/<label>/artifacts.json` lists,
for each one, its header SHA-256 and the hotkey that entered it.

## Licence

Each artifact is licensed under the Creative Commons Attribution-ShareAlike
4.0 International licence (CC BY-SA 4.0),
https://creativecommons.org/licenses/by-sa/4.0/ (legal code:
https://creativecommons.org/licenses/by-sa/4.0/legalcode), by the miner that
entered it, as the SlopNinja testnet terms require
(https://github.com/sam0x17/slop-ninja/blob/main/docs/testnet/terms.md).

To attribute an artifact, name its header SHA-256, the hotkey that entered
it and the licence.

The SlopNinja name and logo are not licensed by this notice.

## Base model

Each artifact also contains work that others license:

- its `tokenizer.json` is the pinned tokenizer, ModernBERT-base's tokenizer
  re-serialized;
- every allow-listed graph class is built on ModernBERT-base's architecture,
  and an artifact trained from ModernBERT-base's weights, or from a SlopNinja
  reference package (which derives from them), contains a modified version of
  those weights.

ModernBERT-base is by Answer.AI and LightOn
(https://huggingface.co/answerdotai/ModernBERT-base, revision
`8949b909ec900327062f0ebf497f51aef5e6f0c8`) and is licensed under the Apache
License 2.0. A copy of that licence is at `artifacts/v2/ModernBERT-base-LICENSE`
on this host.

## Legal notices

A published artifact is taken down only by a recorded legal removal. Send a
copyright notice or other legal notice to the address in the testnet terms.
