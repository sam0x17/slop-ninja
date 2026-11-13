# Wikipedia-derived corpus policy

This policy admits commercially reusable historical Wikipedia excerpts into a
separate corpus lane. It applies to source text, transformations, attribution and
model releases. Code retains its existing license. Existing detector releases
contain no text from this lane and keep their recorded terms.

## Text and attribution

Preserve the source's CC BY-SA 3.0 license on verbatim historical excerpts.
Distribute new prose adaptations under CC BY-SA 4.0. Retain the article title,
revision URL, contributor-history URL, upstream dataset credit, license links,
change notices and any additional imported-text notices. Each excerpt credits
its contributors by its revision URL and contributor-history URL. Additional
attribution notices are kept; a detected notice goes to separate review, not
into an export with the notice removed.
[Wikimedia terms, section 7](https://foundation.wikimedia.org/wiki/Policy:Terms_of_Use#7._Licensing_of_Content).

Keep applicable notices and warranty disclaimers, identify changes, include the
license URI, and add no restrictions on licensed text. Other components of a
collection keep their own licenses.
[CC BY-SA 3.0, sections 3 and 4](https://creativecommons.org/licenses/by-sa/3.0/legalcode.en).
The adaptation license is [CC BY-SA 4.0](https://creativecommons.org/licenses/by-sa/4.0/legalcode.en).

## Source admission

For HC3 `wiki_csai`, bind the exact dataset revision and input hash. Admit only
human-answer text that occurs in full, modulo whitespace, in both historical
wikitext and the rendered lead for the same revision at or before 2022-11-01.
Do not expand templates when checking historical wording. Retain exact HC3 answer
bytes, all source captures and every exclusion locally. Retain HC3 attribution
in addition to Wikipedia contributors.

The first importer accepts 80-500 lexical words, excludes detected copyright,
imported-text and incompatible-license notices, and excludes quoted spans of
eight or more whitespace-delimited words. These rules are conservative screening
rules. They can overexclude and cannot certify that every possible source issue
has been found. Investigate any later notice and remove or correct the affected
records and release before reuse. A source date supplies weak production evidence;
it does not certify human authorship. Wikipedia contributors are not individual
author labels, and HC3's ChatGPT labels do not supply complete generation logs.

Assign connected source-family splits before generation or detector scoring.
Exclude previously used corpus families and exact/normalized duplicates from a
new confirmation pool. Models may have encountered these public sources in
pretraining; a new experiment split does not establish pretraining novelty.

## Model releases

For this lane, SlopNinja chooses to license resulting fine-tuned detector
weights under CC BY-SA 4.0 and include the complete source-attribution manifest.
Preserve the base checkpoint's Apache 2.0 license and
notices separately. Mark the fine-tuning as a modification and identify the
fine-tuned weight license in the model card. Keep runtime code under its own
license. Do not label the entire bundle MIT or Apache 2.0 by copying only the
base-model notice.

Rust exports `data_rights.json` with exact shard hashes. The encoder trainer
requires that manifest when fitting on ShareAlike records and includes it in its
content-hashed artifact. Sparse feature exports retain the same obligations;
their training bundle records the attribution manifest hash in the runtime
contract. Generated descendants retain source credit and get an explicit model
change notice. A model release must include these files and terms.

Source captures and working corpora remain under ignored `data/`. The authorized
[public snapshot](../../datasets/detector-origin-v1/README.md) includes selected
text with record-level attribution, licenses and original input hashes. Pangram
annotations fall outside this ShareAlike policy. Under the project's
publication policy, Pangram scores are kept privately and not published, and
they never become origin labels; Pangram links and ids may be published where
they serve verification.
