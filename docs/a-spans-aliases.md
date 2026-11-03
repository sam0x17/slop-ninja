# A span commit aliases

Recorded on October 31, 2026. The span localization work reached main as the
28 own commits of the `a-spans-v1` branch, cherry-picked in order onto
`bcfadf55`. The Container commit (`848e7e8c`) and this map follow them. The
branch also carries history that must not reach main, so its own commits were
replayed rather than merged.

Each commit on main has the same span changes as its branch commit, on a newer
main. Its message is unchanged, except that it ends with a `[skip ci]` line.
The span docs cite the landed IDs. Records in ignored storage that name a
branch commit, such as the retained binary `full_a-df6c38e5`, keep the branch
ID; the table gives the landed ID.

## Alias map

| Commit on main | Branch commit | Title |
|---|---|---|
| `983e4dfc3e6d7bc3511fc1a8666ab176334600b0` | `8e4a378c340d41511691e3ae8683e6d1a432147f` | Pre-register the A span localization protocol spans-v1 |
| `cb745eace74d6e827be46da7d414f4e2e67c6174` | `e3980e8fbc729530a895e51e456c934c673d9ea3` | Add the A span localization token head, emission rule and v3 runtime |
| `3235e1cd49f6939f4f2713c5e6918e965d0d348e` | `663837eec6cf6cd806d818c88c6e082edb216f2d` | Test the spans-v1 export case builders |
| `7aba3a467b3ef126affe09d299ec68d5c3a679e6` | `92d69e0bb24ae5c9776be93ec8b9f17b5ae4d7af` | Gate the A replay on an FP64 Python reference and keep FP32 as drift |
| `b3e7aafe10456d498711135fb22d398099ce7042` | `82c47dddf31f12e08af54bff4ef0fb5d1ddc246a` | Add the spliced-rewrite span data generator for spans-v1 |
| `ffd73dadc5c87663d42e9e45931794aa915ddb2b` | `c8fa40819a399dc6848835fc68c59ee8ad526657` | Record the A span localization implementation and its smoke run |
| `0e94784ef63d65374dd7b70db7d2f51d948ba4ac` | `1a9afb06796fb33f168c13739773a2dee70d5d30` | Tighten spliced prompts and screens after the 500-document pilot |
| `66a0c6c8552fcd60a131c459271357eaeffdae14` | `142f256e1b2edb682e737b8d387a2238065db49b` | Keep finalizing the Dev-iter span set after the Train prefix cut |
| `6176d5c90289dff4258d0b2a9115e9b1c80aa0e8` | `b344dbff14e6f48ae58a764a8e433c1d7f6df021` | Document the spliced-rewrite span data and its Phase A results |
| `e810022c1f407ac092311b6117ca11578a3cbcb3` | `7ca88482b3b9ccaa2db770a14456d2735cd588d8` | Apply the per-domain cap to accepted spliced Train documents |
| `78964d4d5debef2675a90cb6d8c8a65a2fa52427` | `42c9b3526c8542223184aaa3831cf61ed1f25db7` | Report the final 40,000-document spliced Train set and the Dev-iter span set |
| `958fdad4e6e26d3cdda98ae9f1c4068380c11a97` | `df6c38e5b2613cf0955cd531214588dd1a0d91eb` | Build the FP64 rotary tables as HF's float32 tables and gate R1 on the trained function |
| `1ebd39973f1e2e833bae4ce4deab24ad64130b30` | `5f87bff170c49056c9119e0d126971a35bf2984f` | Amend the spliced span data: meta screen v2, keyed typography normalization and copied-word masks |
| `4a18f06f060bb80cc319417f99552cd1a8d19b50` | `6e4b6e6360f44178697b27b24652785f2523b42e` | Record protocol spans-v1.1 and the amended spliced training snapshot |
| `09c9d523f00e2fd6cfb4135339046f6f250e095e` | `5f2a1d30633ace3a42fc7ccbd843b72a8d076aa5` | Exclude the meta-flagged Dev-iter rows from the trainer's span metrics |
| `ff7c7104d31573484ca399860563dc7c6cc64f8b` | `a3b58d256091e640436784b6898e9bbaa73e2660` | Assemble the spans-v1.1 plus bundle and project the Dev-iter span halves |
| `eaf66e5579de6842e4a3958efba222cfe3b32a76` | `96865a1dfa55aa20178c6a6f440a3589fe8f34da` | Add span offsets, row scoring and the spans-v1.1 span evaluator |
| `cc356b9ba0428253034fdbf58c7d3f75c120c9b8` | `5879bdb8dfd998dee548989096807cb33e18aba6` | Compare padded token-head outputs on the probability scale in the trainer's start gate |
| `d6dd8b4587ed2e97c00a955b698511c580485bd3` | `5dfe941516576dfe47633b9bc55dd7a57ece47b0` | Bound padded token-head probabilities at 1e-3 in the trainer's start gate |
| `d0a2441f41d822f2fd1dbd27baf7ccdca033a1bf` | `070b7dca44d0995a71ce859a493e2452daa3c76a` | Report document outputs in the span evaluator and apply frozen CAL maps to raw heads |
| `c0261f7b0d04c78b46a509d3cef3c4b31dcd3334` | `6a1f408b3a16485ce4956d1ecc8f996ddff8999c` | Slice span metrics by cohort id in the span evaluator |
| `7adbb9aaf95b9414e73a221af02a5ef5553257a4` | `bd3d411ccf2bdfd717d312ab0ac433e0e95474b6` | Report the spans-v1.1 fits, gates, selection and span metrics |
| `259e99cf0755151fb882728180b44033c50cfedc` | `418af8bfff74a3d41dcfd08460aadff9592f1fdd` | State the spans-v1.1 review qualifiers in the span results |
| `537d9deda630fb665e732d429e75cd514113c40b` | `4891bf9e273050d28a251078e93324379c2a81ca` | Report the shipped span configuration: the release plus the S0+ seed-17 token head |
| `e63fe55a51b0fc1bfccc825184553669ce28ef72` | `d4f27fb2c937c28e792680ef9c7c3d9c18c4e123` | Make the frozen S0 fit test start from a trained E1 checkpoint on every torch build |
| `08b4508e76a2d2806e5d27ec1086331cf782c7c1` | `8a89b2d62e674a79eeb7f1d9a9b13b9427ea711d` | Replay the R0 release package in the spans runtime and renew the a-v6-cpu source pins |
| `4cb26e3f5e73181538bc540ad5fb9ac5350ae800` | `168520e857f4b8d9de1deed2af47b230e109bee2` | Describe the release as R0's document outputs plus the S0+ span head (export v3) |
| `ffe28c2ab3e6d9d281e7cb599e29b96c427c38af` | `4ffcf293f19ae8b499568e4a27305f684b753499` | Describe the shipped S0+ head in the span results limits |

## Changes relative to the branch commits

- `983e4dfc` and `6176d5c9` add the span lines to `docs/README.md` after
  main's Container and testnet manifest lines.
- `cb745eac` keeps the branch's crate doc in `grammar-a-baseline/src/lib.rs`,
  followed by main's `comp_diagnostics` module.
- `b3e7aafe` declares both `span_ops` (from main) and `splice` in
  `grammar-a-positives/src/lib.rs`. Main added a `post` field to the
  generation context, so `src/bin/spliced.rs` sets it to `None`: the spliced
  generator runs without post-processing, as on the branch.
- `08b4508e` takes the branch commit's `inference/a-v6-cpu` `QUALIFICATION.md`,
  `UPSTREAM.json` and `src/main.rs`, which renew the source pins for the
  current `modernbert.rs`; `qualified_sources_are_unchanged` passes.
- `4cb26e3f` keeps both paragraphs in `docs/a-baseline-release.md`. The
  served-graph paragraph says the Container serves R0's document outputs from
  the R0 package (export v2), without the token head.

## Follow-up spans-S2

Recorded on November 1, 2026. The spans-S2 follow-up reaches main as the 12
own commits of the `a-spans-s2` branch, cherry-picked in order onto
`72911dad`, then two commits of its own: review fixes (`658889e1`) and the
commit that adds this map. The branch started from `bcfadf55` plus its own
replay of the 28 span commits above; those replays were dropped, because main
already holds the same changes as `983e4dfc`..`ffe28c2a`.

Eleven of the commits have the same changes as their branch commits, on a
newer main. Their messages are unchanged, except that four (`2b1da562`,
`3544d8fc`, `c9c6cc22` and `3d490604`) end with a separate `[skip ci]` line
instead of a `[skip ci]` at the end of their last sentence. The runtime commit
`95e696b3` differs from its branch commit, as described below; its message
describes the separate runtime file. Records in ignored storage that name a
branch commit, such as the protocol amendment, the freeze log, the selection
and build records and the retained binaries' `HEAD` files, keep the branch ID;
the table gives the landed ID. S2c remains a candidate only, and the shipped
configuration is unchanged.

| Commit on main | Branch commit | Title |
|---|---|---|
| `2ac1e8eab6e67b3360be001695c8dd7f395632a9` | `03ac409ec9fbe98d151f8ba298694eff8ac8c87d` | Add the spans-S2 trainer options: anchored document heads, paired human contrast and a span tower |
| `08b45880b4d41ac399f79aeeeec24e1de299de4a` | `c30372e72abc3dd6500d4b08ab0f0b94581bcd20` | Check the spans-S2 teacher against the init fit and stratify the smoke subset by generator label |
| `e154a80165a537a22df4fb200e53f5ec65fb224a` | `f26fe353632e3ce088b52709b902803e205b2a3a` | Add the spans-S2 human-document gate tool (H1, H2) |
| `c4bca633956021229de485e0a8976e760ab63ef9` | `a2c7e7684aaae4932e278c71df6ff6bfa8dda0c3` | Pre-register the span follow-up protocol spans-S2 |
| `fb82fffdb04fd65a4dfe08382ada01fae6109bad` | `7d1610196863d8910d08efa810779bfaedb55288` | Report the spans-S2 follow-up: S2c selected as a candidate, outcome partial |
| `2b1da562f9191765423535458ec2981abf060a84` | `ab4f5a9413d10a6e07c7c6dbedd72bf2ab57b60c` | Export the spans-S2 span tower as export v4 with golden vectors for its runtime branch |
| `95e696b32820a5c1cd3da5a049932a25913b801e` | `758b35fba2226728f8ad213d3377ec203c18ba0e` | Run the spans-S2 span tower from its own runtime file when the export declares it, and requalify the a-v6-cpu source pins |
| `c9e247d1a964fdccf9d3e22e35900a96fce3bb1a` | `49901a09eaa4d6b86fa2bc186c31e4093d84b946` | Report the S2c tower export, its runtime branch and the tower's CPU latency |
| `3544d8fc6bdeb4713e7cc966048fdd3b1e7478b2` | `08a0c84f2527219d92550ddaada3ba9a63cec71a` | Check the span tower's file binding and rotary bases, and note the additive comparison keys |
| `c9c6cc22aa2005f43196edf392d378ca576dbf33` | `fc3eed13f381aef34d91b9600fdd6a39f8af9a95` | Let the span evaluator's grid mode take the declared G576 decoder grid |
| `3d490604fef906aaee16db34675a6e48e09d5580` | `603f9a5466d20a96e9e02a5fa1de5ef2c43daab5` | Report the spans-S2 post-selection steps for S2c: R1, decoder, CAL maps, span_report and the retrospective Test |
| `b4ce76f6943f12298ccdbb1068c861694882d55c` | `5319ad525f53d13e7c08a6505cf2b7a53f3ffba9` | State when the S2c span_report outputs were produced and evaluated, and give peak memory in GiB |

### Changes relative to the S2 branch commits

- `2ac1e8ea` applies the S2 trainer options to main's `train_full_a.py`,
  which also has the optional fixed AI roster (`ai_roster`, rule
  `fixed_v1`). Both options are kept; neither changes the other's default.
- `95e696b3` (branch commit `758b35fb`, titled "Run the spans-S2 span tower
  in the spans runtime when the export declares it, and requalify the
  a-v6-cpu source pins") leaves the heads runtime byte-identical to main.
  The Container, `a_scorer` and the artifact-check spec pin that runtime by
  digest: `full_a.rs`, `full_a_spans.rs`, `involvement_calibration.rs` and
  `shared.rs`. The spec digests, which the testnet artifact spec relies on,
  therefore stay valid.
  The branch commit changed `full_a.rs` and `full_a_spans.rs`. Here the heads
  v3 reader, the tower declaration and the token-state dispatch are in a new
  file, `grammar-a-baseline/src/full_a_tower.rs`. The Container, `a_scorer`
  and artifact-check compile only the four pinned files, through `#[path]`,
  and never compile the new file. Every crate that links
  `grammar-a-baseline` (`a-v6-cpu`, `grammar-a-positives`) compiles it.
  Dispatch still follows the declared heads schema, with no runtime-version
  check. The reader passes heads v1 and v2 bytes to the pinned parser
  unchanged. It gives the pinned parser heads v3 read as heads v2, after
  checking the schema, the token head's input and the tower declaration;
  every other member keeps its exact bytes. For this, `grammar-a-baseline`
  enables `serde_json`'s `raw_value` feature. The token head is the pinned
  function, run on the tower's states. The branch commit's tests of these
  parts moved to the new file. New tests cover the unchanged pass-through,
  the exact member bytes and refused repeated keys. Every `full_a` run also
  binds `tower_runtime_sha256`.
- `3544d8fc` and the later branch commits apply unchanged on top of
  `95e696b3`.
- `658889e1` has no branch commit. It adds tests that every export member,
  the span tower file included, must match its manifest digest and stay
  inside the export, and that every run binds each runtime source digest,
  `full_a_tower.rs` included. `compare_full_a` now also requires a v4 run's
  tower file, read from the export, to have the manifest's digest, and the
  run to bind `tower_runtime_sha256`. The commit also corrects the new
  file's header on which crates compile it, and a golden-vector test that
  folded the one-token case into its longer-case maxima.
- R1 was run again on the landed sources (`658889e1`) and passed. Every
  prediction equals that of the build that ran R1 before (branch commit
  `08a0c84f`), apart from timing, and the comparison maxima are unchanged.
  The freeze log records the rerun. It also records an earlier rerun with
  the same result, from sources before the review fixes (`0ff94d4a`, whose
  tree equals `b4ce76f6`).
- `inference/a-v6-cpu/QUALIFICATION.md` and `UPSTREAM.json` record the
  source-pin requalification against a build of main (`72911dad`), not the
  branch build. The warm-A roster, the R0 release package and the shipped
  spans package are bit-identical to main's build.
