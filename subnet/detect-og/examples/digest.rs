//! Renders one card JSON read from stdin and prints its PNG's SHA-256: the
//! wasm32 check of the golden digests (`wasi-digests.mjs` runs it as
//! `wasm32-wasip1` under Node's WASI, so the bytes come from the same
//! wasm32 code generation as the Worker's).
//!
//! ```sh
//! cargo build --release --target wasm32-wasip1 --example digest
//! node examples/wasi-digests.mjs <target>/wasm32-wasip1/release/examples/digest.wasm
//! ```

use sha2::{Digest, Sha256};
use slop_ninja_detect_page::{Card, GaugeEnds, Origins};
use std::io::Read;

fn main() {
    let ends = match std::env::args().nth(1).as_deref() {
        Some("letters") => GaugeEnds::Letters,
        _ => GaugeEnds::Icons,
    };
    let mut json = String::new();
    std::io::stdin().read_to_string(&mut json).expect("stdin");
    let card: Card = serde_json::from_str(&json).expect("a card");
    let origins = Origins::from_vars("https://slop.ninja", "https://s.slop.ninja").expect("origins");
    let png = slop_ninja_detect_og::card_png(&card, &origins, ends).expect("rendered");
    let hex: String = Sha256::digest(&png).iter().map(|b| format!("{b:02x}")).collect();
    println!("{hex}");
}
