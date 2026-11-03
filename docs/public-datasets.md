# Public sources for the next corpus

Use **FineWeb's October 2021 crawl** for a first broad English frequency reference. We have collected 100 documents through the official Hugging Face dataset server, preserving source URLs, crawl metadata, response hashes and the exact dataset revision. They contain 93,458 whitespace-delimited words from 94 source hosts. This is a deterministic prefix for testing the collector, not a representative sample or a replacement for the matched abstract pilot.

## What Pangram actually identifies

The [Pangram 4 model card](https://www.pangram.com/research/model-card/pangram-4) names FineWeb and FineWeb2. Both have public releases. Its domain table also names academic writing, Amazon reviews and biomedical papers, but gives no download identifiers for those evaluation slices. We cannot equate a similarly named public dataset with Pangram's exact test set. IMDb is an alternative review corpus, not a named release in this card.

The [linked technical report](https://pangram-public.s3.us-east-1.amazonaws.com/pdf/pangram_4_technical_report.pdf) cites public third-party benchmarks, including DetectionAI and DetectRL, and the PELIC learner corpus. Those can support independent evaluations after checking their provenance and permitted uses. The availability of these sources does not imply that Pangram released its training corpus, selected evaluation rows or current model generations.

## Available sources

| Source | Access and time evidence | Fit and rights |
| --- | --- | --- |
| [FineWeb](https://huggingface.co/datasets/HuggingFaceFW/fineweb) | Native Parquet, crawl-specific configs, row-level capture dates. We verified `CC-MAIN-2021-43`. | English web baseline. Database: ODC-By 1.0; Common Crawl terms also apply. Individual source rights remain separate. |
| [FineWeb2](https://huggingface.co/datasets/HuggingFaceFW/fineweb-2) | Parquet grouped by language/script, with row-level crawl dates. Filter each row before 2022. | Multilingual extension. The card directs English users to original FineWeb; there is no `eng_Latn` config in the inspected release. Same database-license distinction. |
| [PLOS publisher abstracts](https://api.plos.org/solr/search-fields/) | Search by publication date, then fetch article XML. Our existing manifest pins 12 articles from 2018. | Scientific prose with article-specific CC BY 4.0 verified. These are our samples, not an identified Pangram benchmark release. |
| [Stanford IMDb](https://ai.stanford.edu/~amaas/data/sentiment/) | Original 2011 archive and publisher-linked Hugging Face Parquet mirror. The HF rows contain text and sentiment labels, without individual dates. | Historical review-language reference. The [HF card](https://huggingface.co/datasets/stanfordnlp/imdb/blob/main/README.md) lists license `other`; it is not admitted. |
| [Multilingual Amazon Reviews](https://registry.opendata.aws/amazon-reviews-ml/) | Reviews collected during 2015–2019; Amazon now marks hosting deprecated. | The [data license](https://github.com/awslabs/open-data-docs/blob/main/docs/amazon-reviews-ml/license.txt) limits use to academic research and excludes commercial research and republication. It is unsuitable as our default commercial-data source. |
| [DetectionAI](https://github.com/brianjabarian/DetectionAI) | Actual original-corpus JSON and generated-output JSON files exist in the authors' repository. The authors describe the human sources as pre-2020. | Useful independent benchmark candidate. The project is MIT-licensed; its novels, reviews and articles come from other sources, and admission needs a per-source licence record. |
| [DetectRL](https://github.com/NLP2CT/DetectRL) | Actual benchmark JSON files are available, including direct prompts and domain-specific tests. | Useful perturbation evaluation candidate. No repository-wide data license was found in the inspected tree; admission needs a data licence record. |
| [PELIC](https://github.com/ELI-Data-Mining-Group/PELIC-dataset) | Learner writing collected in 2005–2012; CSV files through Git LFS. | Helpful for variation in human English, but the dataset specifies CC BY-NC-ND 4.0. Keep its constraints separate from unrestricted training or commercial rewriting data. |

## The working FineWeb collector

`src/public_datasets.rs` implements `fineweb_sample(out, rows)`. It requests at most 100 rows per page and accepts 1–1,000 source rows per invocation. The current sample is in `data/public-datasets/fineweb-2021-43-pilot/human.jsonl`; adjacent files retain the raw response, retrieval provenance and collection summary. The [machine-readable catalog](../experiments/public-datasets/catalog.json) records verified endpoints and revisions.

The collector checks the server's `x-revision` against `9bb295ddab0e05d785b879661af7260fed5140fc`, requires the selected crawl, an English label and capture date before 2022, and rejects missing or truncated text. If present, language confidence must be at least 0.65. It preserves text bytes and records duplicate exclusions without replacing rows. All samples use the training split and are grouped by source hostname, with a leading `www.` removed. This is not registrable-domain grouping.

Historical capture is evidence about when text existed. It does not verify human authorship, prose quality or absence from a model's training data. Boilerplate and topic differences still need attention before comparing this web profile with scientific model outputs. The metadata states the inferred authorship basis explicitly.

The [dataset-server rows API](https://huggingface.co/docs/dataset-viewer/rows) supports small slices without downloading the corpus. For later scaling, the [Parquet API](https://huggingface.co/docs/dataset-viewer/parquet) returns shard URLs. We verified a revision-pinned native shard URL with HTTP byte-range support; its full size is about 2.15 GB, so the current collector uses row slices. It does not download that shard.

The FineWeb database license requires attribution for relevant reuse and distinguishes database rights from rights in individual contents. It also excludes the programs used to build the database. Keep dataset notices, source URLs, code licenses and model-weight terms separately. See [ODC-By 1.0](https://opendatacommons.org/licenses/by/1-0/) and [Common Crawl's terms](https://commoncrawl.org/terms-of-use).
