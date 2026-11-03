use anyhow::{Result, ensure};
use clap::{Parser, Subcommand};
use grammar_a_pools::{adapter::Run, american_stories, btp, cap, courtlistener, cpd, dedup, essays, fedreg, fineweb, fomc, forum, global_voices, gutenberg, hearings, hmd, io, mediawiki, ocr, plos, protected, psl, raid_screen, review, reviews, scan, select as cand_select, sister, stackexchange, xmldoc};
use serde_json::json;
use std::{
    io::BufReader,
    path::{Path, PathBuf},
};

#[derive(Parser)]
#[command(
    name = "slop_ninja_a_pools",
    about = "Human prose pools for A: source adapters, prior-snapshot overlap index and deduplicating admission. No network, detector or model calls."
)]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(clap::Args)]
struct Dump {
    /// Decompressed dump stream; `-` reads stdin (for example `bzcat x.bz2 |`). `.zst` is decoded.
    #[arg(long)]
    dump: PathBuf,
    /// Dump identity, for example `enwiki-20221101-pages-articles-multistream`.
    #[arg(long)]
    dump_name: String,
    #[arg(long)]
    dump_url: Option<String>,
    /// SHA-256 of the downloaded (compressed) file, computed by the runner.
    #[arg(long)]
    dump_sha256: Option<String>,
}

/// Reservation and split-acquisition controls shared by the dump adapters.
#[derive(clap::Args, Clone, Default)]
struct Select {
    /// List every family inside `--buckets` by identifier and metadata only
    /// (`reserved-metadata.jsonl`); nothing is rendered or screened.
    #[arg(long, requires = "buckets")]
    metadata_only: bool,
    /// Keep only the source families listed in this file (one key per line),
    /// for acquiring one reserved split; others are counted only.
    #[arg(long)]
    families: Option<PathBuf>,
}

impl Select {
    fn apply(&self, run: &mut Run) -> Result<()> {
        if self.metadata_only {
            run.enable_metadata_only()?;
        }
        if let Some(f) = &self.families {
            let (sha, bytes) = io::file_sha(f)?;
            let keys = std::fs::read_to_string(f)?.lines().map(str::trim).filter(|l| !l.is_empty()).map(str::to_owned).collect();
            run.select_families(keys);
            run.input(json!({"role": "family_selection", "path": f, "sha256": sha, "bytes": bytes}));
        }
        Ok(())
    }
}

#[derive(Subcommand)]
enum Command {
    /// Project Gutenberg: manifest of downloaded pgN.txt + N.rdf pairs.
    Gutenberg {
        #[arg(long)]
        manifest: PathBuf,
        #[arg(long)]
        out: PathBuf,
        /// Body percent positions; the admitted precedent uses 20.
        #[arg(long, value_delimiter = ',', default_value = "20")]
        fractions: Vec<usize>,
        /// Optional read-only catalog inventory JSONL (stratum and aliases).
        #[arg(long)]
        inventory: Option<PathBuf>,
        #[arg(long)]
        max_candidates: Option<u64>,
        /// Keep only source families whose key hashes into buckets LO..HI of
        /// 0..1000 (see adapter::bucket); reserved-split buckets are never rendered.
        #[arg(long, value_parser = grammar_a_pools::adapter::parse_buckets)]
        buckets: Option<(u32, u32)>,
        /// Excerpt rule: v2 (quotation break and English screen, default) or
        /// v1 (the original book rule, to reproduce earlier rows).
        #[arg(long, default_value = "v2")]
        excerpt_rule: gutenberg::ExcerptRule,
        /// Keep only these source families (one key per line).
        #[arg(long)]
        families: Option<PathBuf>,
    },
    /// Metadata-only Gutenberg plan: inventory + bulk RDF feed + prior index
    /// -> per-candidate role (excluded / reserved_evaluation /
    /// held_family_bucket_reserved / train). Never opens book text.
    GutenbergPlan {
        #[arg(long)]
        inventory: PathBuf,
        /// Root of the extracted RDF feed (holds cache/epub/N/pgN.rdf).
        #[arg(long)]
        rdf_root: PathBuf,
        #[arg(long)]
        index: PathBuf,
        /// Extra prior writer name keys, one per line.
        #[arg(long)]
        extra_writers: Option<PathBuf>,
        /// Reserved bucket range for writers (evaluation) and families (held).
        #[arg(long, value_parser = grammar_a_pools::adapter::parse_buckets, default_value = "900..1000")]
        reserve: (u32, u32),
        #[arg(long)]
        out: PathBuf,
    },
    /// English Wikipedia pages-articles export (lead sections).
    Wikipedia {
        #[command(flatten)]
        dump: Dump,
        #[arg(long)]
        out: PathBuf,
        #[arg(long)]
        max_candidates: Option<u64>,
        /// Keep only source families whose key hashes into buckets LO..HI of
        /// 0..1000 (see adapter::bucket); reserved-split buckets are never rendered.
        #[arg(long, value_parser = grammar_a_pools::adapter::parse_buckets)]
        buckets: Option<(u32, u32)>,
        #[command(flatten)]
        select: Select,
    },
    /// English Wikinews pages-articles export (published article bodies).
    Wikinews {
        #[command(flatten)]
        dump: Dump,
        #[arg(long)]
        out: PathBuf,
        #[arg(long)]
        max_candidates: Option<u64>,
        /// Keep only source families whose key hashes into buckets LO..HI of
        /// 0..1000 (see adapter::bucket); reserved-split buckets are never rendered.
        #[arg(long, value_parser = grammar_a_pools::adapter::parse_buckets)]
        buckets: Option<(u32, u32)>,
        #[command(flatten)]
        select: Select,
    },
    /// Wikimedia sister project (wikivoyage, wikibooks with Cookbook, or
    /// wikisource) pages-articles export; Wikisource needs `--meta` from
    /// `wikisource-meta` over the same dump.
    Sister {
        #[arg(long)]
        project: sister::Project,
        #[command(flatten)]
        dump: Dump,
        #[arg(long)]
        out: PathBuf,
        /// Documents kept per split group (default: wikivoyage 1, wikibooks 10, wikisource 5).
        #[arg(long)]
        max_per_group: Option<usize>,
        /// Wikisource metadata rows (`meta.jsonl` of `wikisource-meta`).
        #[arg(long)]
        meta: Option<PathBuf>,
        #[arg(long)]
        max_candidates: Option<u64>,
        /// Keep only units whose split-group key hashes into buckets LO..HI.
        #[arg(long, value_parser = grammar_a_pools::adapter::parse_buckets)]
        buckets: Option<(u32, u32)>,
        #[command(flatten)]
        select: Select,
    },
    /// F1000Research reviewer reports from PMC Open Access JATS XML (a directory or .zip).
    F1000 {
        #[arg(long)]
        input: PathBuf,
        #[arg(long)]
        corpus: String,
        #[arg(long)]
        out: PathBuf,
        #[arg(long, default_value_t = 5)]
        max_per_group: usize,
        #[arg(long, default_value_t = 5)]
        min_template_reports: usize,
        #[arg(long)]
        max_candidates: Option<u64>,
        /// Keep only reports whose reviewer key hashes into buckets LO..HI.
        #[arg(long, value_parser = grammar_a_pools::adapter::parse_buckets)]
        buckets: Option<(u32, u32)>,
        #[command(flatten)]
        select: Select,
    },
    /// OpenReview reviews from the ORB v1.0.1 export (Python-literal file).
    Orb {
        #[arg(long)]
        input: PathBuf,
        #[arg(long)]
        corpus: String,
        #[arg(long)]
        out: PathBuf,
        #[arg(long, default_value_t = 5)]
        max_per_group: usize,
        #[arg(long, default_value_t = 5)]
        min_template_reports: usize,
        #[arg(long)]
        max_candidates: Option<u64>,
        /// Keep only reviews whose submission key hashes into buckets LO..HI.
        #[arg(long, value_parser = grammar_a_pools::adapter::parse_buckets)]
        buckets: Option<(u32, u32)>,
        #[command(flatten)]
        select: Select,
    },
    /// First Wikisource pass: text-free work and scan-index metadata rows.
    WikisourceMeta {
        #[command(flatten)]
        dump: Dump,
        #[arg(long)]
        out: PathBuf,
    },
    /// One Stack Exchange site's Posts.xml (and optional Users.xml).
    Stackexchange {
        /// Posts.xml stream; `-` reads stdin (for example `7z x -so site.7z Posts.xml |`).
        #[arg(long)]
        posts: PathBuf,
        #[arg(long)]
        users: Option<PathBuf>,
        /// Site domain, for example `english.stackexchange.com`.
        #[arg(long)]
        site: String,
        #[arg(long)]
        dump_name: String,
        #[arg(long)]
        dump_url: Option<String>,
        #[arg(long)]
        dump_sha256: Option<String>,
        #[arg(long)]
        out: PathBuf,
        #[arg(long)]
        max_candidates: Option<u64>,
        /// Keep only source families whose key hashes into buckets LO..HI of
        /// 0..1000 (see adapter::bucket); reserved-split buckets are never rendered.
        #[arg(long, value_parser = grammar_a_pools::adapter::parse_buckets)]
        buckets: Option<(u32, u32)>,
        #[command(flatten)]
        select: Select,
    },
    /// PLOS JATS XML: a directory walked for *.xml, one file, or a .zip archive (for example allofplos.zip).
    Plos {
        #[arg(long)]
        input: PathBuf,
        #[arg(long)]
        corpus: String,
        #[arg(long)]
        corpus_sha256: Option<String>,
        #[arg(long)]
        include_introduction: bool,
        /// Only articles whose CC BY or CC0 statement is prose without a
        /// Creative Commons link (user decision 2026-10-27); link-licensed
        /// articles are counted and skipped.
        #[arg(long)]
        prose_licence_only: bool,
        #[arg(long)]
        out: PathBuf,
        #[arg(long)]
        max_candidates: Option<u64>,
        /// Keep only source families whose key hashes into buckets LO..HI of
        /// 0..1000 (see adapter::bucket); reserved-split buckets are never rendered.
        #[arg(long, value_parser = grammar_a_pools::adapter::parse_buckets)]
        buckets: Option<(u32, u32)>,
        #[command(flatten)]
        select: Select,
    },
    /// US Federal Register daily bulk XML (FR-YYYY-MM-DD.xml): a directory, one file, or a yearly govinfo .zip.
    FederalRegister {
        #[arg(long)]
        input: PathBuf,
        #[arg(long)]
        corpus: String,
        #[arg(long)]
        out: PathBuf,
        #[arg(long)]
        max_candidates: Option<u64>,
        /// Keep only source families whose key hashes into buckets LO..HI of
        /// 0..1000 (see adapter::bucket); reserved-split buckets are never rendered.
        #[arg(long, value_parser = grammar_a_pools::adapter::parse_buckets)]
        buckets: Option<(u32, u32)>,
        #[command(flatten)]
        select: Select,
    },
    /// FineWeb parquet shards: seeded row sample, page gates, whole prose paragraphs.
    Fineweb {
        /// Parquet files (repeatable), read in the order given.
        #[arg(long = "input", required = true)]
        inputs: Vec<PathBuf>,
        /// Downloaded public_suffix_list.dat.
        #[arg(long)]
        psl: PathBuf,
        #[arg(long)]
        seed: String,
        #[arg(long, default_value_t = 40000)]
        target_rows_per_file: u64,
        #[arg(long, default_value_t = 0.8)]
        min_language_score: f64,
        /// Registered domains of other pool sources (one per line).
        #[arg(long)]
        exclude_domains: PathBuf,
        #[arg(long)]
        dataset_revision: String,
        #[arg(long)]
        out: PathBuf,
        #[arg(long)]
        max_candidates: Option<u64>,
        /// Keep only pages whose registered-domain key hashes into buckets LO..HI.
        #[arg(long, value_parser = grammar_a_pools::adapter::parse_buckets)]
        buckets: Option<(u32, u32)>,
        #[command(flatten)]
        select: Select,
    },
    /// Student and learner essay CSV releases (PERSUADE 2.0, ELLIPSE, ASAP 2.0).
    StudentEssays {
        /// persuade2, ellipse or asap2.
        #[arg(long)]
        corpus: String,
        /// SPLIT=PATH of a release table; a zip archive (by magic bytes) is read
        /// in place and its one CSV entry used.
        #[arg(long, required = true)]
        input: Vec<String>,
        /// SPLIT=PASSWORD for an encrypted zip entry (the published password).
        #[arg(long)]
        password: Vec<String>,
        /// `essay-ids.jsonl` of a PERSUADE run (required for ELLIPSE and ASAP 2.0).
        #[arg(long)]
        persuade_ids: Option<PathBuf>,
        /// YYYY-MM-DD terminus for a corpus without built-in date evidence.
        #[arg(long, requires = "collection_terminus_basis")]
        collection_terminus: Option<String>,
        #[arg(long, requires = "collection_terminus")]
        collection_terminus_basis: Option<String>,
        #[arg(long)]
        out: PathBuf,
        #[arg(long)]
        max_candidates: Option<u64>,
        /// Keep only essays whose prompt key hashes into buckets LO..HI.
        #[arg(long, value_parser = grammar_a_pools::adapter::parse_buckets)]
        buckets: Option<(u32, u32)>,
        #[command(flatten)]
        select: Select,
    },
    /// Global Voices WordPress REST post pages (page-*.json in a directory).
    GlobalVoices {
        #[arg(long)]
        posts_dir: PathBuf,
        #[arg(long)]
        out: PathBuf,
        #[arg(long)]
        max_candidates: Option<u64>,
        /// Keep only posts whose author-account key hashes into buckets LO..HI.
        #[arg(long, value_parser = grammar_a_pools::adapter::parse_buckets)]
        buckets: Option<(u32, u32)>,
        #[command(flatten)]
        select: Select,
    },
    /// Enron emails (CMU 2015-05-07 tar.gz): the owner's sent messages.
    Enron {
        #[arg(long)]
        archive: PathBuf,
        #[arg(long)]
        archive_url: String,
        #[arg(long)]
        out: PathBuf,
        #[arg(long)]
        max_candidates: Option<u64>,
        /// Keep only messages whose mailbox-owner key hashes into buckets LO..HI.
        #[arg(long, value_parser = grammar_a_pools::adapter::parse_buckets)]
        buckets: Option<(u32, u32)>,
        #[command(flatten)]
        select: Select,
    },
    /// EuroParl v7 source release (europarl.tgz): English-original speaker turns.
    Europarl {
        #[arg(long)]
        archive: PathBuf,
        #[arg(long)]
        archive_url: String,
        #[arg(long)]
        out: PathBuf,
        #[arg(long)]
        max_candidates: Option<u64>,
        /// Keep only turns whose speaker key hashes into buckets LO..HI.
        #[arg(long, value_parser = grammar_a_pools::adapter::parse_buckets)]
        buckets: Option<(u32, u32)>,
        #[command(flatten)]
        select: Select,
    },
    /// NIH RePORTER ExPORTER fiscal-year project and abstract zips.
    NihExporter {
        /// RePORTER_PRJ_C_FYyyyy.zip files (repeatable).
        #[arg(long = "projects")]
        projects: Vec<PathBuf>,
        /// RePORTER_PRJABS_C_FYyyyy.zip files (repeatable).
        #[arg(long = "abstracts")]
        abstracts: Vec<PathBuf>,
        #[arg(long)]
        out: PathBuf,
        #[arg(long)]
        max_candidates: Option<u64>,
        /// Keep only abstracts whose contact-PI key hashes into buckets LO..HI.
        #[arg(long, value_parser = grammar_a_pools::adapter::parse_buckets)]
        buckets: Option<(u32, u32)>,
        #[command(flatten)]
        select: Select,
    },
    /// USPTO pre-grant publication full text (APPXML weekly zips): background sections.
    UsptoAppxml {
        /// Weekly APPXML zips (repeatable); a sibling `<zip>.receipt.json` gives the download URL.
        #[arg(long = "week")]
        weeks: Vec<PathBuf>,
        #[arg(long)]
        out: PathBuf,
        #[arg(long)]
        max_candidates: Option<u64>,
        /// Keep only applications whose writer key hashes into buckets LO..HI.
        #[arg(long, value_parser = grammar_a_pools::adapter::parse_buckets)]
        buckets: Option<(u32, u32)>,
        #[command(flatten)]
        select: Select,
    },
    /// Join USPTO application families and apply the family, writer,
    /// assignee and publication-year caps (see `uspto::select`).
    UsptoSelect {
        #[arg(long)]
        train: PathBuf,
        /// Metadata-only scan directories (reserved buckets), repeatable.
        #[arg(long = "scan")]
        scans: Vec<PathBuf>,
        #[arg(long)]
        out: PathBuf,
        #[arg(long)]
        seed: String,
        #[arg(long)]
        max_per_writer: usize,
        #[arg(long)]
        max_per_assignee: usize,
        #[arg(long)]
        max_per_year: usize,
        /// Folded window length of the near-duplicate screen (0 disables it).
        #[arg(long, default_value_t = 12)]
        near_window: usize,
    },
    /// Admission assertions on a finished USPTO pool (dates before 2019-01-01, one row per family, caps).
    UsptoCheck {
        #[arg(long)]
        documents: PathBuf,
        /// The pool's reserved-metadata.jsonl (re-joined with the documents).
        #[arg(long)]
        reserved_metadata: PathBuf,
        #[arg(long)]
        max_per_writer: usize,
        #[arg(long)]
        max_per_assignee: usize,
    },
    /// AMI Meeting Corpus manual annotations zip (NXT words and segments).
    Ami {
        #[arg(long)]
        zip: PathBuf,
        #[arg(long)]
        out: PathBuf,
        #[arg(long)]
        max_candidates: Option<u64>,
        /// Keep only meetings whose participant-component key hashes into buckets LO..HI.
        #[arg(long, value_parser = grammar_a_pools::adapter::parse_buckets)]
        buckets: Option<(u32, u32)>,
        #[command(flatten)]
        select: Select,
    },
    /// ICSI Meeting Corpus original MRT transcripts zip.
    Icsi {
        #[arg(long)]
        zip: PathBuf,
        #[arg(long)]
        out: PathBuf,
        #[arg(long)]
        max_candidates: Option<u64>,
        /// Keep only meetings whose participant-component key hashes into buckets LO..HI.
        #[arg(long, value_parser = grammar_a_pools::adapter::parse_buckets)]
        buckets: Option<(u32, u32)>,
        #[command(flatten)]
        select: Select,
    },
    /// Santa Barbara Corpus CHAT zip (evaluation-only diagnostic rows).
    Sbcsae {
        #[arg(long)]
        zip: PathBuf,
        #[arg(long)]
        out: PathBuf,
        #[arg(long)]
        max_candidates: Option<u64>,
        /// Keep only recordings whose key hashes into buckets LO..HI.
        #[arg(long, value_parser = grammar_a_pools::adapter::parse_buckets)]
        buckets: Option<(u32, u32)>,
        #[command(flatten)]
        select: Select,
    },
    /// Reverse overlap check: stream other pools' JSONL rows against an
    /// index built from this pool's candidates; writes text-free hits.
    OverlapScan {
        #[arg(long)]
        index: PathBuf,
        #[arg(long = "input", required = true)]
        inputs: Vec<PathBuf>,
        #[arg(long)]
        out: PathBuf,
    },
    /// Word list for OCR quality measures and line-end hyphen decisions, from the `text` field of clean JSONL files.
    OcrLexicon {
        #[arg(long = "input", required = true)]
        inputs: Vec<PathBuf>,
        #[arg(long, default_value_t = 3)]
        min_count: u64,
        #[arg(long, default_value_t = 2)]
        min_docs: u64,
        #[arg(long)]
        out: PathBuf,
    },
    /// Library of Congress By the People dataset ZIPs listed in a campaign manifest.
    LocBtp {
        #[arg(long)]
        manifest: PathBuf,
        #[arg(long)]
        datasets_dir: PathBuf,
        /// `lexicon.txt` from `ocr-lexicon`.
        #[arg(long)]
        lexicon: PathBuf,
        #[arg(long, default_value_t = 3)]
        max_runs: usize,
        #[arg(long)]
        out: PathBuf,
        #[arg(long)]
        max_candidates: Option<u64>,
        /// Keep only items whose collection key hashes into buckets LO..HI.
        #[arg(long, value_parser = grammar_a_pools::adapter::parse_buckets)]
        buckets: Option<(u32, u32)>,
        #[command(flatten)]
        select: Select,
    },
    /// American Stories per-year scan archives (faro_<year>.tar.gz).
    AmericanStories {
        #[arg(long = "input", required = true)]
        inputs: Vec<PathBuf>,
        #[arg(long)]
        seed: String,
        /// Share of articles visited (by identifier rank).
        #[arg(long)]
        sample_fraction: f64,
        #[arg(long)]
        dataset_revision: String,
        #[arg(long)]
        lexicon: PathBuf,
        #[arg(long)]
        out: PathBuf,
        #[arg(long)]
        max_candidates: Option<u64>,
        /// Keep only articles whose newspaper (LCCN) key hashes into buckets LO..HI.
        #[arg(long, value_parser = grammar_a_pools::adapter::parse_buckets)]
        buckets: Option<(u32, u32)>,
        #[command(flatten)]
        select: Select,
    },
    /// British Library Heritage Made Digital newspaper parquet shards.
    BlHmd {
        #[arg(long = "input", required = true)]
        inputs: Vec<PathBuf>,
        #[arg(long)]
        seed: String,
        #[arg(long)]
        sample_fraction: f64,
        #[arg(long)]
        dataset_revision: String,
        #[arg(long)]
        lexicon: PathBuf,
        /// Admitted `item_type` values.
        #[arg(long, value_delimiter = ',', default_value = "ARTICLE")]
        item_types: Vec<String>,
        #[arg(long, default_value_t = 0.0)]
        min_ocr_confidence: f64,
        #[arg(long)]
        out: PathBuf,
        #[arg(long)]
        max_candidates: Option<u64>,
        /// Keep only articles whose newspaper-title key hashes into buckets LO..HI.
        #[arg(long, value_parser = grammar_a_pools::adapter::parse_buckets)]
        buckets: Option<(u32, u32)>,
        #[command(flatten)]
        select: Select,
    },
    /// Caselaw Access Project static.case.law volume zips (`<reporter>__<volume>.zip`).
    Cap {
        #[arg(long)]
        zips_dir: PathBuf,
        #[arg(long)]
        out: PathBuf,
        #[arg(long)]
        max_candidates: Option<u64>,
        /// Keep only opinions whose judge key hashes into buckets LO..HI.
        #[arg(long, value_parser = grammar_a_pools::adapter::parse_buckets)]
        buckets: Option<(u32, u32)>,
        #[command(flatten)]
        select: Select,
    },
    /// CourtListener bulk opinions CSV (decompressed; `-` is stdin) joined to the clusters CSV.
    Courtlistener {
        #[arg(long)]
        opinions: PathBuf,
        #[arg(long)]
        clusters: PathBuf,
        /// SHA-256 of the downloaded compressed files, from the runner.
        #[arg(long)]
        opinions_sha256: Option<String>,
        #[arg(long)]
        clusters_sha256: Option<String>,
        #[arg(long)]
        snapshot: String,
        #[arg(long)]
        out: PathBuf,
        #[arg(long)]
        max_candidates: Option<u64>,
        /// Keep only opinions whose linked author key hashes into buckets LO..HI.
        #[arg(long, value_parser = grammar_a_pools::adapter::parse_buckets)]
        buckets: Option<(u32, u32)>,
        #[command(flatten)]
        select: Select,
    },
    /// GovInfo Congressional hearings (`CHRG-*.htm` in a directory).
    GovinfoHearings {
        #[arg(long)]
        htm_dir: PathBuf,
        #[arg(long)]
        out: PathBuf,
        #[arg(long)]
        max_candidates: Option<u64>,
        /// Keep only hearings whose family key hashes into buckets LO..HI.
        #[arg(long, value_parser = grammar_a_pools::adapter::parse_buckets)]
        buckets: Option<(u32, u32)>,
        #[command(flatten)]
        select: Select,
    },
    /// GovInfo Daily Compilation of Presidential Documents (`DCPD-*.htm`).
    GovinfoCpd {
        #[arg(long)]
        htm_dir: PathBuf,
        #[arg(long)]
        out: PathBuf,
        #[arg(long)]
        max_candidates: Option<u64>,
        /// Keep only documents whose president key hashes into buckets LO..HI.
        #[arg(long, value_parser = grammar_a_pools::adapter::parse_buckets)]
        buckets: Option<(u32, u32)>,
        #[command(flatten)]
        select: Select,
    },
    /// Extract the text layer of every PDF in a directory (no OCR).
    PdfText {
        #[arg(long)]
        input: PathBuf,
        #[arg(long)]
        out: PathBuf,
    },
    /// FOMC transcripts from `pdf-text` output (`FOMC<date><kind>.txt`).
    Fomc {
        #[arg(long)]
        text_dir: PathBuf,
        #[arg(long)]
        out: PathBuf,
        #[arg(long)]
        max_candidates: Option<u64>,
        /// Keep only turns whose speaker key hashes into buckets LO..HI.
        #[arg(long, value_parser = grammar_a_pools::adapter::parse_buckets)]
        buckets: Option<(u32, u32)>,
        #[command(flatten)]
        select: Select,
    },
    /// Identifier-only post-selection of an adapter output: blocked cases and a per-group cap.
    SelectCandidates {
        #[arg(long)]
        source: PathBuf,
        #[arg(long)]
        out: PathBuf,
        #[arg(long)]
        per_group: Option<usize>,
        #[arg(long, default_value = "writer_key")]
        group_field: String,
        #[arg(long)]
        blocked_case_keys: Option<PathBuf>,
    },
    /// Review corpus (Amazon Reviews 2023 review_categories JSONL.gz files),
    /// sampled by seeded rank under per-file quotas and a per-reviewer cap.
    Reviews {
        /// `amazon`.
        #[arg(long)]
        source: String,
        /// Input files (repeatable); one quota per file.
        #[arg(long = "input", required = true)]
        inputs: Vec<PathBuf>,
        #[arg(long)]
        seed: String,
        /// Total candidates, split over inputs in proportion to the square
        /// root of each file's size (one input takes the whole total).
        #[arg(long)]
        target_total: u64,
        #[arg(long, default_value_t = 200)]
        quota_floor: u64,
        #[arg(long, default_value_t = 2)]
        max_per_user: u64,
        #[arg(long, default_value_t = 4)]
        threads: usize,
        /// Terms pages archived before the run (repeatable); hashed into the report.
        #[arg(long = "terms-evidence")]
        terms_evidence: Vec<PathBuf>,
        #[arg(long)]
        out: PathBuf,
        /// Keep only reviews whose opaque reviewer key hashes into buckets LO..HI.
        #[arg(long, value_parser = grammar_a_pools::adapter::parse_buckets)]
        buckets: Option<(u32, u32)>,
    },
    /// Webis-TLDR-17 Reddit posts (the Zenodo zip, read in place).
    WebisTldr {
        #[arg(long)]
        zip: PathBuf,
        /// SHA-256 of the zip, computed by the runner.
        #[arg(long)]
        zip_sha256: Option<String>,
        #[arg(long)]
        out: PathBuf,
        #[arg(long)]
        max_candidates: Option<u64>,
        /// Keep only posts whose opaque author key hashes into buckets LO..HI.
        #[arg(long, value_parser = grammar_a_pools::adapter::parse_buckets)]
        buckets: Option<(u32, u32)>,
        #[command(flatten)]
        select: Select,
    },
    /// Common Pile Ubuntu IRC shards (`*.jsonl.gz`, one channel-day per row).
    UbuntuIrc {
        #[arg(long = "input", required = true)]
        inputs: Vec<PathBuf>,
        #[arg(long)]
        dataset_revision: String,
        #[arg(long)]
        out: PathBuf,
        #[arg(long)]
        max_candidates: Option<u64>,
        /// Keep only speakers whose opaque channel+nick key hashes into buckets LO..HI.
        #[arg(long, value_parser = grammar_a_pools::adapter::parse_buckets)]
        buckets: Option<(u32, u32)>,
        #[command(flatten)]
        select: Select,
    },
    /// ConvoKit WikiConv English corpora (`full.corpus.zip`, repeatable).
    Wikiconv {
        #[arg(long = "input", required = true)]
        inputs: Vec<PathBuf>,
        #[arg(long)]
        out: PathBuf,
        #[arg(long)]
        max_candidates: Option<u64>,
        /// Keep only utterances whose opaque user key hashes into buckets LO..HI.
        #[arg(long, value_parser = grammar_a_pools::adapter::parse_buckets)]
        buckets: Option<(u32, u32)>,
        #[command(flatten)]
        select: Select,
    },
    /// Keep at most N candidates per writer_key (seeded rank) in a new
    /// adapter-shaped directory.
    CapContributors {
        #[arg(long)]
        input: PathBuf,
        #[arg(long)]
        out: PathBuf,
        #[arg(long)]
        max_per_contributor: usize,
        #[arg(long)]
        seed: String,
        /// Optional seeded document sample before the cap (1..=1000 per mille).
        #[arg(long)]
        keep_per_mille: Option<u64>,
    },
    /// Text-free reservation rows from a Wikipedia multistream index read on
    /// stdin (`offset:page_id:title`), for article-like titles in `--buckets`.
    WikipediaIndexReserve {
        #[arg(long, value_parser = grammar_a_pools::adapter::parse_buckets)]
        buckets: (u32, u32),
        #[arg(long)]
        out: PathBuf,
    },
    /// Reserve Test, CAL, Dev-select and Dev-iter from metadata only and split
    /// admitted pools into Train and Train-mining (see `splits`).
    ReserveSplits {
        #[arg(long)]
        config: PathBuf,
    },
    /// Extend a base split directory (kept byte for byte) with new Train-side
    /// sources and two new evaluation domains (see `splits_v3`).
    ExtendSplits {
        #[arg(long)]
        config: PathBuf,
    },
    /// Extend splits-v3 and the blog splits (kept byte for byte) with the new
    /// human pools and their evaluation reservations (see `splits_v4`).
    ExtendSplitsV4 {
        #[arg(long)]
        config: PathBuf,
    },
    /// FineWeb blog-register pages: every row whose URL is a blog page, first-person whole prose paragraphs (see `fineweb_blogs`).
    FinewebBlogs {
        #[arg(long = "input", required = true)]
        inputs: Vec<PathBuf>,
        #[arg(long)]
        psl: PathBuf,
        #[arg(long, default_value_t = 0.8)]
        min_language_score: f64,
        #[arg(long)]
        exclude_domains: PathBuf,
        #[arg(long)]
        dataset_revision: String,
        /// FineWeb pool metadata scan (`reserved-metadata.jsonl` directory): hosted blogs with a page there inherit its site group.
        #[arg(long)]
        inherit_scan: Vec<PathBuf>,
        #[arg(long)]
        out: PathBuf,
        #[arg(long)]
        max_candidates: Option<u64>,
        /// Keep only pages whose blog group key hashes into buckets LO..HI.
        #[arg(long, value_parser = grammar_a_pools::adapter::parse_buckets)]
        buckets: Option<(u32, u32)>,
        #[command(flatten)]
        select: Select,
    },
    /// PLOS Blogs WordPress REST post pages, one directory per blog (see `plos_blogs`).
    PlosBlogs {
        #[arg(long)]
        posts_root: PathBuf,
        #[arg(long)]
        authors_root: Option<PathBuf>,
        /// Current blogs (subdomain names), repeatable.
        #[arg(long = "blog", required = true)]
        blogs: Vec<String>,
        /// Archived blogs (rejected whole), repeatable.
        #[arg(long = "archived")]
        archived: Vec<String>,
        #[arg(long)]
        out: PathBuf,
        #[arg(long)]
        max_candidates: Option<u64>,
    },
    /// Blog split sibling of a base split directory (see `blog_splits`).
    BlogSplits {
        #[arg(long)]
        config: PathBuf,
    },
    /// Per-blog writer keys for a blog split directory, written as a successor (see `blog_rekey`).
    BlogRekey {
        #[arg(long)]
        config: PathBuf,
    },
    /// Print `key<TAB>bucket` for family keys read one per line from stdin,
    /// using the same bucket function as `--buckets`.
    Bucket,
    /// Build the protected index of every registered prior snapshot.
    IndexPrior {
        #[arg(long)]
        registry: PathBuf,
        /// Base for relative registry paths (the repository root).
        #[arg(long)]
        root: PathBuf,
        #[arg(long)]
        out: PathBuf,
        /// Keep only keys that occur in these adapter outputs' candidates
        /// (repeatable): a probe-filtered index, valid only for deduplicating
        /// exactly those candidates.
        #[arg(long = "probe")]
        probes: Vec<PathBuf>,
        #[arg(long, default_value_t = 1)]
        threads: usize,
        /// Bounded memory: spill sorted key runs at this many keys per kind (0 = in memory).
        #[arg(long, default_value_t = 0)]
        spill_keys: usize,
        #[arg(long)]
        spill_dir: Option<PathBuf>,
    },
    /// Acquire one reserved evaluation split from its admitted dedup output
    /// under the sealed rule. Test is refused unless the written protocol
    /// seal (`--seal`, `--seal-sha256`, `"sealed": true`), a `--reservation`
    /// entry of a v2 config and the RAID screen outputs are given.
    SelectEval {
        /// Split directory with SEALED.json, groups.jsonl and manifest.jsonl.
        #[arg(long)]
        split_dir: PathBuf,
        /// Split config (seed, targets, excluded domains, backfill domain).
        #[arg(long)]
        config: PathBuf,
        /// Dedup output for this split's candidates.
        #[arg(long)]
        dedup: PathBuf,
        /// Adapter output directories (for text-free rejection reasons; repeatable).
        #[arg(long = "adapter")]
        adapters: Vec<PathBuf>,
        /// Diagnostic only: JSONL rows whose folded short windows are compared with selected documents (repeatable).
        #[arg(long = "near-reference")]
        near_reference: Vec<PathBuf>,
        #[arg(long, default_value = "us_federal_register_historical")]
        near_collection: String,
        #[arg(long, default_value_t = 12)]
        near_window: usize,
        /// Reservation entry of a v2 select config (`reservations.<name>`).
        #[arg(long)]
        reservation: Option<String>,
        /// Written protocol seal (required for Test).
        #[arg(long)]
        seal: Option<PathBuf>,
        #[arg(long)]
        seal_sha256: Option<String>,
        /// RAID screen output directories (`raid-s1`, `raid-s2`, `raid-s3`).
        #[arg(long)]
        raid_s1: Option<PathBuf>,
        #[arg(long)]
        raid_s2: Option<PathBuf>,
        #[arg(long)]
        raid_s3: Option<PathBuf>,
        /// Near-window exclusion without replacement: Train-side rows (JSONL) of the collection below.
        #[arg(long)]
        near_exclude_train: Option<PathBuf>,
        #[arg(long, default_value = "global_voices_historical")]
        near_exclude_collection: String,
        #[arg(long, default_value_t = 12)]
        near_exclude_window: usize,
        #[arg(long)]
        out: PathBuf,
    },
    /// RAID screen S1 (title lineage) over reservation metadata only: skip
    /// records and screened family lists (`NAME=DIR`, repeatable).
    RaidS1 {
        #[arg(long)]
        record: PathBuf,
        #[arg(long = "reservation", required = true)]
        reservations: Vec<String>,
        #[arg(long)]
        out: PathBuf,
    },
    /// RAID screen S2 (Reddit author lineage): every Webis-TLDR-17 post of
    /// each reserved author group of one reservation (`NAME=DIR`). Reads
    /// evaluation prose: Test needs the written seal.
    RaidS2 {
        #[arg(long)]
        record: PathBuf,
        #[arg(long)]
        reservation: String,
        #[arg(long)]
        webis_zip: PathBuf,
        #[arg(long)]
        webis_zip_sha256: String,
        #[arg(long)]
        seal: Option<PathBuf>,
        #[arg(long)]
        seal_sha256: Option<String>,
        #[arg(long)]
        out: PathBuf,
    },
    /// RAID screen S3 (text lineage) of adapter candidates (repeatable
    /// `--adapter`). Reads evaluation prose: Test needs the written seal.
    RaidS3 {
        #[arg(long)]
        record: PathBuf,
        #[arg(long = "adapter", required = true)]
        adapters: Vec<PathBuf>,
        /// Split the candidates belong to (`test` requires the seal).
        #[arg(long)]
        split: String,
        #[arg(long)]
        seal: Option<PathBuf>,
        #[arg(long)]
        seal_sha256: Option<String>,
        #[arg(long)]
        out: PathBuf,
    },
    /// Merge select-eval outputs of one split's reservations into one
    /// evaluation-human directory (same files and schema).
    MergeEval {
        /// select-eval output directories, in merge order (repeatable).
        #[arg(long = "part", required = true)]
        parts: Vec<PathBuf>,
        /// v2 select config (split target and minimum).
        #[arg(long)]
        config: PathBuf,
        #[arg(long)]
        seal: Option<PathBuf>,
        #[arg(long)]
        seal_sha256: Option<String>,
        #[arg(long)]
        out: PathBuf,
    },
    /// Map labelled RAID CSVs (train, extra) to candidate rows with rights,
    /// generator, decoding and attack metadata; non-English and code domains,
    /// non-commercial components and out-of-bound texts are not candidates.
    RaidAdapt {
        /// `PATH=SPLIT=SHA256` of a RAID CSV (`.zst` decoded), the RAID split
        /// name and the expected SHA-256 of the decompressed CSV (repeatable).
        #[arg(long = "input", required = true)]
        inputs: Vec<String>,
        #[arg(long)]
        out: PathBuf,
        /// Attacked rows kept per thousand (seeded by RAID id).
        #[arg(long, default_value_t = 100)]
        attack_per_mille: u32,
        /// Smoke runs only: stop after this many candidates (report incomplete).
        #[arg(long)]
        max_rows: Option<u64>,
    },
    /// Admit RAID candidates against a probe-filtered prior index into a pool
    /// directory (documents, metadata, admission records, SHA256SUMS).
    RaidAdmit {
        #[arg(long)]
        candidates: PathBuf,
        #[arg(long)]
        index: PathBuf,
        #[arg(long)]
        out: PathBuf,
    },
    /// Deduplicate adapter outputs against the index and each other.
    Dedup {
        #[arg(long)]
        index: PathBuf,
        /// Adapter output directories (candidates.jsonl + report.json).
        #[arg(long = "source", required = true)]
        sources: Vec<PathBuf>,
        #[arg(long)]
        out: PathBuf,
        /// Shared folded 50-word sequences that make a near duplicate.
        #[arg(long, default_value_t = 1)]
        min_shared: usize,
        /// Window length of the within-collection short-window screen.
        #[arg(long, default_value_t = 0)]
        near_window: usize,
        /// Collection the short-window screen applies to (repeatable).
        #[arg(long = "near-collection")]
        near_collections: Vec<String>,
        /// Collection screened for MediaWiki rendering residue (repeatable).
        #[arg(long = "residue-collection")]
        residue_collections: Vec<String>,
        /// Alias whose families are excluded whole, for example `pg:37029` (repeatable).
        #[arg(long = "exclude-alias")]
        exclude_aliases: Vec<String>,
        /// `hits.jsonl` of a `scan-overlap` run; listed candidates are prior conflicts.
        #[arg(long)]
        prior_hits: Option<PathBuf>,
        /// `reserved-metadata.jsonl` of a metadata-only scan (repeatable): candidates of
        /// a source family listed there leave the pool with their whole family.
        #[arg(long = "reserved-metadata")]
        reserved_metadata: Vec<PathBuf>,
    },
    /// Reverse overlap scan: hold the candidates of small adapter outputs in
    /// memory and stream every registered file against them.
    ScanOverlap {
        #[arg(long)]
        registry: PathBuf,
        /// Base for relative registry paths (the repository root).
        #[arg(long)]
        root: PathBuf,
        /// Adapter output directories (candidates.jsonl).
        #[arg(long = "source", required = true)]
        sources: Vec<PathBuf>,
        #[arg(long)]
        out: PathBuf,
    },
}

fn stream_input(run: &mut Run, label: &str, path: &Path, reader: io::HashingReader<Box<dyn std::io::Read>>) {
    let (sha, bytes) = reader.finish();
    run.input(json!({"role": label, "path": path, "stream_sha256": sha, "stream_bytes": bytes,
                     "stream_note": "SHA-256 of the exact (decompressed) bytes the adapter read"}));
}

fn print(v: &serde_json::Value) {
    println!("{}", serde_json::to_string_pretty(v).unwrap_or_default());
}

fn mediawiki_run(wikipedia: bool, dump: Dump, out: &Path, max_candidates: Option<u64>, buckets: Option<(u32, u32)>, select: &Select) -> Result<()> {
    let (adapter, collection) = if wikipedia {
        ("wikipedia", "wikipedia_enwiki_historical")
    } else {
        ("wikinews", "wikinews_enwikinews_historical")
    };
    let info = mediawiki::DumpInfo { name: dump.dump_name.clone(), sha256: dump.dump_sha256.clone(), url: dump.dump_url.clone() };
    let mut run = Run::start_sampled(out, adapter, collection, mediawiki::dump_value(&info), max_candidates, buckets)?;
    select.apply(&mut run)?;
    let mut r = BufReader::with_capacity(1 << 20, io::HashingReader::new(io::open(&dump.dump)?));
    if wikipedia {
        mediawiki::wikipedia(&mut r, &info, &mut run)?;
    } else {
        mediawiki::wikinews(&mut r, &info, &mut run)?;
    }
    let partial = run.full();
    stream_input(&mut run, "dump", &dump.dump, r.into_inner());
    print(&run.finish(json!({"stopped_at_max_candidates": partial}))?);
    Ok(())
}

fn htm_files(dir: &Path, prefix: &str) -> Result<Vec<PathBuf>> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "htm") && p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with(prefix)))
        .collect();
    v.sort();
    ensure!(!v.is_empty(), "no {prefix}*.htm files in {}", dir.display());
    Ok(v)
}

fn main() -> Result<()> {
    match Args::parse().command {
        Command::Gutenberg { manifest, out, fractions, inventory, max_candidates, buckets, excerpt_rule, families } => {
            ensure!(!fractions.is_empty() && fractions.iter().all(|f| (1..100).contains(f)), "fractions must be 1..99");
            let params = json!({"fractions": fractions, "inventory": inventory, "excerpt_rule": format!("{excerpt_rule:?}").to_lowercase()});
            let mut run = Run::start_sampled(&out, "gutenberg", "project_gutenberg_historical", params, max_candidates, buckets)?;
            Select { metadata_only: false, families }.apply(&mut run)?;
            let (msha, mbytes) = io::file_sha(&manifest)?;
            run.input(json!({"role": "manifest", "path": manifest, "sha256": msha, "bytes": mbytes}));
            if let Some(p) = &inventory {
                let (s, b) = io::file_sha(p)?;
                run.input(json!({"role": "inventory_read_only", "path": p, "sha256": s, "bytes": b}));
            }
            let inv = gutenberg::Inventory::load(inventory.as_deref())?;
            gutenberg::adapt(&manifest, &fractions, &inv, excerpt_rule, &mut run)?;
            print(&run.finish(json!({}))?);
        }
        Command::GutenbergPlan { inventory, rdf_root, index, extra_writers, reserve, out } => {
            let o = grammar_a_pools::gutenberg_plan::Options {
                inventory: &inventory, rdf_root: &rdf_root, index: &index, extra_writers: extra_writers.as_deref(), reserve, out: &out,
            };
            print(&grammar_a_pools::gutenberg_plan::run(&o)?);
        }
        Command::Wikipedia { dump, out, max_candidates, buckets, select } => mediawiki_run(true, dump, &out, max_candidates, buckets, &select)?,
        Command::Wikinews { dump, out, max_candidates, buckets, select } => mediawiki_run(false, dump, &out, max_candidates, buckets, &select)?,
        Command::Sister { project, dump, out, max_per_group, meta, max_candidates, buckets, select } => {
            ensure!((project == sister::Project::Wikisource) == meta.is_some(), "--meta is required for wikisource and only for wikisource");
            let info = mediawiki::DumpInfo { name: dump.dump_name.clone(), sha256: dump.dump_sha256.clone(), url: dump.dump_url.clone() };
            let cap = max_per_group.unwrap_or(project.default_cap());
            ensure!(cap >= 1, "--max-per-group must be at least 1");
            let mut params = mediawiki::dump_value(&info);
            params["max_per_group"] = json!(cap);
            let mut run = Run::start_sampled(&out, project.adapter(), project.collection(), params, max_candidates, buckets)?;
            select.apply(&mut run)?;
            let m = match &meta {
                Some(p) => {
                    let (sha, bytes) = io::file_sha(p)?;
                    run.input(json!({"role": "wikisource_meta", "path": p, "sha256": sha, "bytes": bytes}));
                    Some(sister::Meta::load(p)?)
                }
                None => None,
            };
            let mut r = BufReader::with_capacity(1 << 20, io::HashingReader::new(io::open(&dump.dump)?));
            let extra = sister::adapt(&mut r, &sister::Options { project, dump: &info, max_per_group: cap, meta: m.as_ref() }, &mut run)?;
            stream_input(&mut run, "dump", &dump.dump, r.into_inner());
            print(&run.finish(extra)?);
        }
        Command::F1000 { input, corpus, out, max_per_group, min_template_reports, max_candidates, buckets, select } => {
            ensure!(max_per_group >= 1 && min_template_reports >= 2, "max_per_group >= 1 and min_template_reports >= 2");
            let source = xmldoc::Source::open(&input)?;
            let params = json!({"corpus": corpus, "max_per_group": max_per_group, "min_template_reports": min_template_reports,
                                "bucket_key": "f1000_reviewer:<opaque hash of the first reviewer's folded name>"});
            let mut run = Run::start_sampled(&out, "f1000", review::F1000, params, max_candidates, buckets)?;
            select.apply(&mut run)?;
            let mut d = source.describe();
            d["role"] = json!("pmc_oa_jats_input");
            d["input"] = json!(input);
            run.input(d);
            let extra = review::f1000(&source, &review::Options { corpus: &corpus, max_per_group, min_template_reports }, &mut run)?;
            print(&run.finish(extra)?);
        }
        Command::Orb { input, corpus, out, max_per_group, min_template_reports, max_candidates, buckets, select } => {
            ensure!(max_per_group >= 1 && min_template_reports >= 2, "max_per_group >= 1 and min_template_reports >= 2");
            let params = json!({"corpus": corpus, "max_per_group": max_per_group, "min_template_reports": min_template_reports,
                                "bucket_key": "openreview:forum:<submission id>"});
            let mut run = Run::start_sampled(&out, "orb", review::OPENREVIEW, params, max_candidates, buckets)?;
            select.apply(&mut run)?;
            let (sha, bytes) = io::file_sha(&input)?;
            run.input(json!({"role": "orb_export", "path": input, "sha256": sha, "bytes": bytes}));
            let extra = review::orb(&input, &review::Options { corpus: &corpus, max_per_group, min_template_reports }, &mut run)?;
            print(&run.finish(extra)?);
        }
        Command::WikisourceMeta { dump, out } => {
            let mut r = BufReader::with_capacity(1 << 20, io::HashingReader::new(io::open(&dump.dump)?));
            let mut v = sister::wikisource_meta(&mut r, &out)?;
            let (sha, bytes) = r.into_inner().finish();
            v["dump"] = json!({"name": dump.dump_name, "url": dump.dump_url, "declared_sha256": dump.dump_sha256, "stream_sha256": sha, "stream_bytes": bytes});
            io::write_json(&out.join("meta-report.json"), &v)?;
            print(&v);
        }
        Command::Stackexchange { posts, users, site, dump_name, dump_url, dump_sha256, out, max_candidates, buckets, select } => {
            let params = json!({"site": site, "dump": dump_name, "dump_url": dump_url, "declared_dump_sha256": dump_sha256});
            let mut run = Run::start_sampled(&out, "stackexchange", "stackexchange_historical", params, max_candidates, buckets)?;
            select.apply(&mut run)?;
            let names = match &users {
                Some(u) => {
                    let mut r = BufReader::with_capacity(1 << 20, io::HashingReader::new(io::open(u)?));
                    let n = stackexchange::user_names(&mut r)?;
                    stream_input(&mut run, "users", u, r.into_inner());
                    n
                }
                None => Default::default(),
            };
            let mut r = BufReader::with_capacity(1 << 20, io::HashingReader::new(io::open(&posts)?));
            let s = stackexchange::Site { domain: &site, dump: &dump_name, dump_sha256: dump_sha256.as_deref(), dump_url: dump_url.as_deref() };
            stackexchange::adapt(&mut r, &s, &names, &mut run)?;
            let partial = run.full();
            stream_input(&mut run, "posts", &posts, r.into_inner());
            print(&run.finish(json!({"stopped_at_max_candidates": partial}))?);
        }
        Command::Plos { input, corpus, corpus_sha256, include_introduction, prose_licence_only, out, max_candidates, buckets, select } => {
            let source = xmldoc::Source::open(&input)?;
            let mut params = json!({"corpus": corpus, "declared_corpus_sha256": corpus_sha256, "include_introduction": include_introduction});
            if prose_licence_only {
                params["prose_licence_only"] = json!(true);
            }
            let mut run = Run::start_sampled(&out, "plos", "plos_historical", params, max_candidates, buckets)?;
            select.apply(&mut run)?;
            let mut d = source.describe();
            d["role"] = json!("jats_input");
            d["input"] = json!(input);
            d["per_document_hashes"] = json!("extraction.unit.file_sha256");
            run.input(d);
            plos::adapt(&source, &plos::Options { corpus: &corpus, corpus_sha256: corpus_sha256.as_deref(), include_introduction, prose_licence_only }, &mut run)?;
            print(&run.finish(json!({}))?);
        }
        Command::FederalRegister { input, corpus, out, max_candidates, buckets, select } => {
            let source = xmldoc::Source::open(&input)?;
            let mut run = Run::start_sampled(&out, "federal-register", "us_federal_register_historical", json!({"corpus": corpus}), max_candidates, buckets)?;
            select.apply(&mut run)?;
            let mut d = source.describe();
            d["role"] = json!("bulk_xml_input");
            d["input"] = json!(input);
            d["per_document_hashes"] = json!("extraction.unit.file_sha256");
            run.input(d);
            fedreg::adapt(&source, &corpus, &mut run)?;
            print(&run.finish(json!({}))?);
        }
        Command::Fineweb { inputs, psl: psl_path, seed, target_rows_per_file, min_language_score, exclude_domains, dataset_revision, out, max_candidates, buckets, select } => {
            let list = std::fs::read_to_string(&psl_path)?;
            let p = psl::Psl::parse(&list);
            let excluded: std::collections::HashSet<String> = std::fs::read_to_string(&exclude_domains)?.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')).map(str::to_lowercase).collect();
            let params = json!({"seed": seed, "target_rows_per_file": target_rows_per_file, "min_language_score": min_language_score,
                                "dataset": "HuggingFaceFW/fineweb", "dataset_revision": dataset_revision, "bucket_key": "fineweb_site:<registered domain>"});
            let mut run = Run::start_sampled(&out, "fineweb", fineweb::COLLECTION, params, max_candidates, buckets)?;
            select.apply(&mut run)?;
            let (ps, pb) = io::file_sha(&psl_path)?;
            run.input(json!({"role": "public_suffix_list", "path": psl_path, "sha256": ps, "bytes": pb, "rules": p.rule_count}));
            let (es, eb) = io::file_sha(&exclude_domains)?;
            run.input(json!({"role": "excluded_registered_domains", "path": exclude_domains, "sha256": es, "bytes": eb, "domains": excluded.len()}));
            let o = fineweb::Options { inputs: &inputs, psl: &p, seed: &seed, target_rows_per_file, min_language_score, excluded_domains: &excluded, dataset_revision: &dataset_revision };
            let extra = fineweb::adapt(&o, &mut run)?;
            print(&run.finish(extra)?);
        }
        Command::ScanOverlap { registry, root, sources, out } => {
            let r = scan::run(&registry, &root, &sources, &out)?;
            print(&json!({"candidates": r["candidates"], "candidates_with_hits": r["candidates_with_hits"], "files_scanned": r["files_scanned"],
                          "file_status_counts": r["file_status_counts"], "entries_without_files": r["entries_without_files"]}));
        }
        Command::StudentEssays { corpus, input, password, persuade_ids, collection_terminus, collection_terminus_basis, out, max_candidates, buckets, select } => {
            let spec = essays::spec(&corpus)?;
            let inputs = essays::parse_inputs(&input, &password)?;
            let params = json!({"corpus": corpus, "inputs": inputs.iter().map(|i| json!({"split": i.split, "path": i.path, "encrypted": i.password.is_some()})).collect::<Vec<_>>(),
                                "bucket_key": "student_essay_prompt:<prompt slug>", "collection_terminus": collection_terminus, "collection_terminus_basis": collection_terminus_basis});
            let mut run = Run::start_sampled(&out, "student-essays", spec.collection, params, max_candidates, buckets)?;
            select.apply(&mut run)?;
            let term = collection_terminus.as_deref().zip(collection_terminus_basis.as_deref());
            let o = essays::Options { spec, inputs: &inputs, out: &out, persuade_ids: persuade_ids.as_deref(), collection_terminus: term };
            let extra = essays::adapt(&o, &mut run)?;
            print(&run.finish(extra)?);
        }
        Command::GlobalVoices { posts_dir, out, max_candidates, buckets, select } => {
            let pages = global_voices::page_files(&posts_dir)?;
            ensure!(!pages.is_empty(), "no page-*.json files in {}", posts_dir.display());
            let params = json!({"posts_dir": posts_dir, "pages": pages.len(), "bucket_key": "global_voices_author:<WordPress author id>"});
            let mut run = Run::start_sampled(&out, "global-voices", global_voices::COLLECTION, params, max_candidates, buckets)?;
            select.apply(&mut run)?;
            let extra = global_voices::adapt(&pages, &mut run)?;
            run.input(json!({"role": "wordpress_rest_post_pages", "dir": posts_dir, "files": extra["files"].as_array().map(Vec::len)}));
            print(&run.finish(extra)?);
        }
        Command::Enron { archive, archive_url, out, max_candidates, buckets, select } => {
            let mut run = Run::start_sampled(&out, "enron", grammar_a_pools::pile::ENRON_COLLECTION, json!({"archive": archive, "archive_url": archive_url, "bucket_key": "enron_mailbox:<opaque owner>"}), max_candidates, buckets)?;
            select.apply(&mut run)?;
            let extra = grammar_a_pools::pile::enron(&archive, &archive_url, &mut run)?;
            run.input(json!({"role": "enron_maildir_tar_gz", "path": archive, "sha256": extra["archive"]["sha256"], "bytes": extra["archive"]["bytes"]}));
            print(&run.finish(extra)?);
        }
        Command::Europarl { archive, archive_url, out, max_candidates, buckets, select } => {
            let mut run = Run::start_sampled(&out, "europarl", grammar_a_pools::pile::EUROPARL_COLLECTION, json!({"archive": archive, "archive_url": archive_url, "bucket_key": "europarl_speaker:<opaque speaker>"}), max_candidates, buckets)?;
            select.apply(&mut run)?;
            let extra = grammar_a_pools::pile::europarl(&archive, &archive_url, &mut run)?;
            run.input(json!({"role": "europarl_v7_source_release", "path": archive, "sha256": extra["archive"]["sha256"], "bytes": extra["archive"]["bytes"]}));
            print(&run.finish(extra)?);
        }
        Command::NihExporter { projects, abstracts, out, max_candidates, buckets, select } => {
            ensure!(!abstracts.is_empty() && !projects.is_empty(), "need --projects and --abstracts zips");
            let mut run = Run::start_sampled(&out, "nih-exporter", grammar_a_pools::pile::NIH_COLLECTION, json!({"projects": projects, "abstracts": abstracts, "bucket_key": "nih_contact_pi:<opaque PI id>"}), max_candidates, buckets)?;
            select.apply(&mut run)?;
            for p in projects.iter().chain(abstracts.iter()) {
                let (sha, bytes) = io::file_sha(p)?;
                run.input(json!({"role": "nih_exporter_zip", "path": p, "sha256": sha, "bytes": bytes}));
            }
            let extra = grammar_a_pools::pile::nih(&projects, &abstracts, &mut run)?;
            print(&run.finish(extra)?);
        }
        Command::UsptoAppxml { weeks, out, max_candidates, buckets, select } => {
            ensure!(!weeks.is_empty(), "need --week zips");
            let mut run = Run::start_sampled(&out, "uspto-appxml", grammar_a_pools::uspto::COLLECTION, json!({"weeks": weeks, "bucket_key": "uspto writer key (opaque practitioner, first inventor, applicant or assignee)"}), max_candidates, buckets)?;
            select.apply(&mut run)?;
            let extra = grammar_a_pools::uspto::appxml(&weeks, &mut run)?;
            for f in extra["files"].as_array().into_iter().flatten() {
                run.input(json!({"role": "uspto_appxml_week_zip", "file": f["file"], "url": f["url"], "sha256": f["sha256"], "bytes": f["bytes"]}));
            }
            print(&run.finish(extra)?);
        }
        Command::UsptoSelect { train, scans, out, seed, max_per_writer, max_per_assignee, max_per_year, near_window } => {
            let opt = grammar_a_pools::uspto::SelectOptions { max_per_writer, max_per_assignee, max_per_year, near_window, seed };
            print(&grammar_a_pools::uspto::select(&train, &scans, &out, &opt)?);
        }
        Command::UsptoCheck { documents, reserved_metadata, max_per_writer, max_per_assignee } => {
            print(&grammar_a_pools::uspto::check(&documents, &reserved_metadata, max_per_writer, max_per_assignee)?);
        }
        Command::Ami { zip, out, max_candidates, buckets, select } => {
            let mut run = Run::start_sampled(&out, "ami", grammar_a_pools::ami::COLLECTION, json!({"zip": zip, "bucket_key": "ami_participant_component:<smallest participant id>"}), max_candidates, buckets)?;
            select.apply(&mut run)?;
            let extra = grammar_a_pools::ami::adapt(&zip, &mut run)?;
            print(&run.finish(extra)?);
        }
        Command::Icsi { zip, out, max_candidates, buckets, select } => {
            let mut run = Run::start_sampled(&out, "icsi", grammar_a_pools::icsi::COLLECTION, json!({"zip": zip, "bucket_key": "icsi_participant_component:<smallest participant id>"}), max_candidates, buckets)?;
            select.apply(&mut run)?;
            let extra = grammar_a_pools::icsi::adapt(&zip, &mut run)?;
            print(&run.finish(extra)?);
        }
        Command::Sbcsae { zip, out, max_candidates, buckets, select } => {
            let mut run = Run::start_sampled(&out, "sbcsae", grammar_a_pools::sbcsae::COLLECTION, json!({"zip": zip, "bucket_key": "sbcsae_recording:<SBCnnn>"}), max_candidates, buckets)?;
            select.apply(&mut run)?;
            let extra = grammar_a_pools::sbcsae::adapt(&zip, &mut run)?;
            print(&run.finish(extra)?);
        }
        Command::OverlapScan { index, inputs, out } => {
            let v = grammar_a_pools::overlap_scan::scan(&index, &inputs, &out)?;
            print(&json!({"hits": v["hits"], "inputs": v["inputs"]}));
        }
        Command::OcrLexicon { inputs, min_count, min_docs, out } => {
            print(&ocr::build_lexicon(&inputs, min_count, min_docs, &out)?);
        }
        Command::LocBtp { manifest, datasets_dir, lexicon, max_runs, out, max_candidates, buckets, select } => {
            let lex = load_lexicon(&lexicon)?;
            let params = json!({"manifest": manifest, "datasets_dir": datasets_dir, "max_runs": max_runs, "bucket_key": "loc_btp_collection:<manifest collection key>"});
            let mut run = Run::start_sampled(&out, "loc-btp", btp::COLLECTION, params, max_candidates, buckets)?;
            select.apply(&mut run)?;
            lexicon_input(&mut run, &lexicon, &lex)?;
            let (ms, mb) = io::file_sha(&manifest)?;
            run.input(json!({"role": "btp_campaign_manifest", "path": manifest, "sha256": ms, "bytes": mb}));
            let extra = btp::adapt(&btp::Options { manifest: &manifest, datasets_dir: &datasets_dir, lexicon: &lex, max_runs }, &mut run)?;
            print(&run.finish(extra)?);
        }
        Command::AmericanStories { inputs, seed, sample_fraction, dataset_revision, lexicon, out, max_candidates, buckets, select } => {
            let lex = load_lexicon(&lexicon)?;
            let gate = ocr::Gate::NEWSPAPER;
            let params = json!({"seed": seed, "sample_fraction": sample_fraction, "dataset": american_stories::DATASET, "dataset_revision": dataset_revision,
                                "ocr_gate": gate.to_value(), "bucket_key": "american_stories_lccn:<LCCN>"});
            let mut run = Run::start_sampled(&out, "american-stories", american_stories::COLLECTION, params, max_candidates, buckets)?;
            select.apply(&mut run)?;
            lexicon_input(&mut run, &lexicon, &lex)?;
            let o = american_stories::Options { inputs: &inputs, seed: &seed, fraction: sample_fraction, revision: &dataset_revision, lexicon: &lex, gate };
            let extra = american_stories::adapt(&o, &mut run)?;
            print(&run.finish(extra)?);
        }
        Command::BlHmd { inputs, seed, sample_fraction, dataset_revision, lexicon, item_types, min_ocr_confidence, out, max_candidates, buckets, select } => {
            let lex = load_lexicon(&lexicon)?;
            let gate = ocr::Gate::NEWSPAPER;
            let types: std::collections::HashSet<String> = item_types.into_iter().collect();
            let params = json!({"seed": seed, "sample_fraction": sample_fraction, "dataset": hmd::DATASET, "dataset_revision": dataset_revision,
                                "ocr_gate": gate.to_value(), "item_types": types.iter().collect::<std::collections::BTreeSet<_>>(),
                                "min_ocr_confidence": min_ocr_confidence, "bucket_key": "bl_hmd_title:<folded newspaper title>"});
            let mut run = Run::start_sampled(&out, "bl-hmd", hmd::COLLECTION, params, max_candidates, buckets)?;
            select.apply(&mut run)?;
            lexicon_input(&mut run, &lexicon, &lex)?;
            let o = hmd::Options { inputs: &inputs, seed: &seed, fraction: sample_fraction, revision: &dataset_revision, lexicon: &lex, gate, item_types: &types, min_ocr_confidence };
            let extra = hmd::adapt(&o, &mut run)?;
            print(&run.finish(extra)?);
        }
        Command::Cap { zips_dir, out, max_candidates, buckets, select } => {
            let mut zips: Vec<PathBuf> = std::fs::read_dir(&zips_dir)?.filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.extension().is_some_and(|x| x == "zip")).collect();
            zips.sort();
            ensure!(!zips.is_empty(), "no zips in {}", zips_dir.display());
            let params = json!({"zips_dir": zips_dir, "zips": zips.len(), "bucket_key": "cap_judge:j<jurisdiction id>:<author surname>"});
            let mut run = Run::start_sampled(&out, "caselaw-access-project", cap::COLLECTION, params, max_candidates, buckets)?;
            select.apply(&mut run)?;
            let extra = cap::adapt(&zips, &mut run)?;
            run.input(json!({"role": "static_case_law_volume_zips", "dir": zips_dir, "files": zips.len()}));
            print(&run.finish(extra)?);
        }
        Command::Courtlistener { opinions, clusters, opinions_sha256, clusters_sha256, snapshot, out, max_candidates, buckets, select } => {
            let params = json!({"snapshot": snapshot, "bucket_key": "courtlistener_person:<author_id>", "text_fields": courtlistener::HTML_FIELDS});
            let mut run = Run::start_sampled(&out, "courtlistener", courtlistener::COLLECTION, params, max_candidates, buckets)?;
            select.apply(&mut run)?;
            let mut cr = BufReader::with_capacity(1 << 20, io::HashingReader::new(io::open(&clusters)?));
            let (cl, bad) = courtlistener::load_clusters(&mut cr)?;
            stream_input(&mut run, "clusters_csv", &clusters, cr.into_inner());
            run.input(json!({"role": "clusters_compressed_download", "sha256": clusters_sha256, "clusters": cl.len(), "unparsed_rows": bad}));
            let mut r = BufReader::with_capacity(1 << 20, io::HashingReader::new(io::open(&opinions)?));
            let extra = courtlistener::adapt(&mut r, &cl, &mut run, &out)?;
            let partial = run.full();
            stream_input(&mut run, "opinions_csv", &opinions, r.into_inner());
            run.input(json!({"role": "opinions_compressed_download", "sha256": opinions_sha256}));
            let mut extra = extra;
            extra["stopped_at_max_candidates"] = json!(partial);
            print(&run.finish(extra)?);
        }
        Command::GovinfoHearings { htm_dir, out, max_candidates, buckets, select } => {
            let files = htm_files(&htm_dir, "CHRG-")?;
            let params = json!({"htm_dir": htm_dir, "files": files.len(), "bucket_key": "govinfo_chrg:<package>"});
            let mut run = Run::start_sampled(&out, "govinfo-hearings", hearings::COLLECTION, params, max_candidates, buckets)?;
            select.apply(&mut run)?;
            run.input(json!({"role": "govinfo_chrg_htm", "dir": htm_dir, "files": files.len()}));
            let extra = hearings::adapt(&files, &mut run)?;
            print(&run.finish(extra)?);
        }
        Command::GovinfoCpd { htm_dir, out, max_candidates, buckets, select } => {
            let files = htm_files(&htm_dir, "DCPD-")?;
            let params = json!({"htm_dir": htm_dir, "files": files.len(), "bucket_key": "us_president:<administration name key>"});
            let mut run = Run::start_sampled(&out, "govinfo-cpd", cpd::COLLECTION, params, max_candidates, buckets)?;
            select.apply(&mut run)?;
            run.input(json!({"role": "govinfo_dcpd_htm", "dir": htm_dir, "files": files.len()}));
            let extra = cpd::adapt(&files, &mut run)?;
            print(&run.finish(extra)?);
        }
        Command::PdfText { input, out } => {
            let v = fomc::pdf_text(&input, &out)?;
            let files = v["files"].as_array().cloned().unwrap_or_default();
            let ok = files.iter().filter(|f| f["status"] == "ok").count();
            print(&json!({"files": files.len(), "ok": ok}));
        }
        Command::Fomc { text_dir, out, max_candidates, buckets, select } => {
            let params = json!({"text_dir": text_dir, "bucket_key": "fomc_speaker:<surname>"});
            let mut run = Run::start_sampled(&out, "fomc-transcripts", fomc::COLLECTION, params, max_candidates, buckets)?;
            select.apply(&mut run)?;
            let rep_path = text_dir.join("pdf-text-report.json");
            if rep_path.exists() {
                let (sha, bytes) = io::file_sha(&rep_path)?;
                run.input(json!({"role": "pdf_text_report", "path": rep_path, "sha256": sha, "bytes": bytes}));
            }
            let extra = fomc::adapt(&text_dir, &mut run)?;
            print(&run.finish(extra)?);
        }
        Command::SelectCandidates { source, out, per_group, group_field, blocked_case_keys } => {
            let o = cand_select::Options { per_group, group_field: &group_field, blocked_case_keys: blocked_case_keys.as_deref() };
            print(&cand_select::run(&source, &out, &o)?);
        }
        Command::Reviews { source, inputs, seed, target_total, quota_floor, max_per_user, threads, terms_evidence, out, buckets } => {
            let src = match source.as_str() {
                "amazon" => reviews::Source::Amazon,
                other => anyhow::bail!("unknown review source {other}"),
            };
            let sizes: Vec<u64> = inputs.iter().map(|p| std::fs::metadata(p).map(|m| m.len())).collect::<std::io::Result<_>>()?;
            let quotas = if inputs.len() == 1 { vec![target_total] } else { reviews::sqrt_quotas(&sizes, target_total, quota_floor) };
            let (adapter, collection) = match src {
                reviews::Source::Amazon => ("amazon-reviews-2023", reviews::AMAZON_COLLECTION),
            };
            let params = json!({"source": source, "seed": seed, "target_total": target_total, "quota_floor": quota_floor, "max_per_user": max_per_user,
                                "threads": threads, "quota_rule": "round(target_total * sqrt(bytes_i) / sum_j sqrt(bytes_j)), at least quota_floor; a single input takes target_total",
                                "quotas": inputs.iter().zip(&quotas).map(|(p, q)| json!({"file": p.file_name().and_then(|n| n.to_str()), "bytes": std::fs::metadata(p).map(|m| m.len()).ok(), "quota": q})).collect::<Vec<_>>(),
                                "bucket_key": "<source>_user:<opaque reviewer hash>"});
            let mut run = Run::start_sampled(&out, adapter, collection, params, None, buckets)?;
            let o = reviews::Options { source: src, inputs: &inputs, seed: &seed, quotas, max_per_user, buckets, threads, terms_evidence: &terms_evidence };
            let extra = reviews::adapt(&o, &mut run)?;
            print(&run.finish(extra)?);
        }
        Command::WebisTldr { zip, zip_sha256, out, max_candidates, buckets, select } => {
            let params = json!({"zip": zip, "bucket_key": "reddit_author:<opaque>", "admission_basis": forum::ADMISSION_BASIS});
            let mut run = Run::start_sampled(&out, "webis-tldr-17", forum::REDDIT_COLLECTION, params, max_candidates, buckets)?;
            select.apply(&mut run)?;
            run.input(json!({"role": "webis_tldr_17_zip", "path": zip, "sha256": zip_sha256}));
            let extra = forum::webis(&zip, zip_sha256.as_deref(), &mut run)?;
            print(&run.finish(extra)?);
        }
        Command::UbuntuIrc { inputs, dataset_revision, out, max_candidates, buckets, select } => {
            let params = json!({"shards": inputs, "dataset": "common-pile/ubuntu_irc", "dataset_revision": dataset_revision, "bucket_key": "ubuntu_irc_speaker:<opaque channel+nick>", "admission_basis": forum::ADMISSION_BASIS});
            let mut run = Run::start_sampled(&out, "ubuntu-irc", forum::IRC_COLLECTION, params, max_candidates, buckets)?;
            select.apply(&mut run)?;
            let extra = forum::ubuntu_irc(&inputs, &dataset_revision, &mut run)?;
            run.input(json!({"role": "common_pile_ubuntu_irc_shards", "files": extra["files"]}));
            print(&run.finish(extra)?);
        }
        Command::Wikiconv { inputs, out, max_candidates, buckets, select } => {
            let params = json!({"corpora": inputs, "bucket_key": "wikipedia_talk_user:<opaque>", "admission_basis": forum::ADMISSION_BASIS});
            let mut run = Run::start_sampled(&out, "wikiconv", forum::WIKI_TALK_COLLECTION, params, max_candidates, buckets)?;
            select.apply(&mut run)?;
            let extra = forum::wikiconv(&inputs, &mut run)?;
            run.input(json!({"role": "convokit_wikiconv_english_corpora", "files": extra["files"]}));
            print(&run.finish(extra)?);
        }
        Command::CapContributors { input, out, max_per_contributor, seed, keep_per_mille } => {
            print(&forum::cap_and_sample(&input, &out, max_per_contributor, &seed, keep_per_mille)?);
        }
        Command::WikipediaIndexReserve { buckets, out } => {
            let r = std::io::BufReader::with_capacity(1 << 20, std::io::stdin().lock());
            print(&grammar_a_pools::splits::wikipedia_index_reserve(r, buckets.0, buckets.1, &out)?);
        }
        Command::ReserveSplits { config } => {
            let v = grammar_a_pools::splits::run(&config)?;
            print(&json!({"evaluation": v["evaluation"], "train_side": v["train_side"], "checks": v["checks"], "shortfalls": v["shortfalls"], "notes": v["notes"]}));
        }
        Command::ExtendSplits { config } => {
            let v = grammar_a_pools::splits_v3::run(&config)?;
            print(&json!({"new_domain_evaluation": v["new_domain_evaluation"], "train_side": v["train_side"], "yields": v["yields"],
                          "evaluation_universe": v["evaluation_universe"], "checks": v["checks"], "shortfalls": v["shortfalls"], "notes": v["notes"]}));
        }
        Command::ExtendSplitsV4 { config } => {
            let v = grammar_a_pools::splits_v4::run(&config)?;
            print(&json!({"new_domain_evaluation": v["new_domain_evaluation"], "train_side": v["train_side"], "sources": v["sources"],
                          "evaluation_universe": v["evaluation_universe"], "diagnostic_slices": v["diagnostic_slices"], "near_window_check": v["near_window_check"],
                          "checks": v["checks"], "shortfalls": v["shortfalls"], "notes": v["notes"]}));
        }
        Command::FinewebBlogs { inputs, psl: psl_path, min_language_score, exclude_domains, dataset_revision, inherit_scan, out, max_candidates, buckets, select } => {
            let list = std::fs::read_to_string(&psl_path)?;
            let p = psl::Psl::parse(&list);
            let excluded: std::collections::HashSet<String> = std::fs::read_to_string(&exclude_domains)?.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')).map(str::to_lowercase).collect();
            let mut inherit = std::collections::HashMap::new();
            let mut inherit_inputs = Vec::new();
            for dir in &inherit_scan {
                let f = dir.join("reserved-metadata.jsonl");
                let (sha, bytes) = io::file_sha(&f)?;
                let (m, rows) = grammar_a_pools::fineweb_blogs::inheritance(&p, io::jsonl_rows(&f)?)?;
                inherit_inputs.push(json!({"role": "inheritance_scan", "path": f, "sha256": sha, "bytes": bytes, "rows": rows, "hosted_blog_keys": m.len()}));
                inherit.extend(m);
            }
            let params = json!({"selection": "every row whose URL is a blog page (blogs::classify_url); no row sample", "min_language_score": min_language_score,
                                "dataset": "HuggingFaceFW/fineweb", "dataset_revision": dataset_revision,
                                "bucket_key": "blog group: fineweb_site:<registered domain> for non-hosted blogs, fineweb_blog:<blog id> for hosted-platform blogs (inherited site key when in the scan)"});
            let mut run = Run::start_sampled(&out, "fineweb-blogs", grammar_a_pools::fineweb_blogs::COLLECTION, params, max_candidates, buckets)?;
            select.apply(&mut run)?;
            let (ps, pb) = io::file_sha(&psl_path)?;
            run.input(json!({"role": "public_suffix_list", "path": psl_path, "sha256": ps, "bytes": pb, "rules": p.rule_count}));
            let (es, eb) = io::file_sha(&exclude_domains)?;
            run.input(json!({"role": "excluded_registered_domains", "path": exclude_domains, "sha256": es, "bytes": eb, "domains": excluded.len()}));
            for v in inherit_inputs {
                run.input(v);
            }
            let o = grammar_a_pools::fineweb_blogs::Options { inputs: &inputs, psl: &p, min_language_score, excluded_domains: &excluded, dataset_revision: &dataset_revision,
                                                              inherit: &inherit, first_person: grammar_a_pools::blogs::FIRST_PERSON_RULE };
            let extra = grammar_a_pools::fineweb_blogs::adapt(&o, &mut run)?;
            print(&run.finish(extra)?);
        }
        Command::PlosBlogs { posts_root, authors_root, blogs, archived, out, max_candidates } => {
            let params = json!({"posts_root": posts_root, "authors_root": authors_root, "blogs": blogs, "archived": archived,
                                "bucket_key": "plos_blog_writer:<folded display name> (else plos_blog_account:<blog>/<id>)"});
            let mut run = Run::start(&out, "plos-blogs", grammar_a_pools::plos_blogs::COLLECTION, params, max_candidates)?;
            let o = grammar_a_pools::plos_blogs::Options { posts_root: &posts_root, authors_root: authors_root.as_deref(), blogs: &blogs, archived: &archived };
            let extra = grammar_a_pools::plos_blogs::adapt(&o, &mut run)?;
            print(&run.finish(extra)?);
        }
        Command::BlogSplits { config } => {
            let v = grammar_a_pools::blog_splits::run(&config)?;
            print(&json!({"per_domain": v["per_domain"], "held_reasons": v["held_reasons"], "reservation": v["reservation"]}));
        }
        Command::BlogRekey { config } => {
            let v = grammar_a_pools::blog_rekey::run(&config)?;
            print(&json!({"rekey": v["rekey"], "train_side": v["train_side"], "held_reasons": v["held_reasons"], "reservation": v["reservation"]}));
        }
        Command::Bucket => {
            use std::io::{BufRead, Write};
            let mut out = std::io::stdout().lock();
            for line in std::io::stdin().lock().lines() {
                let k = line?;
                if !k.is_empty() {
                    writeln!(out, "{k}\t{}", grammar_a_pools::adapter::bucket(&k))?;
                }
            }
        }
        Command::IndexPrior { registry, root, out, probes, threads, spill_keys, spill_dir } => {
            let probe = if probes.is_empty() { None } else { Some(protected::Probe::from_candidate_dirs(&probes)?) };
            let v = protected::build_bounded(&registry, &root, &out, probe, threads, spill_keys, spill_dir.as_deref())?;
            print(&json!({"files_scanned": v["files_scanned"], "strings_indexed": v["strings_indexed"],
                          "key_files": v["key_files"], "families": v["families"].as_array().map(Vec::len),
                          "pg_ids": v["pg_ids"].as_array().map(Vec::len), "title_aliases": v["title_aliases"].as_array().map(Vec::len),
                          "entries_without_files": v["entries_without_files"]}));
        }
        Command::SelectEval { split_dir, config, dedup, adapters, near_reference, near_collection, near_window, reservation, seal, seal_sha256, raid_s1, raid_s2, raid_s3, near_exclude_train, near_exclude_collection, near_exclude_window, out } => {
            let o = grammar_a_pools::eval_select::Options {
                split_dir: &split_dir, config: &config, dedup: &dedup, adapters: &adapters, near_reference: &near_reference,
                near_collection: &near_collection, near_window, out: &out,
                reservation: reservation.as_deref(), seal: seal.as_deref(), seal_sha256: seal_sha256.as_deref(),
                raid_s1: raid_s1.as_deref(), raid_s2: raid_s2.as_deref(), raid_s3: raid_s3.as_deref(),
                near_exclude_train: near_exclude_train.as_deref(), near_exclude_collection: &near_exclude_collection, near_exclude_window,
            };
            let v = grammar_a_pools::eval_select::run(&o)?;
            print(&json!({"split": v["split"], "documents": v["documents"], "split_target": v["split_target"], "protocol_minimum": v["protocol_minimum"],
                          "meets_minimum": v["meets_minimum"], "backfill_documents": v["backfill_documents"], "shortfall_before_backfill": v["shortfall_before_backfill"],
                          "per_domain_documents": v["per_domain"].as_object().map(|m| m.iter().map(|(k, x)| (k.clone(), x["documents"].clone())).collect::<serde_json::Map<_, _>>())}));
        }
        Command::RaidS1 { record, reservations, out } => {
            let rec = raid_screen::Record::load(&record)?;
            let named = reservations.iter().map(|r| raid_screen::parse_named(r)).collect::<Result<Vec<_>>>()?;
            let v = raid_screen::s1(&rec, &named, &out)?;
            print(&json!({"rule": v["rule"], "skips_rows": v["skips_rows"], "matched_groups": v["matched_groups"]}));
        }
        Command::RaidS2 { record, reservation, webis_zip, webis_zip_sha256, seal, seal_sha256, out } => {
            let rec = raid_screen::Record::load(&record)?;
            let named = raid_screen::parse_named(&reservation)?;
            let res = raid_screen::Reservation::open(&named.0, &named.1)?;
            let seal_v = if res.split == "test" { raid_screen::require_seal(seal.as_deref(), seal_sha256.as_deref())? } else { json!(null) };
            let v = raid_screen::s2(&rec, &named, &webis_zip, &webis_zip_sha256, &seal_v, &out)?;
            print(&json!({"rule": v["rule"], "reserved_author_groups": v["reserved_author_groups"], "matched_groups": v["matched_groups"], "groups_without_posts": v["groups_without_posts"]}));
        }
        Command::RaidS3 { record, adapters, split, seal, seal_sha256, out } => {
            let rec = raid_screen::Record::load(&record)?;
            let seal_v = if split == "test" { raid_screen::require_seal(seal.as_deref(), seal_sha256.as_deref())? } else { json!(null) };
            let v = raid_screen::s3(&rec, &adapters, &seal_v, &out)?;
            print(&json!({"rule": v["rule"], "candidates_checked": v["candidates_checked"], "hit_rows": v["hit_rows"], "families_hit": v["families_hit"]}));
        }
        Command::MergeEval { parts, config, seal, seal_sha256, out } => {
            let v = grammar_a_pools::eval_select::merge(&parts, &config, seal.as_deref(), seal_sha256.as_deref(), &out)?;
            print(&json!({"split": v["split"], "documents": v["documents"], "split_target": v["split_target"], "protocol_minimum": v["protocol_minimum"], "meets_minimum": v["meets_minimum"]}));
        }
        Command::RaidAdapt { inputs, out, attack_per_mille, max_rows } => {
            let mut parsed = Vec::new();
            for i in &inputs {
                let parts: Vec<&str> = i.rsplitn(3, '=').collect();
                ensure!(parts.len() == 3 && parts[0].len() == 64, "--input must be PATH=SPLIT=SHA256");
                parsed.push((PathBuf::from(parts[2]), parts[1].to_owned(), parts[0].to_owned()));
            }
            ensure!(attack_per_mille <= 1000, "attack_per_mille must be at most 1000");
            print(&grammar_a_pools::raid::adapt(&grammar_a_pools::raid::AdaptOptions { inputs: &parsed, out: &out, attack_per_mille, max_rows })?);
        }
        Command::RaidAdmit { candidates, index, out } => {
            let v = grammar_a_pools::raid::admit(&grammar_a_pools::raid::AdmitOptions { candidates: &candidates, index: &index, out: &out })?;
            let s = grammar_a_pools::raid::finalize(&out, &candidates, &v, &index)?;
            print(&json!({"admitted": v["admitted"], "excluded": v["exclusion_reasons"], "source_groups": v["source_groups"],
                          "admitted_source_groups": v["admitted_source_groups"], "prior_conflict_groups": v["source_groups_excluded_for_prior_conflict"],
                          "counts": s["counts"]["by_axis"]}));
        }
        Command::Dedup { index, sources, out, min_shared, near_window, near_collections, residue_collections, exclude_aliases, prior_hits, reserved_metadata } => {
            let (hits, hits_file) = match &prior_hits {
                Some(p) => (Some(scan::load_hits(p)?), Some((p.clone(), io::file_sha(p)?.0))),
                None => (None, None),
            };
            let (reserved_families, reserved_files) = dedup::load_reserved_families(&reserved_metadata)?;
            let opt = dedup::Options { near_window, near_collections, residue_collections, exclude_aliases, prior_hits: hits, prior_hits_file: hits_file, reserved_families, reserved_files };
            print(&dedup::run_with(&index, &sources, &out, min_shared, &opt)?);
        }
    }
    Ok(())
}
fn load_lexicon(path: &Path) -> Result<ocr::Lexicon> {
    let lex = ocr::Lexicon::parse(&std::fs::read_to_string(path)?);
    ensure!(lex.len() >= 1000, "lexicon {} has only {} words", path.display(), lex.len());
    Ok(lex)
}

fn lexicon_input(run: &mut Run, path: &Path, lex: &ocr::Lexicon) -> Result<()> {
    let (_, bytes) = io::file_sha(path)?;
    run.input(json!({"role": "ocr_lexicon", "path": path, "sha256": lex.sha256, "bytes": bytes, "words": lex.len()}));
    Ok(())
}
