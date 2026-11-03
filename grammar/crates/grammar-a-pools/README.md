# grammar-a-pools

Rust tooling for the human (negative) pools of the A model. It turns downloaded
public sources into normalised human documents, protects every prior corpus
snapshot from overlap, removes duplicates and writes one admission record per
source. It makes no network, model or detector calls; download runners live in
ignored private storage.

## Commands

| Command | Input | Collection | Family key |
| --- | --- | --- | --- |
| `gutenberg` | manifest of `pgN.txt` + `N.rdf` pairs | `project_gutenberg_historical` | `author_work_v2:<author>:<title>` |
| `wikipedia` | pages-articles XML stream | `wikipedia_enwiki_historical` | `wikipedia:page:<id>` |
| `wikinews` | pages-articles XML stream | `wikinews_enwikinews_historical` | `wikinews:<id>` |
| `stackexchange` | one site's `Posts.xml` (+ `Users.xml`) | `stackexchange_historical` | `stackexchange:<site>:question:<id>` |
| `plos` | JATS XML directory, file or `.zip` | `plos_historical` | `doi:<doi>` |
| `federal-register` | govinfo daily XML directory, file or `.zip` | `us_federal_register_historical` | `federal_register:<FR Doc>` |
| `fineweb` | FineWeb parquet shards + Public Suffix List | `fineweb_cc_historical` | `fineweb_page:<uuid>` (group `fineweb_site:<registered domain>`) |
| `global-voices` | WordPress REST post pages (`page-N.json`) | `global_voices_historical` | `global_voices_post:<id>` (group `global_voices_author:<account>`) |
| `student-essays` | PERSUADE 2.0, ELLIPSE or ASAP 2.0 CSV tables (zip entries read in place, published passwords) | `persuade_2_0_student_essays`, `ellipse_ell_student_essays`, `asap_2_0_student_essays` | `<corpus>:essay:<id>` (group `student_essay_prompt:<prompt slug>`) |
| `webis-tldr` | Webis-TLDR-17 Zenodo zip | `reddit_webis_tldr17_historical` | `reddit_post:<id>` (group `reddit_author:<opaque>`) |
| `ubuntu-irc` | Common Pile `ubuntu_irc` raw shards (`*.jsonl.gz`) | `ubuntu_irc_historical` | `ubuntu_irc:<channel>:<date>:<opaque>` (group `ubuntu_irc_speaker:<opaque>`) |
| `wikiconv` | ConvoKit WikiConv English `full.corpus.zip` per year | `wikipedia_talk_wikiconv_historical` | `wikiconv_utterance:<id>` (group `wikipedia_talk_user:<opaque>`) |
| `enron` | CMU Enron mail `tar.gz` (2015-05-07 release) | `enron_email_historical` | `enron_message:<hash>` (group `enron_mailbox:<opaque owner>`) |
| `europarl` | Europarl v7 source release `europarl.tgz` (`txt/en/`) | `europarl_en_original_historical` | `europarl_turn:<file>:<turn>` (group `europarl_speaker:<opaque name>`) |
| `nih-exporter` | RePORTER ExPORTER `RePORTER_PRJ_C_FYyyyy.zip` + `RePORTER_PRJABS_C_FYyyyy.zip` pairs | `nih_exporter_abstract_historical` | `nih_core_project:<number>` (group `nih_contact_pi:<opaque id>`) |
| `uspto-appxml` | USPTO APPXML weekly zips (pre-grant publications, no images) | `uspto_application_background_historical` | `uspto_app:<application>` (group: opaque practitioner, first-inventor, applicant or assignee key) |
| `uspto-select` | `uspto-appxml` output + metadata-only scans | joined application families, one row per family, writer, assignee and year caps, 12-word window screen | `uspto_family:<smallest member key>` |
| `uspto-check` | a finished USPTO pool and its reserved metadata | admission assertions (dates before 2019-01-01; families re-joined from the links: one row per family, no reserved row, no non-English filing office; caps) | |
| `cap-contributors` | an adapter output directory | same collection, at most N per `writer_key` (optional seeded document sample) | |
| `reviews --source amazon` | Amazon Reviews 2023 `review_categories/*.jsonl.gz` | `amazon_reviews_2023_historical` | `amazon_reviews_2023_review:<hash>` (group `amazon_reviews_2023_user:<opaque hash>`) |
| `cap` | static.case.law volume zips (`<reporter>__<volume>.zip`) | `caselaw_access_project_historical` | `cap_opinion:<case>:<index>` (group `cap_judge:j<jurisdiction>:<surname>`) |
| `courtlistener` | bulk opinions CSV (stdin or path) + clusters CSV | `courtlistener_opinions_historical` | `courtlistener_opinion:<id>` (group `courtlistener_person:<author id>`) |
| `govinfo-hearings` | `CHRG-*.htm` directory | `govinfo_congressional_hearings_historical` | `govinfo_chrg:<package>` (family and group) |
| `govinfo-cpd` | `DCPD-*.htm` directory | `govinfo_presidential_documents_historical` | `govinfo_dcpd:<package>` (group `us_president:<name>`) |
| `pdf-text` | PDF directory | one `.txt` text layer per PDF (no OCR) | |
| `fomc` | `pdf-text` output of FOMC transcripts | `fomc_transcripts_historical` | `fomc_turn:<meeting>:<index>` (group `fomc_speaker:<surname>`) |
| `select-candidates` | one adapter output | blocked cases dropped, per-group cap by id hash | |
| `sister --project wikivoyage` | pages-articles XML stream | `wikivoyage_enwikivoyage_historical` | `wikivoyage:page:<id>` (group: same) |
| `sister --project wikibooks` | pages-articles XML stream | `wikibooks_enwikibooks_historical`, `wikibooks_cookbook_historical` | `wikibooks:page:<id>` (group `wikibooks:book:<root title>` or `wikibooks:cookbook_page:<id>`) |
| `wikisource-meta`, then `sister --project wikisource --meta` | pages-articles XML stream (two passes) | `wikisource_enwikisource_historical` | `wikisource:work:<root>` or `wikisource:index:<scan>` (group `author:<name key>`, the Gutenberg writer key, else the family) |
| `f1000` | PMC Open Access JATS XML directory or `.zip` | `f1000research_reports_historical` | `f1000:report:<report DOI>` (group: opaque reviewer hash) |
| `orb` | ORB v1.0.1 export (Python literal) | `openreview_orb_reviews_historical` | `openreview:note:<id>` (group `openreview:forum:<id>`) |
| `loc-btp` | By the People dataset ZIPs + `btp-campaigns-v1.json` | `loc_by_the_people_historical` | `loc_btp_item:<ItemId>` (group `loc_btp_collection:<key>`) |
| `american-stories` | `faro_<year>.tar.gz` scan archives | `american_stories_historical` | `american_stories_article:<scan>:<article>` (group `american_stories_lccn:<LCCN>`) |
| `bl-hmd` | `biglam/hmd_newspapers` parquet shards | `bl_hmd_newspapers_historical` | `bl_hmd_article:<content hash>` (group `bl_hmd_title:<title>`) |
| `ocr-lexicon` | clean JSONL text (Train-side) | word list for OCR measures | |
| `ami` | AMI manual annotations zip (NXT words + segments) | `ami_meeting_corpus_manual_transcripts` | `ami_meeting:<id>` (group `ami_participant_component:<participant>`) |
| `icsi` | ICSI original MRT transcripts zip | `icsi_meeting_corpus_transcripts` | `icsi_meeting:<session>` (group `icsi_participant_component:<participant>`) |
| `sbcsae` | SBCSAE CHAT zip | `sbcsae_conversation_transcripts` | `sbcsae_recording:<SBCnnn>` |
| `fineweb-blogs` | FineWeb parquet shards + Public Suffix List + FineWeb pool scan | `fineweb_blog_cc_historical` | `fineweb_page:<uuid>` (group: the blog, see below) |
| `plos-blogs` | PLOS Blogs WordPress REST pages, one directory per blog | `plos_blog_historical` | `plos_blog_post:<blog>/<id>` (group `plos_blog_writer:<name>`) |
| `blog-splits` | blog split config JSON | Train, Train-mining and a metadata-only FineWeb blog reservation beside a base split | |
| `index-prior` | `prior-registry-v2.json` | protected index | |
| `scan-overlap` | registry + small adapter output directories | reverse overlap hits (`hits.jsonl`, text-free) | |
| `dedup` | adapter output directories + index | admitted pool | |
| `overlap-scan` | index of this pool's candidates + other pools' JSONL | text-free hits (reverse overlap check) | |
| `bucket` | family keys on stdin | `key<TAB>bucket` | |
| `reserve-splits` | split config JSON | metadata-only evaluation reservation, Train and Train-mining | |
| `extend-splits` | extension config JSON | base split kept byte for byte, new Train-side rows, new-domain reservations | |
| `extend-splits-v4` | extension config JSON | splits-v3 and blog splits kept byte for byte, per-source Train quotas, held and diagnostic routing, new evaluation domains, near-window check | |
| `select-eval` | sealed split directory + dedup output | evaluation humans (Test refused) | |
| `raid-adapt` | RAID `train.csv` / `extra.csv` (zstd decoded, decompressed SHA-256 checked) | `raid_benchmark_v1` (human and generated rows) | `raid_source:<RAID source_id>` |
| `raid-admit` | `raid-adapt` output + probe-filtered index | RAID pool (documents, metadata, admission, `SHA256SUMS`) | |

The family key formats match the existing corpus (`wikipedia:page:`, `doi:`,
`wikinews:`, `author_work_v2:`), so prior families are recognised directly.

## Document rules

- **Words.** Whitespace tokens, admitted only in `150..=450`, the bound of every
  current A human row.
- **Gutenberg excerpts.** The default rule is
  `body_percent_next_sentence_220words_end_sentence_max600_quotation_break_v2`.
  It starts and ends like
  `body_percent20_60_next_sentence_220words_end_sentence_max600_v1`, which
  `--excerpt-rule v1` still reproduces byte for byte. An excerpt that overlaps
  quotation material restarts after it, within the window up to the next 10%
  position; an excerpt without quotation material is identical to v1.
  - Quotation material: paragraphs whose every line is indented (block
    quotations, verse, inset letters), unless the book indents most
    multi-line paragraphs that way; and, outside fiction, quoted spans of
    eight or more words (`"…"`, `“…”`, `«…»`, `‘…’`). Fiction keeps its
    dialogue, except works whose RDF subjects mark criticism, biography,
    correspondence, quotations or compilations.
  - English screen on top of the RDF `en` tag: at least 90% of letters in
    Latin script and at least 20% of folded tokens among 64 common English
    function words.
  - The text is the exact UTF-8 slice of the downloaded file, and the byte
    span, restart count and both language shares are recorded.
- **Other sources.** These take the first run of consecutive whole prose
  paragraphs that reaches 220 words. A complete unit with at least 150 words
  is admitted whole. Nothing is cropped inside a paragraph or repaired.
- **Rendering.** Wikitext and HTML are rendered conservatively. A template,
  element or tag that would render inline text leaves a gap, and that paragraph
  is never admitted. The following break a paragraph run:
  - lists, headings, tables and code;
  - quotations (`blockquote`, quoted spans of eight or more words);
  - footnote references, formulas and superscripts.
- **Rights.** Admitted licences are public domain, CC0, CC BY and CC BY-SA,
  plus the ids recorded for classifier-training use (the FineWeb
  database licence, the By the People transcription id and the terms ids in
  `record::CLASSIFIER_TRAINING_TERMS`) and the no-derivatives licence of the
  spoken evaluation diagnostic (see spoken transcripts). Anything else, or an
  unstated licence, is rejected. `docs/a-competitive-protocol.md` lists the
  admitted and the excluded sources. The adapters record:
  - the licence and attribution;
  - share-alike obligations;
  - the evidence URL.
- **Dates.** Dated text must precede 2022-11-30. Pre-cutoff revisions and posts
  are required, and Stack Exchange edits after the cutoff are excluded. Public
  domain Gutenberg works are admitted as historical text, and their PG release
  date must also precede the cutoff.
- **Source screens.** Each source has its own exclusions:
  - Wikipedia and Wikinews: redirects, lists, disambiguation pages,
    translation notices, and the established copyright and notice terms.
  - Wikinews: unpublished drafts, and articles from before the 2005-09-25
    CC BY 2.5 relicensing.
  - Stack Exchange: community-wiki and ownerless posts.
  - Federal Register: documents that mention copyright, and filings by
    self-regulatory organisations.
- **FineWeb.** A seeded row sample per parquet file (`rank(seed,
  "fineweb_row", id)` below `target / rows`), crawl capture before the cutoff,
  FineWeb `language == "en"` with score at least 0.8, a registered domain
  (Public Suffix List, ICANN and private sections) not on the list of other
  pool sources' sites. Text lines are paragraphs; list-like lines,
  boilerplate terms and lines repeated on two or more pages of the same site
  break runs. Rows carry the ODC-By database licence id with
  `redistribute_text: false`; page text itself is not licensed.
- **Global Voices.** Published, unprotected posts whose publication and last
  modification precede the cutoff; a post with an origin notice (translated,
  republished, partner or permission material) is rejected whole; captions,
  embeds, quotations and bracketed editorial notes break runs. CC BY 3.0.
- **Student essays.** PERSUADE 2.0, ELLIPSE and ASAP 2.0 (CC BY 4.0 per
  the READMEs at the pinned commits; earlier revisions stated CC BY-NC-SA 4.0,
  recorded per row). Rows carry `redistribute_text: false`, writer
  demographics as provided (grade, ELL status, gender, race/ethnicity,
  economic and disability status) and a removal cohort id.
  - Dates are corpus termini: PERSUADE before 2022-03-25 and ELLIPSE before
    2022-08-31 (their public competitions). ASAP 2.0 essays that are not
    PERSUADE essays have no dated evidence and are rejected unless
    `--collection-terminus` and its basis are given.
  - ELLIPSE and ASAP 2.0 rows whose Kaggle essay id is a PERSUADE id (from
    the PERSUADE run's text-free `essay-ids.jsonl`) are rejected, so each
    essay enters once.
  - Line ends become LF, C1 controls are decoded as Windows-1252, and
    whitespace inside a paragraph is collapsed (no-break and doubled
    spaces), so typing artefacts are not a human cue. Paragraphs are split
    on blank lines, or on line breaks when an essay has none.
  - A paragraph breaks the run when it has a de-identification placeholder
    (any `word_word` token), mojibake, a URL, damage, a quoted span of 8+
    words, or at least half its folded tokens in 10-token windows shared by
    5+ essays of the same prompt (copied source or prompt text). There is no
    sentence-end or short-paragraph rule, so weak student prose stays in.
    Excerpts above a 0.30 shared-window share are rejected.
  - The bucket and split group is the prompt, shared across the corpora; the
    releases have no writer id, so writer closure is unavailable.
- **Forums and chat** (`forum.rs`). Admitted under the classifier-training
  policy of 2026-10-27: lawful public access, licence or terms and
  basis recorded per row (`rights.admission_basis`, `rights.cohort_id`),
  `redistribute_text: false`, one removable cohort per source.
  - Contributor keys are opaque (`<prefix>:<20 hex>` of a domain-separated
    SHA-256 of the account, or of channel and nick); raw names are never
    written to metadata. Buckets use these keys.
  - Informal screen: the shared prose screen without the eight-word minimum
    or the sentence-final punctuation requirement. Quoted replies (`>`),
    URLs, residual markup, lists, code, long quotations and quotations that
    span paragraphs still break runs.
  - Reddit (Webis-TLDR-17): markdown blocks; link text kept, balanced
    emphasis markers removed, strikethrough and superscript leave a gap;
    TL;DR paragraphs break runs (every Webis post has one by selection).
    Dates are bounded by the corpus (posts 2006-2016, published 2017-11-07).
  - Ubuntu IRC: one document per speaker, channel and log day; message text
    only (timestamps and nick prefixes are not text); whole messages joined
    by newlines; a break at 30-minute gaps, bot commands, symbol-heavy
    messages and angle brackets (pasted chat); name-based bot screen; URL
    date before the cutoff.
  - Wikipedia talk (WikiConv): unmodified, undeleted, unrestored utterances
    only; conservative wikitext rendering; substituted template messages
    (welcomes, warnings, notices) rejected; visible upstream link-removal
    holes (space before punctuation, leading punctuation, pipes, empty
    parentheses) break runs; a trailing signature is stripped from the
    final paragraph and counted, any other `(UTC)` signature breaks.
- **Reviews** (Amazon Reviews 2023). Admitted under the classifier-training
  decision of 2026-10-27, for Train-side use; the dataset asks for
  citation. Rows carry a terms identifier, `redistribute_text: false`, `use_scope`, a per-source
  `cohort_id` and the terms snapshot hashes in the report.
  - Unit: the review body without its title; a body of 150..=450 words is
    admitted whole, a longer one yields its first run of whole lines that
    reaches 220 words; a line with a quoted span of eight or more words
    breaks runs and is never admitted. HTML breaks become newlines (two or more: a paragraph
    break), character references are decoded and whitespace inside a line is
    squashed; each is counted per row and per run.
  - Whole-review rejections: Amazon video and product embeds, other HTML
    tags, `[[`/`]]`, URLs, unknown references,
    damage, the English screen and an ASCII-letter share below 0.9.
  - Convention flags (incentive disclosures, Vine mentions, edit/update
    notes, spoiler mentions, emoji) are recorded and counted, never used to
    select.
  - Dates: the Amazon review timestamp.
  - Groups: the bucket and `writer_key` are an opaque hash of the dataset
    user id, which is not stored. Per input file the eligible reviews with
    the lowest seeded rank are kept up to a quota proportional to the square
    root of the file size, with at most `--max-per-user` per reviewer across
    the run.
- **Court opinions.** Judicial opinions are public domain (government edicts
  doctrine); rows carry `public-domain-us-judicial-opinion`.
  - Caselaw Access Project: only `casebody.opinions[].text` is read; publisher
    head matter (syllabi, headnotes, parties, attorneys) never is. The judge is
    the printed author (the `author` field, or a first line of at most eight
    words with a judicial marker) reduced to one surname within the
    jurisdiction; per curiam, multi-author and unparsed lines are rejected.
    Puerto Rico (translated opinions) is excluded.
  - CourtListener: only structured HTML sources (`html_lawbox`,
    `html_columbia`, `html_anon_2020`, then `html`); `xml_harvard`-only
    opinions (CAP text), PDF-layout `plain_text`-only opinions, OCR-flagged
    and per curiam opinions are rejected; cluster syllabus, headnote and
    summary fields are never read. The group is the linked author person.
  - Footnote bodies, numbered points and star-paging markers break runs. A
    case with an authored opinion outside the rendered buckets is skipped
    whole (CAP in the adapter; CourtListener through
    `blocked-case-keys.txt` and `select-candidates`).
- **Spoken government records.** Speaker labels are removed from excerpts;
  audience-reaction markers (`[Laughter]`, `[Applause]`) are removed and
  counted; any other bracketed annotation breaks the paragraph.
  - Hearings: GPO four-space paragraphs, split into speaker turns; prepared
    statements and other bracketed insertions are excluded up to the next
    turn, the appendix is never read, and the latest front-matter hearing date
    must precede the cutoff. Speaker keys are opaque per hearing.
  - Presidential documents: spoken titles only (remarks, addresses, news
    conferences, interviews, exchanges); only the president's own turns;
    the editorial note and index metadata are excluded.
  - FOMC: the PDF text layer (`pdf-text`; image-only scans yield no text and
    are not OCRed), all-capitals speaker labels, page furniture dropped and
    counted, and paragraph ends reconstructed after sentence-final lines
    shorter than 0.85 of the median line width.
- **OCR artefacts.** `legal::ocr_stats` counts hard glyph artefacts and soft
  token artefacts (letters glued to digits outside ordinal and reporter-series
  shapes, a full stop before a lowercase word, a line-break hyphen, an
  isolated lowercase letter). Any hard artefact, or more than one soft
  artefact per 100 words, rejects the document; both counts are recorded per
  row (`extraction.unit.ocr`) so their rate can be compared with positives.
- **Removable cohorts.** Rows of these sources carry `rights.cohort_id` and
  `provenance.cohort_id` (`legal_gov_<source>_v1`).
- **Wikimedia sister projects.** Pre-cutoff dumps; whole pages rendered,
  headings break runs, reference-type headings end the page. Wikivoyage keeps
  one document per page; Wikibooks keeps at most 10 per book (print versions
  excluded) with the Cookbook namespace as its own collection; Wikisource keeps
  at most 5 per author or work. A group cap chooses by
  `h64("slop_ninja_sister_cap_v1\0" + unit id)`, never by text.
  - Wikisource needs a text-free metadata pass (`wikisource-meta`): header
    year, author, translator and licence templates of main-namespace pages,
    and scan-index fields. A main-namespace page needs a PD, CC0, CC BY or
    CC BY-SA template on itself or its root page. A `Page:` transcription
    needs proofread level 3 or 4, a licence from a transcluding page or a
    scan-index publication year before 1927, and drops the first and last
    paragraph of the scanned page (they may continue across pages).
    Translated works and non-English indexes are excluded.
    Author groups use the Gutenberg writer key `author:<first_last>`, so
    writers reserved for Gutenberg evaluation stay in the reserved buckets.
    A paragraph that starts in lowercase continues one split by a page-break
    template or scan boundary and breaks the run.
- **Peer review.** F1000Research reports are dated and licensed per report
  (`sub-article` pub-date and licence link) and grouped by an opaque hash of
  the first reviewer's folded name. A report without its own licence link
  (older records) takes the publisher's policy licence for reports (CC BY)
  only when the article links a CC BY licence; `rights.licence_basis` records
  which applied. Author `response` sub-articles are never read. OpenReview reviews come from ORB v1.0.1,
  which has neither reviewer identity nor review dates: the group is the
  reviewed submission and the date is an upper bound from the venue year
  (venues dated 2021 or earlier, and the ICLR 2022 main conference). Paragraphs
  whose folded text recurs in at least five reports are questionnaire or
  confirmation boilerplate and break runs. Plain-text reviews split on line
  breaks, except hard-wrapped blocks; lists, markdown, LaTeX and URLs break
  runs. At most 5 documents per group.
- **Transcription artefacts.** Every new adapter screens the excerpt with
  `transcription_artifacts`: long s, typographic ligatures, soft hyphens,
  not-signs, split line-end hyphenation, bracketed editorial notes and common
  OCR confusion tokens reject it; a digit inside a word is only recorded. The
  text is never altered, and the markers are recorded per row.
- **Historical sources** (`ocr` module).
  - OCR and transcription text is normalised inside a paragraph block only:
    hard line breaks become spaces, a line-end `letter-` before a lowercase
    line is joined (the hyphen is kept only when both halves are lexicon
    words and the joined form is not), ligatures and the long s are mapped,
    and soft hyphens removed. A lowercase-initial block continues an
    unfinished previous block; a block still starting in lowercase never
    opens an excerpt. Every edit is counted per document; misreadings are
    never corrected.
  - `ocr_quality_v1` records the lexicon share of alphabetic tokens (all and
    lowercase-initial), the odd-token share (mixed letters and digits other
    than ordinals, symbol-only tokens) and the odd-character share. The
    newspaper gate requires at least 0.90, 0.95, at most 0.02 and 0.005.
    The lexicon (`ocr-lexicon`) is built from Train-side text.
  - American Stories: every content box of the article classed `Legible`;
    newspaper LCCN is the bucket and split group. BL HMD: `ARTICLE` rows,
    repeated identical rows skipped; the newspaper title is the group.
    Both use a seeded identifier-rank sample.
  - By the People: only campaigns whose dataset README states that
    transcription and review were completed before 2022-11-30 (tracked
    `btp-campaigns-v1.json`, each evidence sentence verified verbatim). The
    collection key in the manifest is the bucket and split group, and up to
    three successive paragraph runs are taken per item. Markup
    (`btp_markup_v2`): deleted text in square brackets is removed;
    marginalia, `?` illegibility marks, brackets with editorial notes,
    unbalanced brackets and parenthesised capitals transcriber notes are
    breaks; page-number lines and `--N--` marks are dropped; short heading,
    salutation, closing and signature lines are split off; paragraphs with
    two or more speaker labels are not admitted.
- **Spoken transcripts** (`spoken`, `ami`, `icsi`, `sbcsae`). Human
  transcripts only; ASR output is never read.
  - Unit: a run of consecutive whole speaker turns, one paragraph per turn
    with an opaque letter label (`A:`, `B:`) by first appearance; 150-450
    whitespace words including labels; non-overlapping windows per
    transcript.
  - Non-lexical transcription markup (pauses, breath, laughter and noise
    codes, overlap brackets, timing, lengthening, voice-quality scopes,
    stage notes) is removed and counted per document
    (`extraction.unit.normalization`). Unintelligible, redacted,
    foreign-language, translated or inserted-recording material is a break
    that no excerpt crosses, and any markup character left after
    normalisation makes the turn a break. Fillers, word fragments and
    restart dashes are speech and stay; their rates are recorded.
  - Rights: AMI and ICSI CC BY 4.0 (any split); SBCSAE CC BY-ND 3.0 US as
    an ungated evaluation diagnostic only. SBCSAE rows carry
    `admission_basis` (classifier-training use, user decision
    2026-10-27), `redistribute_text: false` and `allowed_splits`, which
    `record::admitted_rights` requires for any licence outside the default
    set.
- **FineWeb blogs.** Every row (no row sample) whose URL is a blog page:
  a Blogspot registered domain, a user blog on a multi-tenant platform the
  Public Suffix List does not separate (WordPress.com, LiveJournal,
  Typepad, Tumblr, Medium, Xanga and others), a `blog.` host, a `/blog/`
  path, a Blogger or dated-slug permalink, or `blog` in the registered
  domain name. FineWeb's metadata gates and line screens apply, plus blog
  chrome lines (comment headers, labels, post navigation), a per-blog
  template screen and a first-person screen on the excerpt (at least three
  singular or five plural first-person forms, at least 1.2% of words).
  The split group is FineWeb's `fineweb_site:<registered domain>` for
  non-hosted blogs and `fineweb_blog:<blog id>` for hosted-platform blogs;
  a hosted blog with any page in the FineWeb pool's metadata scan
  (`--inherit-scan`) takes that page's platform-wide site group, so a blog
  never spans the two pools' splits. A counts-only register lexicon label
  and first-person counts are recorded per row; neither selects rows.
- **PLOS blogs.** The network default is CC BY 4.0 "unless otherwise
  noted"; archived independent blogs "may maintain" CC BY-NC and are
  excluded whole. Each current-blog post is rejected on a non-commercial,
  no-derivatives, all-rights-reserved or restrictive Creative Commons
  notice or link, or an origin notice. Writers are keyed by display name
  (Co-Authors Plus byline terms resolved to users, then the posting
  account). Train side only.
- **Blog splits.** `blog-splits` reads a base split directory without
  changing it, assigns admitted blog rows to Train and Train-mining by
  connected unit (shared writer or `split_component`) with per-domain
  quotas and per-writer caps, holds base evaluation families and PLOS blog
  writers of reserved PLOS evaluation groups, and lists the FineWeb blog
  scan's eligible reserved-bucket groups as an unallocated metadata-only
  reservation.
- **Contributor buckets.** For FineWeb and Global Voices the bucket key is the
  contributor (site, posting account) while the source family is the page or
  post, so a site or writer never spans splits and one overlapping page does
  not remove its whole contributor at dedup.
- **PLOS prose licences.** `--prose-licence-only` emits only articles without
  a Creative Commons link whose licence statement is CC BY, CC0 or the
  Creative Commons Public Domain declaration in prose; link-licensed articles
  are counted and skipped, so the default route's output is unchanged.
- **Buckets.** `--buckets LO..HI` keeps only families whose
  `bucket(family key)` lies in that range of `0..1000`. Reserved evaluation
  buckets are therefore never rendered by Train runs. Gutenberg buckets the
  work family from RDF metadata before it reads the book text.
- **Gutenberg plan.** `gutenberg-plan` is metadata only. It reads the catalog
  inventory, the bulk RDF feed and the prior index, and never reads book text.
  The split group is the PG writer, identified by name key and PG agent id.
  - A work is excluded on any of these: a rights or metadata failure, a prior
    PG id, title alias or family key, a prior writer (name or agent), or a
    translator, compiler, contributor or second-author relator.
  - A work whose writer bucket (`bucket("author:<author_key>")`) lies in the
    reserved range is `reserved_evaluation`. That applies to every work of the
    writer.
  - A Train writer's work whose family bucket lies in that range is held back
    and never downloaded.
  - The rest form `train-selection.txt`.

- **Pile components** (`pile.rs`). Human components of the Pile are read
  from their original releases, because the Pile keeps no writer metadata.
  - Enron: only messages in the owner's sent folders whose `From` address
    contains the owner's surname. The text before the first quoted, forwarded
    or embedded-header line is kept, cut back to the start of that line's
    paragraph. Addresses, phone numbers, quoted-printable residue, aligned text
    and re-wrapped (pasted) paragraphs break runs. Greetings and sign-offs stay.
  - EuroParl: English files only. A turn qualifies when it has no `LANGUAGE`
    attribute and no inline `(XX)` mark naming another language, in a file
    that marks original languages by either means. Translated speeches and
    chair turns are excluded. The leading report label (`in writing. -`) and
    an `(EN)` mark are removed as headers. Each line is one paragraph.
  - NIH ExPORTER: abstracts are joined to their fiscal-year project row. The
    leading `[unreadable]` marker and the description label are removed, and
    the trailing public-health-relevance or narrative section is cut.
    Unreadable markers, unclosed brackets and symbol residue break runs. The
    date is the award notice date, else the budget or project start date.
  - Lost characters: a `?` that stands for a character the upstream export
    could not encode breaks the run (`lost_character_question_mark_screen_v1`).
    That is a `?` directly followed by a letter or digit, or preceded by the
    start of the text, whitespace or an opening bracket. In EuroParl and NIH
    it also counts when followed by `,;:.` or by a space and a lowercase
    letter or digit. NIH abstracts of later fiscal years lose apostrophes,
    quotes, dashes and Greek letters this way, and a residue only human rows
    carry would become a class cue.
  - Line breaks inside a paragraph are kept as written (Enron mail and later
    NIH fiscal years are hard-wrapped), as in the Project Gutenberg pool.
  - USPTO backgrounds are not read from HUPD, whose card states CC BY-NC-SA
    4.0 in its licensing section; see the USPTO rules below.
  - Bucket keys are opaque contributor hashes: mailbox owner, speaker name
    (order-free, initials dropped) and contact PI.

- **USPTO application backgrounds** (`uspto.rs`). Read from the USPTO bulk
  product "Patent Application Full-Text Data (No Images)" (APPXML weekly
  files), Train only.
  - Formats: `pap-v15`/`pap-v16` (2001-2004) and the ST.36-based
    `us-patent-application` v4.x DTDs. Sequence-listing documents are
    skipped. Documents are parsed without their DTD. Named entities are
    decoded from a fixed table: XML, the HTML/ISO 8879 text characters and
    ISO Greek. Any other entity leaves a gap.
  - Date rule: the application's own filing date and its publication date
    must both precede 2019-01-01, before GPT-2. Continuations, divisionals
    and CIPs are judged by their own filing date, never the priority date.
    Missing or unparseable dates reject the unit. Filing, publication and
    earliest priority dates are recorded per row, and `uspto-check` fails
    the pool unless `max(filing_date, publication_date) < 2019-01-01`
    holds for every row.
  - Rights: published applications are public records. The USPTO terms of
    use state that patent text is typically not subject to copyright
    restrictions, subject to 37 CFR 1.71(d)-(e) notices. Any application
    whose text mentions copyright or a mask work is rejected whole. Rows carry `redistribute_text: false`
    and `evaluation_use: train_only`.
  - Section: from the first heading naming a background section to the
    next heading that is not a background subheading (field, related or
    prior art). A heading that combines background with summary rejects the
    unit. Subheadings break runs. Claims and other sections are never read.
    Tables, formulas, chemistry, images, lists, superscripts, subscripts,
    tagged citations and in-line formula instructions leave a gap, so their
    paragraph breaks the run.
  - Excerpt: one per application. It is the shared whole-paragraph run, or
    else the earliest whole paragraphs under 450 words completed with
    leading whole sentences of the next paragraph up to 220 words. A
    sentence ends at `.`, `!` or `?` before an uppercase letter, except
    after abbreviations, initials and dotted tokens. Rows cut this way
    record `cut_inside_paragraph_at_sentence_end`.
  - Paragraph screens: the shared prose screen, the lost-character `?`
    screen, symbol residue, unbalanced brackets, pipes and OCR artefacts
    (`inven- tion` hyphenation, digits inside words, spaces before
    commas). The excerpt also passes the English screen.
  - Groups: the bucket and `writer_key` are an opaque key of the
    practitioner or correspondent named in the bibliographic data
    (publications through early 2011 carry a correspondence address), else
    the first inventor, applicant or assignee. The key is a grouping key, not an authorship claim.
    `uspto-select` joins application families as the union of application,
    parent, child, provisional, PCT, foreign-priority and
    earlier-publication numbers over candidates and reserved rows. An
    application's own key (`uspto_app:N`) and the `us_app:N` key printed by
    related applications are one key. Families with a reserved row are
    dropped. Families naming a foreign-priority or PCT receiving office
    outside the English-language offices (US, GB, IE, AU, NZ, CA, IN, SG,
    ZA) are dropped whole as probable translations; EP and IB count as
    non-English because their filing language is not recorded. Foreign
    applications that print no priority or PCT number are not detected.
    It then keeps candidates in
    seeded order, at most one per family, under the writer-key, assignee
    and publication-year caps, and drops any candidate sharing a folded
    12-word window with a kept one (boilerplate and near duplicates).

## RAID benchmark cohort

`raid.rs` maps the labelled RAID splits (Dugan et al., ACL 2024; MIT
dataset) into a removable Train-only cohort. Unlike the other pools it holds
both classes: human rows are `human_only`, generated rows are `model_only`
with `share_target` 1 and one whole-text `ai_byte_spans` entry. Every row
records the RAID id, source id, generator (label, checkpoint, access and
terms), decoding strategy, repetition penalty and attack (name, RAID
mutation rate and what produced the final text), plus format flags (list
lines, heading-like lines, hard wraps, repeated word 8-gram share).

- **Rights.** Each component is gated on non-commercial terms, and the
  terms and decision are recorded per row under `rights.components` and in
  `admission/rights.json`.
  - Human rows are admitted for book summaries (CMU Book Summary Dataset,
    CC BY-SA 3.0), Reddit (Webis-TLDR-17, the same basis as the Reddit pool)
    and Wikipedia (GPT-wiki-intro intros from the 20220301 dump, CC BY-SA
    3.0).
  - Human rows are excluded for news (BBC dataset: non-commercial and
    research only), recipes (RecipeNLG: non-commercial research and
    education only), reviews (IMDb: personal and non-commercial use only),
    poetry (poems owned by their authors and reproduced by the source site
    for educational and informational purposes) and abstracts (RAID states a
    2023-or-later filter, so pre-cutoff composition is not established).
  - Generated rows are title-conditioned. They are admitted for every
    English domain except MPT-30B-Chat (CC BY-NC-SA 4.0).
  - The terms of each generator (for example the Cohere terms of use) are
    recorded per row as stated, with their URL.
  - The `extra` split is code, Czech and German: never admitted.
- **Attacks.** Attacked rows are a seeded per-mille sample by RAID id.
  - Human rows with the paraphrase attack are DIPPER rewrites. They become
    `model_only` source-conditioned paraphrases in their own cohort.
  - Human rows with the synonym attack (model-chosen word substitutions
    without recorded spans) are excluded.
  - Other attacked human rows stay `human_only` with the attack flag.
- **Shape.** Outer whitespace is trimmed. A row is kept whole when it has
  150..=450 whitespace words; otherwise the first run of whole blank-line
  paragraphs that reaches 220 words is kept. The Latin-letter floor
  applies, with a function-word floor of 0.10 (homoglyph rows are exempt).
- **Cohorts.**
  - `raid-v1-paired`: domains with admitted humans.
  - `raid-v1-unpaired-domain`: generations without RAID human negatives.
    Weight these with care against domain shortcuts.
  - `raid-v1-human-paraphrase`: the DIPPER rewrites of human rows.
- **Admission.** The prior index is probe-filtered to the unattacked
  candidates.
  - A text or family conflict excludes the whole RAID source group.
  - A row that shares a whole-text key or a folded 50-word sequence with an
    earlier-ranked row of another group is excluded, and the two groups
    become one `split_component`.
  - Exact duplicates inside a group are excluded. Attack variants inside a
    group are kept.
- **Leaderboard contract.** `examples/raid_leaderboard.rs` has two commands.
  - `requests` writes A requests (`{"id","text"}`, text byte-identical to
    the RAID `generation`) for the public test CSV.
  - `predictions` joins the full-A runner's `predictions.jsonl` back into
    the leaderboard's `predictions.json`: `[{"id","score"}]`, where the
    score is `ai_confidence_percent / 100`, in test order. It also writes a
    template `metadata.json` and a `run.json` binding every hash.
  - It never runs A and never submits.

## Overlap and duplicates

`index-prior` reads every registered file: JSONL, JSON and their `.zst` forms,
plus plain text such as raw downloaded books. It indexes every string of at
least 20 words, including prompts and nested provenance, as:

- exact, whitespace-normalised and case/punctuation-folded hashes;
- whitespace and folded 50-word sequences;
- family keys, Project Gutenberg ids and title aliases.

An unparsable file is still indexed as text.

Registry names are shell globs: `*` matches any run of characters anywhere
in the pattern (`pg*.txt`, `*vectors*.jsonl`) and `?` one character.
`prior-registry-v1.json` indexes were built with an earlier matcher that
treated a middle star as a literal name, so they missed the raw stage-1
`pg*.txt` books; use v2 (`index.json` records `name_matcher`).

`dedup` applies these rules:

- A family with any prior overlap is excluded whole, with no replacement.
  `--exclude-alias pg:N` excludes a named work's family the same way.
  `--reserved-metadata FILE` (a metadata-only scan of the reserved buckets)
  excludes every family listed there the same way. Buckets follow the writer,
  so a family whose writer key changes between units (an NIH project whose
  contact PI changed) could otherwise straddle the reservation.
- Within the pool, candidates are visited in a fixed hash order of their ids.
  The first copy is kept, and later exact, normalised, folded or
  shared-50-word-sequence copies are excluded and the kept copy is named.
- Families joined by a cross-family duplicate share a `split_component`, which
  later split assignment must keep together.
- Optional screens (method `…_v2`):
  - `--near-window 12 --near-collection C`: inside collection `C`, a
    candidate that shares any folded 12-word window with an earlier admitted
    candidate of `C` is excluded and names it, so a template family keeps one
    member. No family link is made. The A pools use it for the Federal
    Register.
  - `--residue-collection C`: MediaWiki rendering residue (a pipe, an empty
    `( )`, a literal `[n]` marker, unbalanced square brackets) excludes the
    candidate.

**Reverse scan.** For a small pool, `scan-overlap` holds the candidates'
keys in memory and streams every registered file against them, with the same
checks as `index-prior` (all JSON strings of 20+ words, text hashes, both
50-word sequence kinds, and family-key or alias equality) behind a folded
10-token window prefilter. Memory follows the pool rather than the registry,
and a file a concurrent track is still writing is read as it stands and its
status recorded (`scanned_but_changed_during_scan`, `read_error_partial_scan`
or `vanished_before_scan` for optional entries). `dedup --prior-hits
hits.jsonl` then treats each hit as a prior conflict, so its family leaves the
pool whole; pass an index built from an empty registry as `--index`.

`dedup` writes `admission/<collection>.json` (`slop_ninja_a_pool_admission_v1`)
next to the admitted records.

## Splits

`reserve-splits` reserves Test, CAL, Dev-select and Dev-iter from metadata
only and assigns admitted documents to Train or Train-mining by whole
`split_component`.

- `excluded_domains` removes a domain from every split. Its targets and Train
  quota must be zero, and a pool row of that domain is an error.
- `train_max_docs_per_contributor` caps Train documents per Gutenberg writer
  and per Federal Register office unit (department and first sub-agency). The
  summary reports distinct contributors and their inverse Simpson effective
  number.
- `protocol_minimum_human_docs` and `backfill_domain` add capacity checks
  against the protocol minima and a declared backfill rule to every
  `SEALED.json`.
- `train/documents.jsonl` and `train-mining/documents.jsonl` carry the text
  rows with `frozen_split`. They are the only files trainers, generators and
  assembly read. Pool files still contain held documents and are not training
  inputs.

Admission is not a split assignment, a tokenizer qualification or evidence of
model quality. Every human label remains a historical or pre-cutoff proxy.

### Split extension v4

`extend-splits-v4` (`splits_v4.rs`) extends splits-v3 and the per-blog blog
splits without changing any earlier assignment.

- Every listed file of both base directories is verified against its
  `SHA256SUMS`. A listed file that never existed is accepted only when the
  config names it (`base_sha256sums_known_missing`), and is recorded in
  `base-v3/SHA256SUMS-defect.json`.
- `train/`, `train-mining/` and `held/` start with the exact splits-v3 bytes,
  then the exact blog-split bytes, then the new rows. Evaluation files of
  splits-v3 keep their paths; the blog evaluation reservation is copied to
  `blogs-v3/`.
- The pool is one cross-source deduplication of the new per-source pools
  against a registry of every earlier pool (`prior-registry-data4-v1.json`).
  Each collection maps to a source with a use: `train` (seeded Train quota
  and a per-contributor cap; the rest is Train-mining), `held` (never Train
  or mined) or `diagnostic` (evaluation-only, an ungated slice in
  `diagnostics/<slice>/`). An excluded collection in the pool is an error.
- New evaluation domains are reserved in `new-domains-v4/` from metadata-only
  scans (`adapter_metadata_scan`, `essay_ids`, `blog_reserved_units`) of
  never-rendered buckets. A reserved group key that is a Train-side group key
  of the domain's collections stops the run; units whose family has
  Train-side or earlier evaluation text are dropped.
- `near_window_check` lists acquired evaluation candidates of one collection
  that share a folded N-word window with that collection's Train-side rows
  (text-free), for exclusion at evaluation selection.
