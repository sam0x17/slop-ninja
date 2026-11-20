# detect-og

The stateless Worker that renders a SlopNinja detection page's share card: a
2400 x 1260 PNG drawn from the page's card JSON (`slop_ninja_detect_page::Card`,
`subnet/crates/detect-page`). The website and the secret-host Worker reach it
only through their service bindings; it has no route, no binding, no secret
and makes no outbound request. The detect Worker builds the card from the page
(`POST /internal/v1/card`), so the page and its card agree, and a secret
page's card carries no slug: its link prints the secret host and nothing after
it.

| Route | Does |
|---|---|
| `POST /render {card}` | the card's PNG (`200 image/png`, `Cache-Control: no-store`); 400 for a body that is not one card or a card the template refuses, 413 over 64 KiB, 503 when the configuration is unusable or the render does not fit its memory (the caller then serves the static site card) |

The dial on a scored detection shows the displayed confidence in AI involvement.
A qualified reading such as "above" or "below" keeps its qualifier and places
the needle at the displayed endpoint. A second, labeled bar shows estimated
AI share when available. Short texts (24 to 149 words) show the involvement
reading alone. A paid whole-document card shows estimated AI share and a
section ladder. The card also shows the source, scan date and model. A pasted
text's card identifies the source as private and prints only the secret host.
Generic and paid whole-document cards omit the ninja.

Modules: `card.rs` (the template: one SVG document per card), `layout.rs`
(line breaking and fitting with the fonts' own metrics), `fonts.rs` (the
embedded Inter subset and its `cmap`, `hmtx` and `kern` tables), `art.rs` (the
ninja states, gauge busts and wordmark from `assets/brand/`, embedded at build
time and inlined as nested SVG), `svg.rs` (escaping and number form),
`render.rs` (resvg 0.50 and PNG encoding), `routes.rs` (arena routes and
footprints), `wire.rs` (the request) and `cf.rs` (the wasm32 export).

Rendering safety: card strings enter the SVG only as escaped text nodes; the
template refuses a card that fails `Card::check` (bounds, forms, control and
bidi characters) or holds a character outside the fonts; the parser loads the
two embedded faces only, sets no resources directory and resolves no
`<image>` reference, data URLs included. The PNG carries no metadata chunk,
so equal cards give equal bytes.

Each request has its own arena scope. The renderer reserves 36 MiB in a
child scope before drawing. The native arena test exercises both gauge styles
and a worst-case section ladder that fits the 64 KiB request limit. The PNG
leaves the child scope as a JavaScript copy.

Configuration: vars `SITE_ORIGIN` and `SECRET_ORIGIN` (the hosts a card
prints) and `GAUGE_ENDS` (`icons`, the default, or `letters`; no emoji art
ships with this Worker, so `emoji` is a configuration error). `wrangler.toml`
has the production (`testnet`) and staging environments, the staging origins
as placeholders the deploy wrappers fill, and `[env.dev]`, with which the
Worker joins the local dev session (`subnet/devtools`).

Fonts: [`src/fonts/`](src/fonts/README.md) holds an Inter subset under the SIL
Open Font License 1.1, rebuilt byte for byte by [`font-subset`](font-subset/src/main.rs).
The art is SlopNinja's brand art in `assets/brand/`, outside the code licence.

Checks:

```sh
cargo test --workspace                     # unit, golden, hostile-string and source tests
UPDATE_GOLDEN=1 cargo test --test golden   # rewrite tests/golden/digests.txt after a deliberate change
RENDER_DIR=<dir> cargo test --test golden  # also write every golden card as PNG for a look
cargo test --features arena-witness arena_witness -- --test-threads=1 --nocapture
cargo build --release --target wasm32-wasip1 --example digest
node examples/wasi-digests.mjs <target>/wasm32-wasip1/release/examples/digest.wasm   # the same digests from wasm32
worker-build --release                     # the Worker build
```

The golden digests are the same natively (x86_64) and in wasm32; they hold for
resvg 0.50.0 (pinned exactly) and change with the art, the fonts or the
template.
