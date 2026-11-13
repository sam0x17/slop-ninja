# SlopNinja brand assets

These are the approved SlopNinja logo assets, based on the artwork supplied by
the project owner. Use this design as the project identity. The ninja's
headband ties and the scanner reticle over its eye are ninja red (`#e8173f`
to `#ff3358`, the site's second brand colour), and the o of "Slop" in every
wordmark is a thin scope reticle in the same red: a ring at about a third of
the letter weight, broken at the four cardinal points, the left and right
ticks crossing the ring, a short top tick, the bottom left open as on the
eye, and the eye's inner arc low on the right (a step deeper red on the
light-background wordmark, so it holds on white). Every other colour, shape
and proportion is the original artwork. The reticle is drawn as stroked
centrelines at 8 units with a 1 px non-scaling layer under them, so the one
master serves every size; keep it as one file rather than a small variant.

| File | Use |
| --- | --- |
| [Full logo](slop-ninja-full-logo.svg) | Ninja illustration and wordmark |
| [Wordmark](slop-ninja-wordmark.svg) | Horizontal SlopNinja lettering for dark backgrounds |
| [Light-background wordmark](slop-ninja-wordmark-on-light.svg) | Text-only mark for light backgrounds |
| [Icon](slop-ninja-icon.svg) | Square ninja illustration |
| [Favicon](slop-ninja-favicon.svg) | Head crop of the icon for favicons from 32 px up: the brow strip is a crimson band and the headband ties are scaled into the crop |
| [16 px favicon](slop-ninja-favicon-16.svg) | Pixel drawing for the 16 px favicon entry, with a 2 px crimson band and a stepped tie |
| [Ninja states](ninja/) | The detection pages' and cards' ninja in its four states: `strike`, `ready`, `stoic` and `bow` |
| [Gauge ends](gauge/) | The slop gauge's two end markers: the human bust (`human.svg`) and the robot bust (`robot.svg`) |

The illustrated full logo and ninja icon may appear only on dark backgrounds.
The primary wordmark also uses white lettering and belongs on dark backgrounds.
Use the text-only light-background wordmark on light surfaces. Keep the SVGs as
the source assets for future exports.

The two favicon files are favicon-only emphasis: the full icon's brow stays
charcoal, with its red on the ties. The website generates the same head crop at
build time and keeps the 16 px drawing; these copies are byte-identical to what it
serves.

The website keeps byte-identical copies of the icon and the dark-background
wordmark and derives its favicons, touch icon and 1200 x 630 Open Graph card
from them in its own repository. It uses neither the full logo nor the
light-background wordmark. When either SVG changes here, copy it to the site
and regenerate those rasters there.

The social media PNGs (the Twitter/X avatar and three header options) are not
stored in the repository: generate them with `node assets/brand/build-social.mjs`,
which composes them from the icon and wordmark here, so they carry the same
ninja-red ties and reticle, and sets the header tagline as the site does
("Don’t just detect slop. Fix it.", with "slop." in the hero's ninja red
and "Fix it." in mint, in Inter, the site's type). PNG exports and editable
SVG compositions go to `~/Documents/SlopNinja` by default; an optional
argument selects another output directory. The generator requires Node.js
and `rsvg-convert` (or a stand-in that renders SVG to PNG at a given size),
with Inter installed on the host or `INTER_FONT_DIR` pointing at a directory
of Inter font files (it then writes a fontconfig for the render that adds
them); without Inter the tagline falls back to Arial. All
social assets have dark backgrounds, and the headers leave room for the profile
avatar.

## Ninja states and gauge ends

`ninja/` holds the four states of the ninja that a detection page and its
share card draw beside the result: sword drawn (`strike.svg`), sheathed and
ready (`ready.svg`), standing stoic (`stoic.svg`) and bowing (`bow.svg`), each
a 913 x 913 drawing for dark backgrounds. The state follows the estimated AI
share, clamped to what the evidence band allows
(`subnet/crates/detect-page/src/ninja.rs`); the art keeps the blade in a high
guard, away from the text. `gauge/` holds the slop gauge's end markers, a
human bust at the human end and a robot bust at the slop end, each drawn to be
clipped to a light disc. The card renderer (`subnet/detect-og`) embeds these
files at build time, and the website serves byte-identical copies, so a
change here is copied to the site.

Like the logo, these files are not covered by the code and documentation
licences.
