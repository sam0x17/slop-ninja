# Component notices

Copyright (c) 2026 Slop Ninja Research, where applicable. Original detector
software uses the MIT license in `LICENSE.code`. Third-party dependencies,
weights and source works retain their recorded terms and warranty disclaimers.

The detector derives from Answer.AI / LightOn's ModernBERT-base, revision
`8949b909ec900327062f0ebf497f51aef5e6f0c8`. The immutable artifact includes its
upstream model card and Apache 2.0 license. Slop Ninja Research added a three-class
classification head, fine-tuned with source-origin loss weighting and raw/whitespace
views, and fitted a Calibration temperature. V6 extends the input limit to 2,048
tokens and adds training examples with successive model revisions. The training,
calibration and protocol records identify these modifications.

Slop Ninja Research distributes the fine-tuned weights under CC BY-SA 4.0 as a
project release choice. Retain the upstream notices and source attribution when
redistributing; see `encoder/LICENSE` and `provenance/SHARE_ALIKE_POLICY.md`.

The CMU Book Summaries collection by David Bamman and Noah Smith (2013),
https://www.cs.cmu.edu/~dbamman/booksummaries.html, is offered under CC BY-SA 3.0
United States (https://creativecommons.org/licenses/by-sa/3.0/us/).
Historical Wikipedia-derived summaries retain CC BY-SA 3.0 Unported terms.
Both grants, CMU extraction credit, Wikipedia contributors, revision links,
contributor-history links and extraction changes are preserved in
`encoder/DATA_RIGHTS.json` and `provenance/source-credits.json`. Authors of the
summarized books are not credited as authors of the Wikipedia summaries.

The source-credit file preserves Wikipedia contributor attribution and HC3
extraction credit to Biyang Guo et al. for encyclopedia sources. PLOS and
historical Wikinews works retain their recorded CC BY 4.0 and CC BY 2.5 terms,
author credits, revision links and extraction notices. Applicable generated
adaptations retain CC BY-SA 4.0 terms and source credit. The attribution manifest
covers 4,903 records from 685 source works in Train, Development and Calibration;
it contains no corpus passages.

Generation metadata records the models, pinned revisions and license evidence
used to produce drafts and revisions. Generator weights are not bundled.
Phi-4 supplied a separate evaluation route and was absent from fitting,
Development and Calibration. The source records preserve each generation and
editing workflow; detector scores do not replace those origin labels.

Fix Slop instructions adapt a versioned prose-only subset of the Fix Slop
project. The adapted text and MIT notice crediting Hardik Pandya are retained
under `provenance/prompts/`. No source author or upstream project is claimed to
endorse this release.

Pangram is a separate evaluation service. This bundle includes aggregate
comparisons, with the underlying provider reports retained outside the bundle.
Pangram observations did not select this checkpoint or change its thresholds.
