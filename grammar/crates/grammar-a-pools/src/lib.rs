//! Rights-gated human prose pools for Slop Ninja A.
//!
//! Source adapters turn downloaded raw files into normalised human
//! documents (`slop_ninja_a_pool_document_v1`) with exact text, provenance,
//! dates, licences, family keys and whitespace word counts in the admitted
//! `150..=450` bound. `protected` indexes every registered prior snapshot;
//! `dedup` removes prior overlap and within-pool exact and near duplicates
//! and writes per-source admission records. Nothing here calls a detector,
//! a model or the network.
pub mod adapter;
pub mod american_stories;
pub mod ami;
pub mod blog_rekey;
pub mod blogs;
pub mod blog_splits;
pub mod btp;
pub mod cap;
pub mod courtlistener;
pub mod cpd;
pub mod csvread;
pub mod dedup;
pub mod essays;
pub mod eval_select;
pub mod fedreg;
pub mod fineweb;
pub mod fineweb_blogs;
pub mod fomc;
pub mod forum;
pub mod global_voices;
pub mod gutenberg;
pub mod gutenberg_plan;
pub mod hearings;
pub mod hmd;
pub mod html;
pub mod icsi;
pub mod io;
pub mod legal;
pub mod mediawiki;
pub mod ocr;
pub mod overlap_scan;
pub mod pile;
pub mod plos;
pub mod plos_blogs;
pub mod protected;
pub mod psl;
pub mod raid;
pub mod raid_screen;
pub mod record;
pub mod review;
pub mod reviews;
pub mod sbcsae;
pub mod scan;
pub mod select;
pub mod sister;
pub mod splits;
pub mod splits_v3;
pub mod splits_v4;
pub mod spoken;
pub mod stackexchange;
pub mod text;
pub mod uspto;
pub mod wikitext;
pub mod xmldoc;
