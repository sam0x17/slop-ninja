# Training source acquisition

Primary-source review, 2026-09-09. Start with Global Voices for attributed modern
prose and PLOS for factual revision tasks. Build a consented writer/editing
collection alongside them. The first [local Global Voices screen](global-voices-transfer.md)
now covers 168 articles from fourteen provisionally qualified accounts.

## Sources to qualify first

| Source | Published text terms | Intended use | Required source checks |
| --- | --- | --- | --- |
| Global Voices original prose | CC BY 3.0 by default; commercial adaptation allowed | Multiple works per writer, grammar/word distributions, permitted source-conditioned rewrites | Individual writer rather than translator/shared account; original language; third-party quotations; item exceptions |
| PLOS articles | CC BY, commercial reuse allowed; retain each article's exact license | Claims, quantities, citations and qualifications for factual revision | Treat papers as coauthored works; retain work-family grouping and attribution |
| PLOS blogs | Default CC BY 4.0; archived exceptions include CC BY-NC | Additional registers and potentially attributable blog authors | Check the particular blog and post, not just the platform-wide default |
| Commissioned/opt-in writers and editors | Explicit agreement for each intended use | Stronger individual authorship, same-facts/different-writer tasks and reviewed edit preferences | Separate training, private evaluation and redistribution permissions; record collaboration and AI assistance |

Global Voices explicitly permits commercial adaptation and requires credit,
license/modification notices and an original-story link when republishing.
Its directory includes writers, translators and organizations, so byline IDs
are starting metadata rather than verified authorship. Its editorial AI policy
does not prove the production history of every article.
[Republishing policy](https://globalvoices.org/about/global-voices-attribution-policy/),
[contributor directory](https://globalvoices.org/our-people/contributors/),
[AI policy](https://globalvoices.org/about/global-voices-policy-on-ai/).

A bounded metadata check of that English directory found 208 active profile
URLs, including 114 with at least six displayed posts and 86 with at least 12.
It also listed 2,048 inactive profiles without post counts, for 2,256 distinct
profiles overall. Three profile checks found translations, coauthored work and
a generic guest account. These are account counts before eligibility filtering;
at that stage, no article pages or archive pagination had been collected. The
inactive profiles are a discovery pool, not a confirmed supply of 2,048 writers
with enough text.

PLOS allows commercial reuse under attribution terms. Check licenses in article
XML and on individual blogs, excluding noncommercial archives. Our existing
[12-source manifest](../experiments/matched-pilot-v1/human-source-manifest.json)
provides a small starting set with source and extraction hashes. Those abstracts
supply content constraints; they do not identify a single person's writing style.
[Article policy](https://journals.plos.org/plosone/s/licenses-and-copyright),
[blog policy and exceptions](https://plos.org/blogs/about/).

The first bounded acquisition selected twenty accounts before extraction or
scoring; fourteen supplied twelve provisionally eligible articles across at
least four dates. It recorded 840 account–article observations and 820 distinct
works, retaining 456 articles before taking the fixed sample. The
[qualification report](global-voices-transfer.md) records translation/byline,
origin-notice, length and duplicate exclusions. Retain raw text privately for
reproducibility and keep the same article family and its translations in one
split; do not silently join separated spans into new sentences.

Sampling across regions and subjects reduces obvious topic confounding, but
does not remove it. Include same-author/different-topic and different-author/
same-topic evaluations. A contributor directory with hundreds of entries does
not establish hundreds of eligible author profiles.

## Optional or excluded pools

Wikimedia revisions can supply editorial pairs under the applicable CC BY-SA
terms, with revision/parent IDs and attribution retained. An accepted edit can
add facts or introduce an error; it is not automatically an improvement or a
meaning-preserving pair. The last editor did not write the whole article. Keep
this pool separate until its exact downstream release/training use is resolved;
do not infer either blanket permission or a blanket prohibition for private
weights from ShareAlike alone.
[Wikimedia terms](https://foundation.wikimedia.org/wiki/Policy:Terms_of_Use/en),
[revision API](https://www.mediawiki.org/wiki/API:Revisions/en).

Project Gutenberg has useful multi-work authors, but item-level public-domain
status, jurisdiction, translations and trademark conditions need checking. It
is an optional historical transfer test; our initial target remains modern
writing. [Permission guidance](https://www.gutenberg.org/policy/permission.html).

Stack Exchange has versioned CC BY-SA contributions, while its official dump
download announcement includes separate conditions concerning LLM training.
Keep it out of the initial package until the acquisition and training route is
settled. [Text licensing](https://stackoverflow.com/help/licensing),
[download conditions](https://meta.stackexchange.com/questions/401324/announcing-a-change-to-the-data-dump-process).

The Blog Authorship Corpus remains noncommercial local research. We did not
verify an affirmative commercial text license for Amazon Reviews in this pass.
Neither belongs in the initial miner data release. An open loader, a public
download or inclusion in Pangram's evaluation does not settle text rights.

## Manifest and labels

Every source record needs the following before joining a training release:

- Source URL, item/revision/parent IDs, retrieval time and exact raw/text hashes.
- Text license, version, evidence URL and captured evidence hash; attribution,
  exceptions and acquisition terms. Record loader-code licenses separately.
- Author namespace/ID and role: original writer, coauthor, translator, editor
  or organization; publication/composition dates, language and edition links.
- Extraction version, excluded quotation/third-party spans, and author/work/
  revision-family split IDs. Public attribution stays available with released
  text; pseudonymous benchmark IDs do not erase attribution obligations.
- For generated material, parent text, model/tokenizer revision, prompt recipe,
  generation settings and exact output. Mark synthetic supervision explicitly.
- For reviewed edits, the brief, protected facts, reviewer provenance,
  preservation judgment, tone/style ratings and preference or tie.
- Consent or license scope for training, private evaluation and redistribution,
  including whether customer reference writing can be reused at all.

Store counts and denominators in SQLite using the existing Rust corpus tools.
Freeze author and work-family splits before vocabulary selection or model fitting.
Keep raw corpora and private agreements under ignored `data/`; publish source
manifests and aggregate eligibility counts only after checking their release
scope. The [model plan](miner-models-and-data.md) defines how these sources become
author pairs, production-history examples and editorial preference records.
