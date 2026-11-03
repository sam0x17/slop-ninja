//! Pure form rules for Slop Ninja A positives (`grammar-a-form`).
//!
//! One source for the form path shared by the CC cohort tools
//! (`grammar-cc-human`), the local and spliced positive generators and the
//! frontier collector, and the coordinator's Wasm port:
//! * `conventions`: quote, dash, range and ellipsis sites, a parent's own
//!   conventions, the batch prior and its keyed draws, rendering;
//! * `layout`: the excerpt layout, paragraph structure, break placement and
//!   the child normaliser;
//! * `screen`: the FineWeb line classifier, the paragraph form screen and
//!   `classify_all` for profile v2's licence screen;
//! * `echo`: the echo screen `cc_echo_screen_v1`;
//! * `window`: the final-document length window `cc_final_length_window_v1`;
//! * `measure`: layout measurement `layout_measure_v2` (lines, gaps,
//!   paragraphs, hard wrap, frames, counts) for profile v2 and the layout
//!   audit;
//! * `transfer`: the layout transfer `layout_transfer_v2` of profile v2;
//! * `profile`: the profile ids, `Profile`, the every-class steps, profile
//!   v2's population x domain prior and its per-child step;
//! * `text` and `rank`: the text rules and seeded rank they rest on.
//!
//! Every function is pure: no I/O, no global or thread-local state, no
//! clock, no randomness beyond seeded hashes, and output size linear in the
//! input. The crate depends only on serde, serde_json and sha2, so it builds
//! for `wasm32-unknown-unknown`.
#![forbid(unsafe_code)]

pub mod conventions;
pub mod echo;
pub mod layout;
pub mod measure;
pub mod profile;
pub mod rank;
pub mod screen;
pub mod text;
pub mod transfer;
pub mod window;
