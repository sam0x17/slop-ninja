# Slop Ninja brand assets

These are the approved Slop Ninja logo assets, based on the artwork supplied by
the project owner. Use this design as the project identity.

| File | Use |
| --- | --- |
| [Full logo](slop-ninja-full-logo.svg) | Ninja illustration and wordmark |
| [Wordmark](slop-ninja-wordmark.svg) | Horizontal Slop Ninja lettering for dark backgrounds |
| [Light-background wordmark](slop-ninja-wordmark-on-light.svg) | Text-only mark for light backgrounds |
| [Icon](slop-ninja-icon.svg) | Square ninja illustration |

The illustrated full logo and ninja icon may appear only on dark backgrounds.
The primary wordmark also uses white lettering and belongs on dark backgrounds.
Use the text-only light-background wordmark on light surfaces. Keep the SVGs as
the source assets for future exports.

Generate the Twitter/X avatar and three header options with
`node assets/brand/build-social.mjs`. PNG exports and editable SVG compositions
go to `~/Documents/SlopNinja` by default; an optional argument selects another
output directory. The generator requires Node.js and `rsvg-convert`. All social
assets have dark backgrounds, and the headers leave room for the profile avatar.
