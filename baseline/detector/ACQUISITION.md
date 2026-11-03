# First detector source acquisition

Collected on September 11, 2026 UTC. The Rust collector admitted 160 distinct
source works, with 180-499 whitespace-delimited words per excerpt. All labels
use the `historical_proxy` evidence category. These are weak human examples for
the initial experiment; none has a documented unaided composition workflow.

| Source and register | Screened | Admitted | Text license |
| --- | ---: | ---: | --- |
| PLOS ONE research abstracts | 150 | 80 | CC BY 4.0 |
| English Wikinews reporting | 242 | 80 | CC BY 2.5 |

PLOS admission requires the article's own JATS license notice, credited authors,
DOI, English research-article type and a 2019 publication date. The publisher
supplied current XML without an immutable historical revision identifier. We
retained its exact bytes and SHA256, and explicitly leave historical byte identity
unverified. [PLOS publication terms](https://plos.org/terms-of-use/).

For Wikinews, the collector selects the latest available revision before 2022,
then requests that exact revision's rendered HTML. The admitted revision dates
range from June 3, 2007 to August 16, 2021. Those are revision timestamps, not
inferred publication dates. The publisher permits attribution to Wikinews and
assigns CC BY 2.5 to text from that period.
[Wikinews copyright policy](https://en.wikinews.org/wiki/Wikinews:Copyright).

Both licenses permit commercial reuse and adaptation with attribution. Each
record carries the source, applicable license, credit and extraction changes.
The collector excludes unsupported rights notices and potential external
quotations; it does not apply the page's license to its images or external source
documents. [CC BY 4.0](https://creativecommons.org/licenses/by/4.0/),
[CC BY 2.5](https://creativecommons.org/licenses/by/2.5/).

## Extraction and exclusions

PLOS extraction keeps complete abstract paragraphs and omits section headings.
It rejects the whole abstract when it contains unsupported formula, superscript,
citation, quotation or additional rights markup. Wikinews extraction uses direct
article-body paragraphs before the first subsection, omits marked publisher
datelines and hidden metadata, and excludes possible quotations and references.
Each retained or excluded news paragraph has an extraction decision and hash.
The word limit applies to complete paragraphs; sentences are not truncated.

| Exclusion reason | PLOS | Wikinews |
| --- | ---: | ---: |
| Possible external quotation | 19 | Included in excerpt-length failures |
| Unsupported abstract markup | 28 | 0 |
| Outside the declared excerpt-length range | 15 | 127 |
| No explicit admitted CC BY notice | 7 | 0 |
| Not a research article | 1 | 0 |
| Copyright or provenance exception template | 0 | 20 |
| No historical revision | 0 | 5 |
| Not a dated news article | 0 | 10 |

Quote filtering uses explicit markup and punctuation, including paired ASCII
single quotes. It is a conservative screen and cannot establish that every
unmarked passage has been attributed correctly. Spot reads identified publisher
datelines joined into lead paragraphs; the final extraction excludes those
marked nodes and was replayed over all retained responses.

Filtering can produce discontinuous excerpts and missing transitions. Source
selection is deterministic: PLOS discovery sorts DOIs, and Wikinews discovery
starts at titles beginning with A. The collection covers two registers and can
contain related stories or shared authors across different work IDs. These
limits require additional data and source-family review before representative
transfer or false-positive claims. Wikinews template expansion occurs at
retrieval time, although the underlying prose revision is fixed.

## Artifacts and reproduction

The [public source manifest](manifests/acquisition-v1.jsonl) contains attribution,
source versions and hashes without corpus text or absolute local paths. Its
SHA256 is `8c905e637ceb1395531e2e570f4297e885b9f370d3dc38ea88150ef0dbceca1a`.
The source-text records, raw responses, HTTP metadata, license evidence,
extraction decisions and rejected attempts remain under ignored
`data/baseline-detector/acquisition-v1/`.

Run from `baseline/detector/`:

```sh
cargo run --bin acquire_sources -- \
  --output ../../data/baseline-detector/acquisition-v1 \
  --plos 80 --wikinews 80 \
  --manifest manifests/acquisition-v1.jsonl
```

The collector checks cached response hashes and refuses changed configurations
or changed existing record exports. Final replay reused 634 captured responses,
made zero HTTP requests and reproduced all 160 records. Fresh downloads can
differ where a publisher changes current XML or template rendering; the public
manifest comparison then fails instead of claiming the original corpus was
reproduced. New requests are sequential, at most two per second, without retries.

Use `human-records.jsonl` as the dataset input. Individual extraction files also
retain results from development of the filter and do not define the final cohort.
All 160 roots are unsplit in this acquisition export. Freeze their partitions
before generating descendants, as specified in the [pilot protocol](PILOT_PROTOCOL.md).
