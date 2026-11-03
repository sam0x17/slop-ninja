# CFG-versus-encoder commit aliases

Recorded on October 31, 2026. The CFG-versus-encoder writer-profile work reached
main as the eight own commits of the `cfg-vs-encoder` branch, replayed in order
onto `ec397c2d`. This map follows them. The branch was based on `3aa51700`. It
stays on the remote with its original commits, so every branch ID below still
resolves.

Each commit on main has the same changes as its branch commit, on a newer main.
Every message is unchanged, except that the arm F commit's message now ends with
a `[skip ci]` line.

The [protocol](cfg-vs-encoder-protocol.md) and [results](cfg-vs-encoder-results.md)
notes cite branch commits where a record binds them. These are the protocol
freeze commit and the commits that built the recorded evaluator binaries. The
records in ignored storage under `data/grammar-private/cfg-vs-encoder-20261031/`
also name branch IDs. The table gives the landed ID for each.

## Alias map

| Commit on main | Branch commit | Title |
|---|---|---|
| `ef1a913fb9e3233e9482b44841233509b13e71ff` | `34d2fe3ae28d0f285305f22d754a1ce49bbf1cfb` | Freeze the CFG-versus-encoder writer-profile protocol |
| `36c716c5229077e57b1520582de15fdeef631014` | `edc5085935337f311a7f7d65c3d9e6eec1261742` | Add feature extraction for the CFG-versus-encoder protocol |
| `155149365b3dde56860ed7bab53cb2cf7156362b` | `1b87f640f17d629aff8a32ec1b810800b69b57b4` | Record CFG-versus-encoder feature extraction coverage |
| `3a5b3384788b0da4646c663c333438b7f80be52e` | `25f8cd9c119b78c6d03e52921da2cca7767ce939` | Add the evaluator for the CFG-versus-encoder protocol |
| `67708b2f1133bbac17f867a5b55ce6b29233212c` | `5f74ff3d0ef712a66d8d8a50501d1fcbed336758` | Record the frozen CFG-versus-encoder evaluation outcome |
| `3f3672c14e5b10fe1e0ba5d16df3dbf0fe87083e` | `2841e0af3a461ef34a028665770baa002980b4fa` | Report verified CFG-versus-encoder writer-profile results |
| `1d72888b7091846d17c173c6894018d7ac6b4699` | `f3ed1561a4977a1333bf842cb99cff8f062a265d` | Add arm F and the v2 reproduction gate to the CFG-versus-encoder evaluator |
| `3e9b20dc384f476853d0b4615f134ab56e2de544` | `0a4201e6bd3053eade1de97bd306703a9fe1a128` | Report the CFG-on-encoder-plus-stylometry follow-up |

## Bound branch IDs

| Branch commit | What binds it |
|---|---|
| `34d2fe3a` | The v1 freeze commit, made before any governed metric was computed |
| `25f8cd9c` | Source of the v1 evaluator binary (`4e4fe0cd…`) |
| `f3ed1561` | Source of the version 2 evaluator binary (`8bfaf740…`), with parent `2841e0af` |

The replay had no conflicts. Each landed tree equals the branch commit's changes
applied to `ec397c2d`.
