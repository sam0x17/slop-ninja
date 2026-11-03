# Reference detector data sources

Screened on **2026-09-10**. This register supports the [detector plan](PLAN.md).
It records acquisition candidates and their admission conditions. Admission requires
an exact version, item-level rights evidence and a recorded origin-label basis.
No new corpus has been downloaded or admitted through this document.

## Admission policy

Start with CC0, CC BY and documented public-domain text. Commercial reuse under
those terms can support the project, subject to attribution and item exceptions.
Record detector training, public text redistribution, public model release and
source-conditioned generation as separate intended uses. A repository's software
license, a dataset card label or public availability alone cannot establish the
rights for every incorporated text. Equally, a conditional entry below does not
mean its license is invalid or that all of its records are unusable.

Keep corpora, extracted text and generation records in ignored `data/`. Check in
source manifests, acquisition code, aggregate counts and source documentation.
Select the model-release license after the admitted data and base-model licenses
are known. CC BY-SA sources require a compatible plan for any redistributed
adapted text; do not assume that this establishes the license of trained weights.

Preserve these distinct origin-label classes:

- **Documented production:** writing-session evidence, author attestations with
  recorded editing scope, or our own exact generation/edit logs. Record what each
  establishes and how incomplete records were handled.
- **Historical proxy:** exact text revisions demonstrably published before the
  declared cutoff. Keep publication evidence and confidence separate from the
  human label. Old publication dates do not prove unaided composition.
- **Uncertain:** modern prose without adequate production records, ambiguous
  translations, or text whose revision history cannot support the proposed label.
  Keep it out of the definitive human-only evaluation set.

Never use Pangram or the reference detector to certify a human-origin training
label. Their observations belong in evaluation records, separate from production
evidence. Retain author, coauthor, translator, editor and speaker roles separately;
an account or byline does not always identify the person who wrote a passage.

## Priority human-text sources

| Source | Rights and acquisition | Admission work and intended contribution |
| --- | --- | --- |
| **Global Voices original English prose** | The publisher defaults to CC BY, with commercial adaptation allowed and item exceptions; its current footer identifies CC BY 3.0. Preserve writer credit, original URL, exact license and extraction changes. Use publisher archives/API with the applicable robots policy and bounded request rate. [Republishing policy](https://globalvoices.org/about/global-voices-attribution-policy/). | First source for named writers and reported commentary across countries. Review article-origin notices, cross-posts, quotations and translation credits. The [existing screen](../../docs/global-voices-transfer.md) contains 168 selected articles from 14 provisional accounts, spanning 2014-2025; it does not establish human-only labels. Expand beyond that sample. The [publisher AI policy](https://globalvoices.org/about/global-voices-policy-on-ai/) supplies editorial context, not per-item production proof. |
| **PLOS articles and the PMC Open Access subset** | Allowlist exact article versions carrying CC0 or CC BY. PLOS's default allows broad reuse unless indicated otherwise. PMC requires per-version license checks; use its official cloud inventory, metadata and XML/text objects. [PLOS terms](https://plos.org/terms-of-use/), [PMC subset](https://pmc.ncbi.nlm.nih.gov/tools/openftlist/), [current PMC access](https://pmc.ncbi.nlm.nih.gov/tools/pmcaws/). | Large acquisition candidate for scientific explanation and argumentation. Preserve DOI/PMCID, version, publication date, author list and article-level license. Exclude NC, ND and unresolved exceptions from the first cohort. Group coauthored articles by work and related source families; do not assign every passage to the first author. Cap scientific prose so register cannot substitute for origin classification. |
| **PLOS blogs with CC BY text** | Default blog text uses CC BY 4.0, with individual blogger and PLOS attribution. Some independent archived blogs retain CC BY-NC. [Official blog licensing](https://plos.org/blogs/about/). | Adds less formal scientific commentary. Check each blog/post notice; exclude NC archives unless a separate commercial grant is obtained. Distinguish personal authors from organizational bylines and Q&A speakers. Media and reader comments need their own rights treatment. |
| **Historical English Wikinews** | Text from September 25, 2005 through December 15, 2024 generally uses CC BY 2.5; newer text uses CC BY 4.0. Earlier text is public domain. Preserve applicable item exceptions and attribution. [Copyright policy](https://en.wikinews.org/wiki/Wikinews:Copyright). | Adds contemporary reporting. Select exact historical revisions for proxy labels. Preserve contributor history and remove or separately qualify externally quoted passages. Treat collective news editing as a source-group attribute rather than individual-author ground truth. |
| **Filtered U.S. government prose** | Select work demonstrably written by federal employees as part of official duties. GovInfo warns that government publications can incorporate independently copyrighted material. [GovInfo rights policy](https://www.govinfo.gov/about/policies), [developer access](https://www.govinfo.gov/developers). | Use agency explanations and filtered Congressional Record passages to add administrative and argumentative registers. The [daily Record](https://www.govinfo.gov/help/crec) has text and metadata from 1994 onward. Exclude inserted outside articles, unsupported contractor material and other rights exceptions. Speaker names do not establish sole authorship; staff-written speeches remain a separate attribution category. Record relevant jurisdiction for public-domain determinations. |

PMC's current cloud layout no longer separates commercial and noncommercial
articles into directories. Select by per-version JSON metadata and save an
inventory snapshot; cloud modification time may reflect processing rather than
text composition. Use only its documented automated retrieval methods.
[Current PMC retrieval details](https://pmc.ncbi.nlm.nih.gov/tools/pmcaws/).

## Secondary human-text sources

| Source | Disposition | Conditions |
| --- | --- | --- |
| **Wikipedia historical revisions** | Secondary source, conditional on the release plan for CC BY-SA text. | Broad topics with collaborative authorship. Preserve revision URLs, contributor histories, imported-text notices and exact license versions. Use page/revision families for splitting; the last editor is not the author. [Wikimedia terms](https://foundation.wikimedia.org/wiki/Policy:Terms_of_Use#7._Licensing_of_Content). |
| **Stack Exchange, including nonprogramming sites** | Not acquired. | Public contributions use CC BY-SA 2.5/3.0/4.0 according to revision date. Current official dump access includes conditions concerning LLM-training projects; acquisition needs a recorded access route and terms record. Preserve account IDs, thread/revision groups and licenses; remove copied documentation and code from prose examples. [Content licensing](https://stackoverflow.com/help/licensing), [official dump-access change](https://meta.stackexchange.com/questions/401324/announcing-a-change-to-the-data-dump-process). |
| **Selected Wikisource works** | Later supplementary source. | Use work-level public-domain/free-license notices, edition and translation metadata. Original authors, translators and transcribers are different roles. Much available writing predates the intended usage registers, so older works need their own evaluation slice. [Copyright policy](https://en.wikisource.org/wiki/Wikisource:Copyright_policy). |

For Wikimedia projects, use the official monthly history/current XML exports
and their completed `SHA256SUMS` manifests. Legacy XML production is deprecated.
History exports allow actual earlier text selection; page-creation dates alone
cannot date later rewritten text. [Export specification](https://wikitech.wikimedia.org/wiki/MediaWiki_Content_File_Exports).

## Existing detection and collaborative-writing datasets

Use eligible subsets as supplements after admission. Published benchmark size is
not the amount qualified for this project. Older generators and copied source
families also limit their value as unseen final evaluation data.

| Candidate | Screened disposition | Required next check |
| --- | --- | --- |
| **HC3 `wiki_csai`** | Small paired seed candidate under CC BY-SA. The authors explicitly identify Wikipedia as this subset's source. | Recover original pages/revisions and attribution; preserve question/answer families and generator provenance. The broader HC3 release contains custom, unknown and noncommercial upstream terms and needs subset-specific treatment. [Authors' copyright table](https://github.com/Hello-SimpleAI/chatgpt-comparison-detection#dataset-copyright). |
| **RAID `books`** | Promising paired supplement after a source join. Its source is the CMU collection of Wikipedia book summaries, licensed CC BY-SA. | Match exact summary records and carry their license/attribution into generated and attacked descendants. Book author metadata identifies the author of the book, not the Wikipedia summary writer. [CMU source license](https://www.cs.cmu.edu/~dbamman/booksummaries.html), [RAID source construction](https://aclanthology.org/2024.acl-long.674.pdf). |
| **RAID, other domains** | Not admitted as a full corpus. Its MIT release includes multiple upstream text collections. | Audit source domains separately; admission needs a per-source licence record. Preserve `source_id`, `adv_source_id`, model, decoding and attack metadata; every descendant shares its source partition. [Official dataset card](https://huggingface.co/datasets/liamdugan/raid). |
| **MAGE; M4 / SemEval-2024 Task 8** | Not imported in bulk. Admission needs per-source identity reconstruction. | Preserve source-family and generator lineage beyond class labels. Admission needs a per-source licence record. Human-prefix/model-continuation examples describe that specific workflow. [MAGE](https://github.com/yafuly/MAGE), [M4](https://github.com/mbzuai-nlp/M4), [SemEval release](https://github.com/mbzuai-nlp/SemEval2024-task8). |
| **Ghostbuster** | Not admitted. Its repository is CC BY 3.0. | Admission needs a per-source licence record for the incorporated IvyPanda, Reuters and WritingPrompts text. [Actual license](https://github.com/vivek3141/ghostbuster/blob/master/LICENSE), [construction paper](https://arxiv.org/html/2305.15047v3). |
| **GPT-wiki-intro** | Not admitted; admission needs a complete licence record for the synthetic release. | The card's `cc` label does not identify a complete license. Preserve Wikipedia provenance and the human prefix used in generation. This uses an older GPT Curie model. [Original card](https://huggingface.co/datasets/aadityaubhat/GPT-wiki-intro). |
| **CoAuthor** | Useful design reference for real collaborative editing; the writing sessions are not admitted. | The official site documents 1,445 writing sessions with 63 writers, GPT-3 interactions and keystroke records. Its interface code and downloadable writing sessions are distinct artifacts. Admission needs a licence record for the sessions themselves, separate from the interface's software license. [Official dataset site](https://coauthor.stanford.edu/). |

For any repackaged detection collection, trace incorporated records back to their
upstream licenses and source identities. A permissive collection label can be
accurate about its own contribution while leaving additional upstream conditions
to satisfy. Prefer recording new model generations on admitted source families
when reconstructing a legacy corpus would cost more than reproducing its useful
coverage.

## Existing local data excluded from distributable weights

The Blog Authorship research corpus, its fitted profiles/checkpoints, private
correspondence, coursework, book text and derived edits remain excluded from the
reference model's distributable training lineage. Keep their existing experiment
records as historical results. Reusing collector or feature-extraction code does
not require reusing its training data or fitted vocabulary/normalization.

The existing Global Voices transfer experiment evaluated Blog-trained weights.
Qualifying new Global Voices records does not qualify those old weights for
release; refit every learned component from the admitted training partition.
[Existing experiment limitations](../../docs/global-voices-transfer.md).

## Acquisition stages and gaps

These are planning targets, not measured available volumes or training claims:

1. Review up to 100 records from each priority source before scale. Record rights
   yield, usable dates, attribution quality, quote/translation rates, duplicate
   rates and register. Pause a source when its failures require a different
   extraction or permission policy.
2. Aim for a pilot of up to 10,000 distinct original source works across at least
   four registers, mostly historical human candidates with matched generated
   descendants. Count works separately from excerpts and model variants. The
   author component follows the whitepaper target of 50 writers with twelve
   works and two documented edits each. Strongly labeled mixed examples require
   documented human contributions through commissioned or opt-in workflows.
   The final mixture follows measured eligible yield and split requirements.
3. Expand toward 50,000-100,000 source works after the pilot exposes source and
   date shortcuts. For author verification, separately seek at least 300
   qualified writers with eight distinct works each; author coverage is an
   acquisition goal requiring its own yield report.

The shortlist still lacks enough ordinary personal writing: informal blogs,
letters, personal essays, workplace explanations and messages. Fill those gaps
through explicit contributor grants or separately qualified permissive sources.
Commissioned writing can provide both documented human production and held-out
evaluation examples. Scope grants for detector training, model release, public
text distribution and adversarial rewriting separately. Customer inference text
does not enter training by default.

For generated positives, use admitted source works and permitted models/services,
with exact prompts, revisions, generator versions and sampling settings. Keep
source-conditioned rewrites distinct from original generation. Human editing of
generated text needs its own production record and permission scope. Keep
synthetic splices and algorithmic perturbations as separately labeled stress
cases; they do not establish genuine collaborative editing. All descendants of
a source work remain in the same data partition. Training can include explicitly
marked weak and strong label lanes; definitive provenance claims and human-only
false-positive measurements use the strong lane.

## Required admission record

Record original URL and immutable ID/version; raw and extracted text hashes;
exact license and captured rights notice; attribution; acquisition method/terms;
permitted uses; publication, revision and retrieval times; origin evidence and
confidence; writer/coauthor/translator/editor roles; work/thread/revision and
duplicate groups; extraction changes; and the assigned partition.

Every admitted batch needs aggregate counts by source, license, label basis,
register, length, date and author qualification. Keep rejected records and reasons
in local acquisition records. Any public manifest must omit private identifying
records and must itself fit the source's permitted redistribution scope.
