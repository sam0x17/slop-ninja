# grammar-cc-human

Pilot sampler for a Common Crawl (CC) human slice for mainnet A cohorts. It
downloads a bounded, fixed-seed sample of CC WET files from
`https://data.commoncrawl.org`, screens every record, picks one excerpt per
page, assigns a cohort domain, dedups against corpus-v1 and writes yield
tables. It makes no model, detector or hosted-LLM calls. Texts and every
intermediate file stay in ignored storage (`data/task-roots/cc-human/`). CC
text is `restricted_no_redistribution`, the same class as FineWeb: it is used
privately for evaluation and never published.

Binary: `slop_ninja_cc_human --root <dir> <step>`. Each step resumes: a file
whose completion marker exists is skipped.

| Step | What it does | Main outputs under `--root` |
| --- | --- | --- |
| `paths` | Downloads `wet.paths.gz` for every frame crawl (`per-crawl.tsv`). | `meta/paths/` |
| `select` | Ranks each crawl's WET paths with `rank(seed, "cc_human_wet_v1", path)` and keeps the lowest `--per-crawl`. CC-MAIN-2022-49 files must carry a segment stamp before 2022-11-30. | `selection.tsv`, `selection.json` |
| `fetch` | Polite journaled download. | `raw/`, `journal/downloads.jsonl` |
| `pages` | Pass 1: record parse, gates and page screens. | `pages/`, `rejects/`, `funnel/*.pages.json` |
| `candidates` | Pass 2: site templates, keyed passage choice and excerpt-level English. | `candidates/`, `funnel/*.candidates.json` |
| `dedup` | Pass 3: corpus-v1 dedup, host closure and within-pool dedup. | `dedup/`, `pool/admitted.jsonl.zst` |
| `pointers` | Re-fetches sampled admitted records by HTTP Range and checks every provenance field. | `report/pointers.json` |
| `report` | Yield tables, rejection reasons and the yearly projection. | `report/*.tsv`, `report/summary.json` |
| `bench` | Times each page screen on one WET file. | stdout |

## Downloads

- Only `https://data.commoncrawl.org`. There are at most 4 connections
  (`--connections`, default 2), and a HEAD request comes before each GET.
- **Byte budget.** A file starts only if the bytes already received, plus the
  bytes reserved by running transfers, plus its own size, stay within
  `--budget-bytes`. Bytes of failed attempts count too.
- **Disk headroom.** A file starts only if free space minus its size stays at
  or above `--min-free-bytes` (default 400 GB).
- **Throttling.** A 429 or 503 backs off for 15 s, 60 s and then 180 s. Three
  consecutive throttled responses stop every transfer.
- **Journal.** Every attempt is a fsynced journal line. A file is renamed from
  `.part` only after its length matches `Content-Length`. Range reads are
  journaled too.

## Record provenance

Every page and candidate row keeps:
- the crawl and WET path, and the SHA-256 of the downloaded file;
- the record's gzip member offset and length;
- `WARC-Record-ID`, `WARC-Refers-To` (the WARC response record),
  `WARC-Target-URI` and `WARC-Date` (the fetch time);
- `WARC-Block-Digest` (checked against the block) and the CLD2 header when
  present;
- the SHA-256 of the page text;
- the excerpt's half-open span over the page's non-empty lines;
- the excerpt's exact, normalised and folded SHA-256.

CC writes one gzip member per record, so (WET path, offset, length) is a
stable pointer after the raw file is deleted. `pointers` proves this: it
fetches the bytes again, decodes them and compares the record id, date, URL,
block digest, page digest and the rebuilt excerpt.

## Screens

In order, each counted by domain in the funnel:

1. **date_gate:** a conversion record whose block digest matches, with
   `WARC-Date` before 2022-11-30.
2. **url_gate:** an http(s) URL with a registered domain (the FineWeb Public
   Suffix List copy). The domain must not be on any exclusion list:
   - FineWeb's `excluded-registered-domains-v1`;
   - Reddit, Hacker News, Yelp, Goodreads and Amazon;
   - sites of corpus-v1 sources (CourtListener, law.justia.com);
   - AO3 (fiction communities that object to AI use are excluded);
   - adult hosts: the in-house token list, label substrings (`porn`, `xxx`,
     `phonesex` and others) and listed adult story archives;
   - essay-mill hosts (any host label containing `essay`).

   Screens v2 add path screens at the same gate: legal boilerplate pages
   (privacy, terms, EULA, cookie-policy and disclaimer pages), HTML pages
   served from upload folders, and chapter pages on translated web-novel
   and manga aggregators.
3. **length_gate:** 1,000 to 1,000,000 characters.
4. **gopher_quality** and **gopher_repetition**: Rae et al. 2021, as in
   datatrove. WET pages have no blank-line paragraphs, so the paragraph rules
   are the line rules.
5. **machine_text:** versioned marker lists:
   - machine translation and translated web-novel strings;
   - automated news and data text;
   - AI writing and spinner tools;
   - ASR and caption markers;
   - spintax, needing two groups and skipped on technical pages;
   - keyword stuffing: a non-stopword unigram above 8% of tokens.
6. **rights_text:** a non-commercial Creative Commons statement.
7. **excerpt:** each WET line is a paragraph, classified by the FineWeb
   adapter's `classify`. That covers list markers, boilerplate terms and `@`,
   site templates counted over every pilot page of the registered domain,
   markup and quotation breaks.
   - Every valid run (150..=450 words, stopping at the first line boundary at
     or after 220 words) is a candidate.
   - The run with the lowest `rank(seed, "cc_human_passage_v1",
     record_id:start)` is kept, so a lookup index must cover every valid
     run.
8. **english:** on the excerpt, all three rules must pass:
   - `english_screen`;
   - `whatlang` English with `is_reliable`;
   - the first CLD2 language is `eng` when the header is present
     (CC-MAIN-2020-24 and later).

   The final domain is then assigned with the excerpt (below), and the
   **excerpt_screens** stage rejects sexually explicit excerpts (three or
   more distinct explicit terms).
9. **corpus_v1_dedup:** read-only. Every corpus-v1 text is read from its pack
   and digest-checked. A candidate is excluded when any corpus text matches it
   by:
   - exact, normalised or folded hash;
   - a shared folded 50-word window;
   - a shared folded 12-word window (every window, no sampling);
   - MinHash over 5-word shingles (128 permutations, 16 × 8 LSH), verified at
     Jaccard ≥ 0.5.
10. **host_closure:** registered domains of corpus-v1 FineWeb records in an
    evaluation role exclude the candidate. Train-only domains are flagged.
11. **pool_dedup:** in keyed rank order, the same whole-text, 12-word and
    MinHash screens run against earlier kept candidates. Excluded pairs join
    their groups into one split component.

**Domains.** Rules v3 (`rules/domain-rules-pilot-v3.json`) are the
default; v1 stays for reproducing the pilot-v1 pool and v2 for the
precision-v2 frame (the twin extractor keeps v2, the rules twins-v1 used).
- Tier 1 decides first: hosts (optionally narrowed to a path prefix), host
  patterns and strong path patterns, in priority order.
- Then content cues: a domain needs `min_cues` distinct cues on the page and
  `min_excerpt_cues` of them in the chosen excerpt; legal and speech also
  need one of their anchors (case citations and party words; speaker labels,
  applause and parliamentary formulas) in the excerpt.
- Then tier 2: weak hosts, weak host patterns and weak paths fire only when
  their domain has `weak_min_cues` cues on the page and
  `weak_min_excerpt_cues` in the excerpt.
- Everything else is `general_web`. Pass 1 assigns a provisional page-only
  domain; pass 2 assigns the final one with the excerpt and keeps the
  pass-1 result as `page_domain` (candidate schema v3).
- The v2 changes come from the precision review below: day-dated blog
  permalinks moved from news to essays, news and review section paths and
  how-to slugs became gated, chapter and `/s/N/N` paths left fiction's
  strong rules, fiction hosts narrowed to story pages, retailer hosts left
  reviews, speech paths match whole segments, and `^mail.` left email.
- The v3 changes come from the fresh v2 draw:
  - technical and reviews are never assigned by content cues alone
    (`content_assign: false`); only their URL rules assign them, and their
    cues still gate their weak rules;
  - newspaper opinion, comment, editorial, column and letters sections
    (`sections`) move from news to essays;
  - an excerpt that sits in a blog's or news site's comment section
    (`comment_sections`: a comment meta line just before it, or a comment
    timestamp at its start, under a comment-count header or after at least
    two meta lines) moves from essays or news to forum;
  - one missing forum host was added.
- **Groups.** The group is the registered domain. Multi-author platforms
  group by user subdomain, or by host plus first path segment.
- **Secrecy.** These are the pilot drafts. Pilot documents never enter
  cohorts, so the rules and the pilot seed may be public. Production loads
  private rules (`--rules`) and a secret seed.

## Running on build-host

Build with one Cargo job, from the root of the checkout. A user unit starts
in the home directory with the systemd user manager's `PATH`, which lacks
`~/.cargo/bin`. The command therefore passes the calling shell's `PATH`, so
`cargo` and `rustc` resolve as they do in that shell, and every other path it
uses is absolute. The shared Cargo lock lives in the main checkout, which
`--git-common-dir` finds from any worktree; the target directory stays inside
the checkout being built:
```
main=$(dirname "$(git rev-parse --path-format=absolute --git-common-dir)")
systemd-run --user --slice=slop-a-comp.slice -p MemoryMax=8G -p MemorySwapMax=0 \
  -p RuntimeMaxSec=1800 --working-directory="$PWD/grammar" \
  --setenv=PATH="$PATH" \
  --setenv=CARGO_TARGET_DIR="$PWD/data/task-roots/cc-human/target" \
  flock "$main/data/locks/cargo.lock" cargo build --release -j 4 -p grammar-cc-human
```

Run each step as a unit in the same slice:
- `MemoryMax=6G`, `MemorySwapMax=0` and an absolute `--working-directory`;
- a copied binary under `pilot-v1/bin/`, and an `.exit` file.

`--psl` and `--fineweb-excluded` take the FineWeb pool's
`raw/meta/public_suffix_list.dat` and `excluded-registered-domains-v1.txt`.

## Pilot v1 results (2026-10-30)

These are counts only. Texts and the full tables are in
`data/task-roots/cc-human/pilot-v1/`. Its candidate and pool rows carry
candidate schema v1. Schema v2 changes only the `rights_basis` label, and
v3 adds the excerpt-based final domain with `page_domain`.

**Sample.** 2 WET files per crawl over the 89 crawls CC-MAIN-2013-20..2022-49,
178 files, 24.455 GB.
- **Records.** 8,245,186, with no digest mismatches and no parse errors.
- **Pointer check.** 50 of 50 Range re-fetches matched.
- **Admitted.** 416,492 documents, or 17,031 per GB and 3.63M excerpt words
  per GB.
- **Funnel per GB.** 337k records, 108k pages after the page screens, 21.6k
  with a valid run, 18.6k English, 17.0k after dedup and host closure.
- **corpus-v1** excluded 2.0% of English candidates, every one by a shared
  12-word window.
- **Host closure** excluded 3.9%. Most of it is wordpress.com, which
  corpus-v1 groups as one site.

| Domain | Admitted per GB | WET GB per year for 66.7k |
| --- | ---: | ---: |
| news | 3,601 | 19 |
| essays | 2,816 | 24 |
| forum | 619 | 108 |
| reviews | 414 | 161 |
| technical | 336 | 199 |
| legal | 133 | 502 |
| email | 66 | 1,008 |
| fiction | 39 | 1,702 |
| speech | 37 | 1,826 |
| general_web | 8,970 | 7 |

**Measured cost:**
- Pass 1 takes 164 single-thread seconds per GB, and pass 2 takes 28.
- Dedup against corpus-v1 takes about 21 core-minutes a run.
- Downloads ran at 39 MB/s on two connections.

**600k documents a year:**
- At the observed mix, that is 35 GB of WET and about 20 core-hours a year.
- With all nine domains filled equally from random WET, speech sets the
  volume: 1.83 TB a year, 0.26% of the frame's WET bytes, and about 116
  core-hours.
- Data access costs $0. Host time is about $1–6 a year at $0.05 per
  vCPU-hour.

Domain precision is measured in the review below.

## Contamination and era measurements (`slop_ninja_cc_human_measure`)

`slop_ninja_cc_human_measure` measures contamination and crawl-era drift on
the admitted pool. It makes no network, model or detector calls.

| Step | What it does |
| --- | --- |
| `sample` | Keyed draw per crawl year × domain cell: one document per group first, then fill, 50 per cell; the first 20 of each cell form the nested Pangram subsample. Writes request shards for the A runtime and the bulk-collector inputs (split `cc_pilot`). |
| `report` | Joins A predictions and a Pangram store to the sample. Flag rates by year, domain, era bin and cell with Wilson intervals, pool-weighted margins, early/late and E2–E5/E0–E1 contrasts (Newcombe), a Mantel trend test stratified by domain, group-bootstrap drift of A's continuous human scores, A/Pangram agreement, the Tier C Test human comparison, and a 50-document review sample of flagged documents. |
| `era-probe` | Logistic regression on hashed word unigrams and bigrams (2^20 buckets), early (2013–2016) against late (2020–2022) crawls, balanced per domain and split by group; raw and with digit tokens masked. It also scores the A sample and the Tier C Test rows. |

Pangram goes through `examples/a_comp_pangram_bulk.rs` with split `cc_pilot`:
bulk API only, comparator evidence only.

### Results (2026-10-30)

- **Sample.** 4,954 documents (50 per cell; three cells had fewer), 2,000 in the
  Pangram subsample.
- **A.** The released seed-17 model (calibrated Rust FP64 runtime, CPU) at the
  CAL 1% operating point (involvement ≥ 0.9090909090909091). All 4,954 scored.
  - Flagged 155/4,954 (3.13%, Wilson 2.68–3.65%); pool-weighted 2.20%.
  - No rise with crawl year. The within-domain trend is +0.016 points a year
    (z = 0.19). E2–E5 minus E0–E1 is +0.47 points (−0.51 to +1.52).
  - By domain the flag rate ranges from 1.2% (forum) to 3.4% (legal), except
    fiction at 11.6% (55/475), which is flagged in every year from 2013 on.
  - No drift in the continuous human scores: involvement changes by +0.0001 a
    year (group bootstrap −0.0022 to +0.0023), and the domain-balanced AUC of
    2020–2022 against 2013–2016 is 0.496 (0.477–0.516).
  - Comparison: the same model flags 43/3,286 (1.31%) of the Tier C Test
    primary humans, 7/220 (3.2%) of its FineWeb humans and 3/150 (2.0%) of its
    FineWeb blog humans.
- **Pangram 4.1** (partial). 302 of the 2,000 documents were observed before
  collection stopped. Two more failed on an echo mismatch, and 1,696 were not
  submitted. All 302 are labelled Human, 0/302 (upper bound 1.25%).
- **Manual review** of 50 flagged documents (45 flagged by A):
  - 38 of the 45 read as human-written, mostly modern narrative fiction and
    publisher, press-release and marketing copy.
  - 4 are machine-made or machine-processed: one TV caption transcript, one
    synonym-spun page, two scraped spam pages.
  - 1 is a templated SEO page, and 2 are translated web novels.
  - None reads like GPT-2- or GPT-3-era generated prose.
- **Era probe.** Test AUC 0.688 (0.672–0.703), or 0.672 with digit tokens
  masked. The cues are topical (pandemic, election, crypto and gambling
  vocabulary, social platforms) and page furniture. Mean P(late) rises from
  0.43 (2013–2016) to 0.57 (2021–2022). Within Tier C Test domains, the probe
  separates model-only rows from the same domain's humans at AUC 0.72 and
  mixed rows at 0.56. It is uncorrelated with A's involvement within cells
  (r = −0.01).

Limits: detector sensitivity to pre-2022 machine text is not measured. The
per-cell samples are small: with about 250 documents per arm, the E2–E5
excess interval is about ±3 points per domain. V6 and E0 were not run, and
Pangram covers 15% of its subsample.

## Lookup-risk measurement (`slop_ninja_cc_lookup`)

`slop_ninja_cc_lookup` measures how much a sandboxed artifact could gain by
carrying an index of the CC frame. It reads the pilot outputs and the consumed
Tier C Test (statistics only) and writes JSON and TSV under `--out`
(`data/task-roots/cc-human/lookup-v1/out`). It makes no network, model or
detector calls.

| Step | What it does | Main outputs |
| --- | --- | --- |
| `keys` | Rebuilds every admitted page's valid runs and records the keys such an index needs: every run (A), sentences of 6+ folded tokens in the run union (B), winnowed 8-grams (W, window 8), MinHash LSH bands per run (C, 16 x 8), plus the chosen excerpt's query keys. Checks that the chosen run and excerpt SHA-256 are reproduced. | `keys/`, `docs.tsv`, `keys.json` |
| `bloom` | Real Bloom filters over the member half (split by source group) at 0.1%, 1% and 5%: bits per key and per document, empirical false-positive rate on 20M random keys, member recall, non-member hits. | `bloom.json` |
| `background` | Hits of non-indexed text (other-half CC excerpts, Tier C humans, Tier C AI text) on nested indexes of 1/16..1/2 of the pilot, with log-log growth. | `background.json` |
| `nearmatch` | Per Tier C document, overlap with its own human parent under each key type. | `nearmatch.tsv` |
| `lightedit` | The same for synthetic light edits (1..20% of words replaced) of every Tier C human. | `lightedit.tsv` |
| `frame` | Unique admitted documents in the frame from CC's per-crawl counts (URL novelty) and the pilot's per-crawl yields; index size against coverage and false-positive rate; coverage an index buys per byte budget under five sampling designs (greedy over domain x year strata). | `frame.json`, `frame-*.tsv` |
| `gain` | Cross-fitted Brier gain of a miner that applies lookup buckets as a likelihood ratio to the seed-17 scores, by coverage, strategy and policy, under the 40/40/20 mix (or `--mix`), with optional light-edit share and background modes (`none`, `pilot`, `fit`). | `<tag>.json`, `<tag>.tsv` |
| `canary` | Power of a reused private anchor bank to detect the gain, using paired family-level differences between systems without a lookup index on Tier C (sigma) and their between-domain spread (tau). | `canary.json`, `canary.tsv` |

Measured on 2026-10-30 (pilot v1, 416,492 admitted excerpts): every chosen run
and excerpt was reproduced; each page carries 5.53 valid runs, 29.5 B keys,
131.8 W keys and 78.8 C keys on average; the Bloom filters hit their target
false-positive rates to three significant figures and do not compress.

## Domain precision review (`slop_ninja_cc_human_precision`)

`slop_ninja_cc_human_precision` measures how often the domain rules assign
the right cohort domain. It makes no network, model or detector calls.
- `sample --root <pilot> --out <dir>` draws 50 admitted documents per
  cohort domain (general_web excluded). Each domain's draw is allocated to
  crawl years in proportion to its admitted documents (largest remainder)
  and taken by ascending `rank(seed, "cc_domain_precision_v1", id)` in each
  year, so it is self-weighting. The pass-1 page text is joined so changed
  rules can be re-run on what the pilot saw.
  With `--assignments <reassign --per-doc file>` the draw stratifies on the
  re-assigned domains instead (excluded documents are skipped), and every
  pick must reproduce its assignment under `--rules` (default: the current
  rules). `--withhold <ids file>` (repeatable; one id per line or JSONL with
  `id`) removes documents already read; `SAMPLE.json` records the hash of
  the withheld ids and how many fell in each domain's frame.
- `score --sample --labels --sample-summary [--current-rules]` joins hand
  labels (true domain, register, register match, excluded categories) and
  writes domain precision, register match and usable precision with Wilson
  intervals, pool- and mix-weighted totals (normal interval, and Wilson at
  the Kish effective size), the confusion table and the rule-tier and
  rule-id breakdowns. For a draw from an assignment it also splits each
  domain by where the pool had the document (same domain, general_web,
  another domain). With a second rule set it re-runs each sampled document
  and reports where it would go.
- `caps --frame <lookup-v1 frame.json> --root <pilot> --assignments <file>
  [--rules <file>]` gives the density-rule caps of every domain and email
  channel under one assignment (`shares::caps`): count, mean valid runs per
  page, unique documents (the lookup-v1 frame scaled by the count),
  ideal-index bytes at 1e-4 FPR, the share cap (g = 0.44) and the
  whole-stratum cap (g = 1). Caps depend on the rules that made the
  assignment, so the record names them.
- `derive-shares --base <table> --caps <file> --cut <strata> --to essays
  --id <id>` writes a launch share table with the named strata cut to their
  caps (floored to three significant figures) and the freed points given to
  the receiver, and lists any stratum still over its cap. `score --shares
  <id>` weights domains by a table.
- `reassign --root <pilot> [--per-doc <file>] [--detail email]` re-runs the
  current rules and screens over every admitted page. It counts the domain
  transitions, the new rule ids per destination, rule-to-rule transitions
  for the detail domains, and the email channel split (press release: a wire
  host or a press/news-release path; list archive: any other email URL rule;
  cues only) before and after. `--pool-rules` names the rules the pool was
  built with (default v1) so its rule ids resolve; `rule_sources` maps each
  id to its host or pattern. `--per-doc` writes every document's
  re-assignment (no text) for a fresh draw.

Reviewed documents are exposed and never enter cohorts. Labels, texts and
the reports stay in ignored storage (`data/task-roots/cc-human/precision-v1/`
and `precision-v2/`).

**Result (2026-10-30, rules v1, 450 documents):** domain precision was
55% pool-weighted, from 36% (news) and 40% (technical) to 96% (email).
Every domain was below the 90% target except email, whose register
is press releases rather than correspondence. Rules v2 fix the errors that
URL and cue rules can fix; their in-sample precision is optimistic until a
fresh draw is labelled.

**Result (2026-10-31, rules v2 and screens v2, fresh draw of 450):** seed
`cc-human-precision-v2-20261031`, 50 per v2 domain, self-weighting by crawl
year, withholding the 499 documents read earlier. Usable precision (Wilson
95%): forum 86% [74, 93], email 84% [72, 92], speech 82% [69, 90], legal
72% [58, 83], news 64% [50, 76], fiction 62% [48, 74], essays 58% [44, 71],
reviews 58% [44, 71], technical 50% [37, 63]. Pooled by the proposed v2
launch shares it is 62% [56, 69] (domain precision 67% [60, 73]); by the v1
shares 63% [57, 69]. No domain reaches 90%. Documents that v2 moves into
technical from general_web are 4 of 20 usable. Machine text is 13 of 450
(2.9%), as under v1. Under v2 the pilot pool's 1,422 email documents are
1,349 press releases, 72 list-archive pages and 1 cue-only page (v1: 1,351,
118 and 149 of 1,618).

**Result (2026-10-31, rules v3, fresh draw of 450):** seed
`cc-human-precision-v3-20261031`, 50 per v3 domain, self-weighting by crawl
year, withholding the 949 documents read in the earlier reviews. Usable
precision (Wilson 95%): speech 84% [72, 92], email 84% [72, 92], forum 80%
[67, 89], reviews 80% [67, 89], news 76% [63, 86], technical 72% [58, 83],
essays 68% [54, 79], fiction 66% [52, 78], legal 62% [48, 74]. Pooled by
the default share table (`cc_launch_shares_v5`) it is 72% [64, 79] (domain
precision 78% [71, 84]). Comment-section entrants to forum were 14 of 14
domain-correct. Remaining errors: comment threads under technical and review
pages (7), law-firm marketing in legal (5), and general_web pages reached by
URL rules. Machine text fell to 7 of 450 (1.6%). Under v3 the pilot pool
holds technical 2,070 (v2: 8,443), reviews 4,300 (6,429), forum 18,650
(13,569), essays 67,145, news 28,739, and email as under v2.

## Launch share tables (`shares`)

Versioned tables of human-parent shares by domain, with the email split
into press releases and list mail, live in `rules/launch-shares-v*.json`;
`batch --shares <id>` draws parents at one. Caps come from `caps` on a named
assignment, and derived tables record their base, caps and cut.

| Table | Status | news | essays | technical | reviews | forum | legal | speech | fiction | email (press + list) |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| v1 | era-v1 batch, superseded | 27.5 | 27.8 | 12 | 10 | 8.5 | 7.5 | 3.5 | 2.5 | 0.7 (0.65 + 0.05) |
| v2 | proposed, not adopted | 27.5 | 36.0 | 12 | 6.5 | 7.8 | 5.4 | 2.3 | 1.8 | 0.7 (0.65 + 0.05) |
| v3 | four strata cut to rules-v2 caps | 27.5 | 41.05 | 9.38 | 6.37 | 7.8 | 3.11 | 2.3 | 1.8 | 0.688 (0.65 + 0.0384) |
| v4 | the same cut on rules-v3 caps | 27.5 | 50.25 | 2.39 | 4.16 | 7.8 | 3.11 | 2.3 | 1.8 | 0.688 |
| v5 | default: every stratum under its rules-v3 cap | 26.5 | 51.25 | 2.39 | 4.16 | 7.8 | 3.11 | 2.3 | 1.8 | 0.688 |

Rules v3 shrink the technical frame to a quarter and the reviews frame by a
third, so their caps fall to 2.39% and 4.17%; news loses its opinion
sections and comment threads, and its cap falls to 26.6%, under v4's 27.5%.
Forum's cap rises to 14.9%. All caps keep the factor-of-2 frame uncertainty
of lookup-v1.

## Membership twins (`slop_ninja_cc_twins`)

`slop_ninja_cc_twins` builds the twin audit set for the lookup canary.
Twins are pages whose URL carries a pre-cutoff date (`/YYYY/MM/`, before
2022-11) and whose URL Common Crawl first captured after the cutoff. Each
twin is matched by host and URL month to pages that the pre-cutoff frame did
capture. Twins are audit material only and never enter cohorts. The tool
makes no model, detector or hosted-LLM calls.

| Step | What it does |
| --- | --- |
| `index` | Downloads `cluster.idx` for the 89 frame crawls and the post-cutoff crawls CC-MAIN-2023-06 and CC-MAIN-2024-51. |
| `select` | Finds post-cutoff (2023-06) index blocks whose first key and next first key are dated paths of one host. It keeps one block per host (keyed rank), applies the URL and path screens, reads that block and keeps a contiguous run of 300 dated 200 HTML URLs at a keyed start. |
| `plan` | Reports the CDX bytes each sub-range needs over the 91 crawls, without reading them. |
| `cdx --batch B` | For each sub-range in `select/batches/B.json` and every crawl, reads the CDX blocks that can hold its keys and keeps the lines in range. Adjacent blocks go in one request. `--only-crawls` gives a prescreen batch (`prescreen*`), which is never built. |
| `build` | A twin has a 200 HTML capture after the cutoff and no capture of any status of its SURT in any crawl up to and including CC-MAIN-2022-49. Its base key (no query, trailing slash, AMP or print suffix) must also be absent before the cutoff. A frame URL has a 200 HTML capture before 2022-11-30, and its latest one is read. Strata are host × URL month. Twins come from strata that also hold a frame URL, in keyed rank and capped per host. Frame URLs are drawn 1:1 per stratum, and a frame URL recaptured after the cutoff also gives a `frame_post` record. |
| `records` | Reads the WARC response records by HTTP Range. |
| `extract` | Runs one HTML-to-lines extractor on every arm. The page's metadata date must fall within 45 days of the URL month and before the cutoff. Twins and `frame_post` records also need any modification date before the cutoff. Then come the pilot's page screens, site templates over distinct URLs of a registered domain, the pilot's keyed paragraph-run choice and the English rules (without the CLD2 header, for both arms). Finally, a twin that shares a folded 12-word window with any frame-side excerpt leaves the matched set. A request shards are written for every document. |
| `report` | Joins the released A (seed 17) predictions and writes the matched gaps (host bootstrap), the lookup-canary statistic and its power. Models without a lookup index enter through Tier C: each Tier C human gives a logit shift of model m against R0, which is drawn within the document's R0 score bin. |
| `tau` | Measures tau, the offset of models without a lookup index, directly. Each such model scored on the twin set (`--model NAME=PREDICTIONS`) gives `D = u_m - u_0` per document against R0 and the lookup-canary statistic T, with a host-bootstrap SE and 95% interval. The panel gives tau-hat (the 95th percentile of \|T\|), the noise-corrected systematic part and a chi-square 95% upper bound. The canary's power is recomputed with them, and the canary is simulated with the real models as baselines without a lookup index and as lookup attackers. Writes `tau/out/tau.json`. |
| `bank-plan` | Plans a twin bank without network reads: candidate host archives from the `cluster.idx` of both post-cutoff crawls (as in `select`), URL, path, copy-risk and non-English ccTLD screens on the host, one block per registered domain (keyed rank), the whole block as the range, and its CDX reads in all 91 crawls. Ranges above the CDX byte cap are dropped; the rest are ordered by a keyed rank of the registered domain. Writes `plan/PLAN.json`, which freezes every bank parameter (`twins_bank::BankParams`), and `plan/ranges.jsonl.zst`. |
| `bank-run` | Processes the planned ranges in order, one at a time: CDX, picks (the `build` rule with a per-host twin cap and `frame_ratio` frame URLs per picked twin in each stratum), WARC records, then the `extract` path. Each range ends with `ranges/NNNNN/DONE.json`, which holds its host × URL-month counts; `journal/progress.jsonl` gets one line per range and `state/STATUS.json` the running totals. A restarted run skips finished ranges and re-reads only records not yet indexed. The run stops when the matched twin and frame counts reach the targets times (1 + stop margin), then runs `bank-finish`. Exit codes separate the restartable stop (74, throttled) from the ones that need a person (75 rate block, 76 byte budget, 77 disk headroom, 78 plan exhausted). |
| `bank-finish` | Joins the finished ranges, runs the twin text check across the whole bank and writes `extract/` (documents, A request shards, `EXTRACT.json`) in the layout `report` and `tau` read. One range per registered domain makes per-range site templates equal to bank-wide ones. |

`build` and `extract` share their per-range code with the bank
(`twins::build_range`, `twins::extract_dir`, `twins::twin_text_check`), and
both arms of every bank use the pilot rules v2 (`twins::TWIN_RULES_ID`). With
`frame_ratio` 1.0 the pick rule is the twins-v1 1:1 draw.

Range reads share one pacing schedule (at most 5 a second) and back off
exponentially on 429 and 503 responses. A 403 is a rate block: it pauses every
reader for two minutes, and three in a row stop the run. Only one network step
runs at a time.

The report leaves copy-risk hosts out of the matched analysis
(`twins::copy_risk_host`): mirror hosts with an embedded host name,
syndication portals and `archive.` subdomains. The frame may hold their
text under another host. `--keep-copy-risk-hosts` keeps them.

**Result (2026-10-31, twins-v1):** 15.99 GB were read: 11.77 GB of
`cluster.idx`, 3.75 GB of CDX ranges and 0.47 GB of WARC records. In the
sampled post-cutoff archive ranges, 20.6% of dated 200 HTML URLs had never
been captured before the cutoff. After the pilot's rules, the matched set
holds 698 twins and 525 frame excerpts in 146 host-month strata on 28 hosts.
Released A seed 17 shows no difference between the arms: frame-minus-twin
loss -0.0021 [-0.027, 0.023], AUC 0.464. The composition part of tau
(Tier C panel shifts) is about 0; the `tau` result below measures the full
tau. The report, texts and scores stay in
`data/task-roots/cc-human/twins-v1/`.

**Result (2026-10-31, `tau`):** three close models without a lookup index
were scored on the same 1,823 documents: Faux seeds 29 and 41 and a uniform
weight soup of seeds 17 and 29, calibrated on CAL only, which passes the Rust
replay gate. Against R0 they give T = -0.0058 [-0.016, 0.005], -0.0074
[-0.020, 0.006] and -0.0059 [-0.014, 0.001]. Each lies within about 1.5
host-bootstrap SEs (0.004 to 0.007) of zero. At this size the set cannot show
that \|T\| stays within 0.0025. Panel: tau-hat 0.0073, systematic part
0.0035, 95% upper bound 0.018. With these values, a 20k-twin bank flags a
close model's lookup with 80% power at 12%, 19% and 49% frame coverage
(human-document statistic). Different-architecture models were not scored,
because no CPU export of them exists.

## Era gates (`slop_ninja_cc_era`)

`slop_ninja_cc_era` runs the pre-mainnet era gates on a Common
Crawl-parented mock batch: the era probe on parents against topic- and
date-conditioned mirrors (AUC at most 0.60), era-only and format-only
adversaries (each at most 5% of the reference's headroom B0), and B0, σ, δ
and w re-derived on the same batch. Generation is local only (llama-server
under `tools/gpu-lock.sh`); there are no hosted-model, detector or Pangram
calls.

| Step | What it does |
| --- | --- |
| `batch` | Draws parents from the admitted pool at the launch domain shares, crawl years weighted by first-seen unique documents (`lookup-v1` frame), one document per source group (Efraimidis–Spirakis keyed draw). Roles in role-rank order: human-only negatives, mixed-rewrite parents and mirror parents in three arms (topic terms and existence-by month; topic terms only; register and length only). Mixed operations rotate over prefix continuation, middle rewrite, middle infill (grammar-a-positives plan-v3 rules) and an isolated span rewrite. Writes the parents and grammar-a-positives requests. |
| `generate` | Runs one generator's requests against a local llama-server with the grammar-a-positives journals and acceptance checks (resumable), under a cohort generation profile (`--profile`, default `cc_cohort_profile_v1`, which needs `--norm`): the parent-conditioned normaliser, form screen and final-document length window run inside the acceptance path. `cc_era_parent_norm_v1` reproduces format-v1 and `raw` era-v1. |
| `norm-prior` | Writes the normaliser's batch prior (how many parents show each quote, dash, range and ellipsis convention) and its key seed. With `--profile cc_cohort_profile_v2` the batch is one population (`--population`, which must equal the population its `BATCH.json` names; no `--seed`): the prior is tallied population-wide and per parent domain, and its seed is the population's `norm_prior_seed`. |
| `renorm` | Re-verdicts existing journals under a normalising profile, per generator in completion order, without any model call. A request whose accepted response is now rejected keeps its remaining attempts for `generate`. |
| `topup` | Re-issues every exhausted, unaccepted request once to the next generator in the list, with the same prompt and span. |
| `assemble` | Splices accepted rewrites into their parents, collects mirrors, writes the mock cohort (20/55/25: every human-only and mixed document and the first dated mirrors per domain) and A request shards. Under a normalising profile every child must carry the normaliser record, and every document of every class passes the profile's every-class step, recorded per document; under `cc_cohort_profile_v1` a document of any class that fails the paragraph screen or the length window leaves the cohort (`excluded.jsonl`). `ASSEMBLY.json` adds word quantiles by origin and each origin's Kolmogorov-Smirnov distance from the human-only lengths. Under `cc_cohort_profile_v2` it also writes the R-form population (protocol A2 §7.2, as read by amendment 19): `rform-cohort.jsonl` holds, in parent order, every cohort human-only document unchanged, each cohort child's raw twin (`rform:<doc id>`, `twin_of` its cohort document; the accepted response's checked `raw_text` spliced into the same parent, present exactly when its cohort document is) and the `rform_screen_fail_v1` stratum (`rform_screen_fail_v1:<doc id>`, no `twin_of`): each child whose first raw response failed the form screen only, that response spliced into the parent and window-checked. The first raw response is the primary request's lowest attempt that returned model content, accepted or not, else the top-up's when the primary has none; a response rejected before the profile step counts and keeps the child out. `rform-excluded.jsonl` lists stratum documents the window or the splice left out (no text), `requests-rform/` shards the population like the cohort, and `ASSEMBLY.json` gains an `rform` record (rules, counts, form-sensitivity pairs, digests). The R-form documents are unique by text, and a duplicate refuses the assembly. The stratum belongs only to R-form evaluation populations; N0 and S0-13 take raw twins only. |
| `normalise` | Superseded by the cohort profile; kept to reproduce era-v1. Copies a cohort through the layout normaliser v0 (typographic quotes, dashes and ellipses to ASCII; Unicode spaces and zero-width characters; one paragraph per line) for re-scoring. |
| `gates` | Computes both versioned gate definitions and names the selected one (`--gate-defs`, default `cc_era_gate_defs_v2`). Retrains the era probe (the `era_probe` design) without any batch source group and reports the gate AUC with a pair bootstrap, by arm, generator, domain and parent year, plus the n-gram buckets behind the shift. Fits the era-only and format-only adversaries (cross-fitted ridge logistic models, linear and octile-binned, best of a small ridge grid, group folds, five fold seeds) and reports Brier gain over the constant prior as a share of B0, with the incremental gain over the reference's own logit; the format features are also fitted as their layout and typography part and their character-class block, and the length diagnostic as its word-count and mean-word-length parts. Computes B0 and the pooled seed-pair σ of dq at 1k, 2k and 4k cases (analytic, stratified by domain × origin, plus a stratified bootstrap), δ = max(0.01, 2σ) and w = 2σ·B0. Every mean is reweighted to the 20/55/25 target mix. |

Mirrors never see their parent's text; a mirror prompt carries the
parent's register, word count, eight TF-IDF key terms and, in the dated
arm, its capture month. Parents, generated text and the cohort stay in
`data/task-roots/cc-human/era-v1/`; `gates.json` holds aggregate statistics
only.

**Result (2026-10-31, era-v1).** The batch held 2,900 parents; local
generation used GLM-4-9B, Mistral-7B and OLMo-2-13B (about 49 GPU minutes,
one top-up round). The cohort held 400 human-only, 1,005 mixed and 500
model-only documents, reweighted to 20/55/25, plus 1,400 mirrors across the
three arms.
- **Era probe:** AUC 0.701 [0.682, 0.721] for dated mirrors against their
  parents. This **fails** the 0.60 gate. Topic-only mirrors score 0.679 and
  unconditioned ones 0.654, so date conditioning does not lower the AUC. The
  shift comes from register and vocabulary the probe learned as "late"
  (journey, the heart of, within, unique, ensure; fewer I, don't, know).
  Explicit years and dated topics contribute little.
- **Era-only adversary:** 3.0% of B0 [1.4, 5.2], which **passes**. Explicit
  era cues alone gain 0.1%, and the incremental gain over the reference is
  -1.8%.
- **Format-only adversary:** 25.3% of B0 [20.5, 32.3], which **fails**. It
  falls to 8.0% under layout normaliser v0 and 7.8% when every whitespace run
  is flattened, still above 5%. Its incremental gain over the reference is
  0.1% [-3.5, 4.1].
- **B0 and σ:** B0 = 0.103, 84% of it on mixed documents. A cross-fitted
  recalibration of the reference's logit alone reaches 0.080 (a slope and
  intercept fit gives 22.4% of B0 in sample). Pooled
  seed σ is 2.55% / 1.80% / 1.28% of B0 at 1k / 2k / 4k cases. That gives
  δ = 0.051 / 0.036 / 0.026 and w = 0.0052 / 0.0037 / 0.0026 Brier.

The report is `data/task-roots/cc-human/era-v1/REPORT.txt`.

**Gate definitions.**
- `cc_era_gate_defs_v1` (pre-registered for era-v1): probe AUC at most 0.60,
  era-only adversary at most 5% of B0, format-only adversary (all 51 format
  features) at most 5%.
- `cc_era_gate_defs_v2` (default): the era gate is the era-only adversary at
  most 5% of B0, and the probe AUC is a reported diagnostic. The format gate
  is the layout and typography adversary (the 43 format features outside the
  character-class block) at most 5%. The character-class block (case,
  digits, non-ASCII, symbols, emoji, punctuation density, control characters)
  is a monitored diagnostic.
- A gate passes on its point estimate; the bootstrap upper bound is reported
  beside it.

### Parent-conditioned normaliser (`era_norm`, `cc_era_parent_norm_v1`)

Pilot excerpts have one fixed form: squashed lines, one blank line between
paragraphs, and every paragraph passing the shared prose screen
(`fineweb::classify`). Generated children did not. The normaliser puts each
child into its parent's form inside the acceptance path, through the
grammar-a-positives `PostProcess` hook:
- **Conventions.** The parent's majority double-quote, single-quote, dash
  (em closed or spaced, spaced en dash, spaced hyphen, double hyphen), numeric
  range and ellipsis styles. Without a majority, quotes take the parent's
  other quote class, then a keyed draw from the batch prior. The prior counts
  every batch parent's own majority. Draws are keyed per document, so every
  attempt and the top-up of one document make the same choice. ASCII quotes
  become curly by context.
- **Layout.** Lines are squashed and blank lines dropped, which removes
  trailing and leading spaces, tabs and single breaks. Paired markdown
  emphasis markers are removed.
- **Paragraphs.** The child is rebuilt to the parent's structure. A mirror
  takes its parent's paragraph break fractions; a mixed child takes those of
  the human span it replaces. Breaks go to admissible sentence boundaries with
  the least total distance, every paragraph keeping at least eight words.
- **Form screen.** Each generated line must end a sentence and not be
  list-like, and each final paragraph holding generated text must pass
  `fineweb::classify`. A child that fails is rejected like any other
  acceptance failure.
- **Every class.** At assembly every document, human negatives included, goes
  through the excerpt layout and the paragraph screen, and the result is
  recorded per document.

Folded token sequences are unchanged, so the overlap screens see the same
windows. Whitespace word counts can change where a dash style changes
spacing. Records hold counts, conventions and digests, never text.

The pure rules (conventions, layout, normaliser, paragraph screen, profiles,
length window and the every-class step) live in the shared crate
`grammar-a-form`; `era_norm` re-exports them under their v1 names and keeps
the prior file, the acceptance hook and `renorm`. The `era_norm_v1_identity`
test pins their outputs on synthetic fixtures to digests recorded before
the move.

**Result (2026-10-31, format-v1).** The era-v1 journals were re-verdicted
under the normaliser, and only the affected children were regenerated
locally (about 16 GPU minutes).
- **Selection.** 252 of 2,174 accepted primary responses failed the form
  screen, mostly on unterminated title lines and quoted spans of eight or
  more words; 18 GLM rewrites became byte-identical to the span they
  replaced. The cohort then held 400 human-only, 988 mixed and 496
  model-only documents. Human texts were unchanged.
- **Gates** (shares of B0, bootstrap 95%; raw era-v1 values in brackets):
  - era probe 0.705 [0.701], still **failing**;
  - era-only 2.9% [3.0%], still **passing**;
  - format-only 7.4% (5.4 to 10.1) [25.3%], still **failing** the 5% gate.
- **Character-class split** (diagnostic). Without the character-class block
  (case, digits, non-ASCII, emoji, punctuation share), the format gain is
  1.7% (-0.2 to 3.8). The block alone gains 6.8%. Whether it counts as
  format is open.
- **Reference.** On mixed documents whose response was only normalised,
  R0's Brier rose from 0.089 to 0.143. Mixed Brier overall went from 0.156
  to 0.177 and B0 from 0.103 to 0.114, so the reference leans on generator
  formatting.

The report is `data/task-roots/cc-human/format-v1/REPORT.txt`.

### Cohort generation profile (`cc_cohort_profile_v1`)

The default post-process of cohort generation, for every class:
- the parent-conditioned normaliser and form screen above, in the
  acceptance path;
- one final-document length window, `cc_final_length_window_v1`: 150 to 450
  whitespace words, the human excerpt's own bound. It is checked in the
  acceptance path on the single-span assembly (the grammar-a-positives
  `doc_words` window), so a short or long child is regenerated with its
  remaining attempts;
- at assembly, the excerpt layout, the paragraph screen and the length
  window on every document of every class. A document that fails leaves the
  cohort whatever its class. Each document's record holds the post-process
  record, the every-class result and its word count.

A human-only document is its own parent, so the normaliser's target form is
the document itself and its text is never rewritten. The profile replaces
the layout normaliser v0, which rewrote every class to ASCII.

**Result (2026-10-31, the era-v1/format-v1 mock cohort, cc-human v3).** The
format-v1 journals were re-verdicted under the profile and only the
children the window or the screens dropped were regenerated locally (about
17 GPU minutes). The window rejected 335 accepted responses (299 primary,
36 top-up; 329 short, 6 long); the cohort then held 400 human-only, 947 mixed and 480 model-only members
and no document of any class failed the every-class step. Scores of
unchanged texts were reused (20 of 20 re-scored identically).
- **Gates v2** (shares of B0, bootstrap 95%; format-v1 cohort in brackets):
  era-only 2.9% (1.5 to 5.0) [2.9%], **passes**; layout and typography 1.6%
  (0.3 to 4.1) [1.7%], **passes**.
- **Diagnostics:** probe AUC 0.714 [0.705]; character-class block 6.9%
  [6.8%]; all format features 7.5% [7.4%].
- **Length:** the length-only diagnostic stays at 16.6% [16.6%] because it
  is almost all mean word length (16.7%); word count alone falls from 1.1%
  to 0.2% (-0.6 to 0.9). The window matches the word-count distributions
  (Kolmogorov-Smirnov distance from the human-only documents: mixed 0.04,
  model-only 0.12, with model-only shifted up because short mirrors are
  regenerated).
- **Reference:** B0 0.112 [0.114]; R0 mixed Brier 0.174 [0.177]; human-only
  Brier and flags unchanged (11 of 400 at the CAL 1% point). Seed-pair σ is
  2.53% / 1.79% / 1.26% of B0 at 1k / 2k / 4k, δ 0.051 / 0.036 / 0.025 and
  w 0.0057 / 0.0040 / 0.0028 Brier.

### Cohort generation profile v2 (`cc_cohort_profile_v2`)

Protocol A2 §6.2's profile for every round-2 positive, as amended by
amendments 7 and 10. It is `cc_cohort_profile_v1` with these changes (pure
rules in `grammar_a_form::{measure, transfer, screen, profile}`; that crate's
README states them in full):
- **Layout transfer `layout_transfer_v2`**, measured by `layout_measure_v2`.
  The child takes every whitespace-layout property from its source, measured
  exactly: the whole parent for a mirror, the replaced span's paragraphs and
  lines for a mixed child, with the whole parent's hard wrapping and width W.
  Widths are columns (leading whitespace and body). A single break is a
  paragraph boundary unless it is a wrap: a run of lines joined inside a
  sentence is first-pass evidence when a greedy wrapper could have made every
  one of its breaks and it reads as running prose, and the widest line before
  evidence is W; in a hard-wrapped text a second pass reads further wraps
  against W, never next to a list line, an unwrapped line (wider than W,
  outside every evidence run) or a change of leading whitespace. Each
  boundary's separator string is reused in order; a whole-text source's short
  first and last paragraphs (a greeting, a sign-off, a signature) are edge
  frames, kept as lines with their own separators; a child paragraph is
  wrapped when the source paragraph at its midpoint is; lines take the
  source's leading and trailing whitespace by role. A line-form source (more
  than half of its lines in failing paragraphs that hold no wrap: IRC, verse,
  transcripts, lists) keeps the child's own lines, joined by the source's
  separators in order.
- **Screens.** The licence screen `cc_form_licence_screen_v1` applies to every
  prose-path child: a reason outside the source body's licence (every
  `classify` reason of its body paragraphs) rejects a final body paragraph or
  a raw body line, and a reason outside the frame licence rejects a child
  frame line; a licensed list or unpunctuated paragraph end is kept as a
  fixed unit. A source that passes the screen has no frame and an empty
  licence, so its children get §6.2's screen exactly. Line-form children are
  not screened. The echo screen `cc_echo_screen_v1` applies to every child.
- **Priors.** Keyed convention draws come from the child's own population
  and domain (`norm-prior --profile cc_cohort_profile_v2 --population P` on
  a batch whose `BATCH.json` names P), then the population-wide prior, then
  none, under P's seed `norm_prior_seed(P)` (V21's plan key with x =
  `norm_prior`). P is one of the closed list `R2_POPULATIONS` (amendment 10
  item 10.8): `batch_population`, `write_prior_v2` and the loader refuse any
  other id. A v2 prior holds one batch's parents; a D-step's keyed pilot and
  its bulk are one batch. A v2 prior serves only profile v2, and a v1 prior
  only the v1 profiles; `generate`, `renorm` and `assemble` (and `era_batch`'s
  library entry points) refuse a mismatch, and the loader refuses a prior
  whose population or seed is not its batch's. Generation and assembly
  records name the prior's SHA-256.
- **Assembly** keeps v1's every-class step for this C-block tool.
- **Records** add the component ids, the source's layout class and measured
  properties, what the transfer applied, the licences and unlicensed reasons,
  the echo verdict and the raw-response SHA-256, never text.
- **Verdicts** keep the cleaned response the step read as `raw_text` only
  under profile v2 (`EraPost::is_profile_v2`: a v2 prior), where the R-form
  raw twin reads it. A v1 normaliser's verdicts keep none, so v1 `generate`
  and v1 `renorm` journals keep the bytes they had before profile v2.

On CC excerpt-layout parents without paired emphasis, whose paragraphs all
pass the screen, the v2 child equals v1's byte for byte (tests in
`grammar-a-form`'s `profile_v2` and `era_norm`). The example
`cc_era_profile_v2_parity` replays v1 `renorm` on recorded journals, anchors
each replayed verdict whole to the recorded one, runs the v2 child step
beside it with the same conventions, and writes counts and digests only. `tests/profile_v2_parity.rs` pins the form crate's copies of
the spliced assembly rules and the V21 plan key to `grammar-a-positives`.

## Data gates for A bundles (`slop_ninja_a_gates`)

Protocol A2's data gates on A rows (bundle, scoring or pilot rows), on the
CC gates' feature sets, adversary and era probe (`slop_ninja_cc_era gates`
itself is unchanged). Modules `a_gates`, `layout_audit` and `pilot_gate`
state the rules in full.

| Step | What it does |
| --- | --- |
| `gates` | G0a (default), G0b or G0c per block and per Tier domain: the layout and typography adversary (43 features), the era-only adversary (needs `--pool`; gated on C-block rows) and the word-count-only adversary, with mean word length and the character-class block as diagnostics. Origins are reweighted to 20/55/25 within each scope, folds are by source group, and gains are in Brier units. With `--b0-ref-t` / `--b0-ref-c` each gated gain is bounded at 0.05 × B0_ref of its block; without them the verdict is `pending_b0_ref` (the "before" figures). A block is always gated; a Tier domain is gated when every domain × origin cell it holds has at least 59 rows, with every human source pooled into `human_only` (amendment 17 item 17.1); the domain × class cells (humans by source) are reported and gate nothing. G0c refuses rows without a C block, and a run with no gated item is `vacuous`, never a pass. |
| `audit` | `r2_layout_audit_v1` per block × domain × class, on `layout_measure_v2` and `layout_transfer_v2`: trailing- and leading-whitespace line shares, single breaks and blank-line runs per document, the hard-wrapped share and W (in columns), separator strings, the line-form and screen-pass shares; the "before" figures of protocol A2 amendment 10 item 10.12 (1–7) and amendment 13 item 13.10 (2–5, 7–9) as amendment 14 reads them (`figures`, mapped by `figure_fields`): class and licence shares, frames, first-pass runs and their rejections, second-pass wraps, widths, lead changes, unwrapped lines, the sign-off proxy, self-identity and the verse, line-form and short-email counts; and the source-level stop invariants (`stop_invariants`, each zero before any keyed pilot). The re-verdict dry run's figures are counted on its children, not here. Counts only. |
| `sigma` | σ, δ and w from three seeds (17, 29, 41) by the cohort-mix method (protocol A2 amendment 12 items 12.3–12.4): domain × origin allocation at n with `dom_given` from the row-weight field (`--weight-field`, e.g. `cohort_weight`, recorded), σ = the stratified bootstrap SE of q (B = 2,000, seed 20261030, splitmix64, replicates shared by the seeds) pooled over 17–29, 17–41 and 29–41, with the analytic SE, its continuous-allocation form and `slop_ninja_cc_era gates`' reward-v1 formula reported beside it; σ on the manifest's 1e-18 grid and σ_eff = max(σ_m, σ′), with the Kish ESS of the row weights. |
| `pilot-gate` | `r2_pilot_gate_v1` on one D-item's pilot: six fixed child − parent layout properties, none of which moves with a child's length when the layout is copied (whole texts for mirrors, the replaced and generated lines for mixed children; amendment 12 item 12.2 as amended by amendment 16 item 16.1): the trailing- and leading-whitespace line shares, the shares of paragraph boundaries that are one newline and that are blank-line runs, the wrap reading (W for a whole text; for a span region 0 when unwrapped, 1 when it holds a wrap, else none), and the share of boundaries taking the parent's modal separator; per source group, the exact two-sided sign test per domain × property with Bonferroni over the gated domains' cells. In every gated domain the layout adversary gain on the domain's humans plus the children must be below the gain on the same humans plus R0's positives of the same origins and sources (`--before-rows`), both weighted humans 0.2, positives 0.8 (amendment 12 item 12.1). Children take their parent's domain and source group, need a source, and must be T-block. A smoke check: passing is not evidence of equivalence. |
| `paired-test` | The pilot gate's paired child − parent tests alone, reported without a verdict (G0a's Stage-1 layout-audit report). |

Classes follow §4.3: `human_<source>` for pooled, RAID, Enron, stage-3 and
CC humans, and `mixed` / `model_only` for positives of every source. Records
hold counts, statistics and digests, never text.

## Limits

- **WET text only.** The pilot uses CC's naive HTML-to-text extraction. Mail,
  forum and transcript units would need WARC HTML parsing and per-domain unit
  rules, so every domain uses paragraph runs.
- **Site templates.** They only count pages inside the pilot sample, which
  holds few pages per site. A larger draw finds more templates.
- **Content dates.** They are not parsed, and the existence-by date is the
  capture date.
- **Host closure** covers corpus-v1's FineWeb-derived records. Other web
  sources are handled by exclusion lists and by text dedup.
- **No detector scores** are computed or used by the sampler. The measurement
  binaries above read detector outputs produced elsewhere; they never filter,
  label or remove a document.
