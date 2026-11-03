# Global Voices historical-source screen

Inspect the saved Global Voices collection as a possible addition to A's human
source coverage. The previous author-transfer experiment used Blog-trained
weights; those weights, fitted vocabulary and normalization remain excluded from
the distributable detector. Reusing eligible publisher text requires a separate
record-level review. No data enters the active A fits through this screen.

Use the unchanged legacy `records.json`, SHA256
`88ba5f33757d72d78a1b43bad01829102b130be3f74bee6d8ac1514a442d02e7`.
Verify the saved response metadata and body hashes, then join every extracted
record to its WordPress response by exact story URL. Check publication and
modification timestamps, content hash and source-text hash. Preserve all 840
account/article observations and count distinct works separately.

Report the old extraction exclusions, publication-year coverage and counts with
both publisher timestamps before 2022-11-01. Invalid timestamps, modifications
before publication and dates after retrieval remain explicit. Report how many
date-screened works passed the old extraction rules and how many appeared in
the previous 168-article author experiment. These are metadata checks, not
historical byte verification, authorship certification or commercial admission.
For the first review packet, require publication no later than modification and
neither timestamp later than retrieval. Keep other orderings for interpretation;
a modification before publication may reflect scheduling and does not by itself
prove invalid metadata. Report raw cutoff counts before applying this condition.

Among distinct works passing the old extraction checks and timestamp screen,
sort SHA256 of the JSON tuple `(slop-ninja-global-voices-source-screen-v1, id)`
with ID as tie-breaker. Select up to eight complete source records for manual
reading before looking at detector scores. Keep every exclusion and do not
replace an inconvenient selected source. Inspect the complete article context,
retained and removed paragraphs, byline/translation roles, item-level notices,
and whether quotation removal damaged the argument.

The [publisher's republishing policy](https://globalvoices.org/about/global-voices-attribution-policy/)
permits commercial adaptation under its CC BY default, with attribution and
exceptions for other material. The footer identifies CC BY 3.0. The
[AI editorial policy](https://globalvoices.org/about/global-voices-policy-on-ai/)
does not prove an individual article's production history. An old WordPress
date is weaker evidence than an independently archived exact historical text.
Any later admission must preserve that distinction and source obligations.

Run all processing on the Studio. This first screen uses saved responses only:
no new article retrieval, model inference, Pangram request, split assignment or
training change. Its selected records and raw text stay outside Git. Publish
only the reproducible code, protocol and aggregate/identity report. Further
source collection or model fitting remains a separate experiment.
