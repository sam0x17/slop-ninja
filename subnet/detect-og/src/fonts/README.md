# Card fonts (Inter subset, SIL Open Font License 1.1)

`inter-400.ttf` and `inter-700.ttf` are a subset of Inter 4.001 by The Inter
Project Authors (<https://github.com/rsms/inter>), licensed under the SIL
Open Font License, Version 1.1 ([`OFL.txt`](OFL.txt), the Inter project's
`LICENSE.txt` unchanged). They are a Modified
Version under that licence: these two files, modified or not, are distributed
only under it, never under the code licence of the rest of this repository,
and they are not sold by themselves. Inter declares no Reserved Font Name.

They are the only fonts the card renderer loads. Everything a card prints is
in the subset: printable ASCII, the middle dot (U+00B7) and the ellipsis
(U+2026); the renderer refuses a card with any other character.

| File | Source (static TrueType instance) | Source SHA-256 | Subset SHA-256 |
|---|---|---|---|
| `inter-400.ttf` | Inter 4.001, weight 400, optical size 14 | `1b08e7fc267a5c7e1d614100f604b83e7e8a0be241f0f288faa2b3ac93a683ba` | `0936e33caf8401e573fd0d1bfdd147c825f221df56f4f913046d3ce3b5bdeb60` |
| `inter-700.ttf` | Inter 4.001, weight 700, optical size 14 | `b37284b5701b6b168dfc770aa1a4ac492106422fd3ba76bc7641e37434e8019c` | `7d21e8f40660bba92716c176be31674bf592951fcb456dca9e15833c131a6b57` |

The sources are the static instances Google Fonts serves for Inter (its
`v20` files). The subset is rebuilt byte for byte from them with the tool in
[`../../font-subset`](../../font-subset/src/main.rs), which refuses any other
source file:

```sh
cargo run --release -p slop-ninja-detect-og-font-subset -- <Inter 400 .ttf> <Inter 700 .ttf> src/fonts
```

What the subset keeps: the 97 characters' glyphs, the components of any
composite among them and `.notdef` (103 glyphs, renumbered in their original
order); the `name`, `OS/2`, `head`, `hhea` and `maxp` tables (counts updated);
one format 4 `cmap`; `glyf` and `loca`; `hmtx`; `post` version 3; and a
format 0 `kern` table with the advance adjustments the source's GPOS `kern`
feature makes for the Latin script between the kept glyphs (1,127 pairs at
400, 1,321 at 700). It drops GSUB, GPOS, GDEF, STAT, `gasp` and `prep`: the
renderer applies no substitutions and no hinting.
