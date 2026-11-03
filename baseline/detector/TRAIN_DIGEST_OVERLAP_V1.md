# Digest overlap diagnostic

The fixed human Train register sample includes Wikinews page 1445, a digest of
several article excerpts with dates and editorial review-status labels. A page
ID need not identify all underlying source works in such a compilation.

Compare that exact selected text against every record in the original V6 Train,
Development, Calibration and Test shards. Bind the four original hashes and the
already frozen 16-source review packet. Include model and mixed descendants as
well as human roots; report same-family matches separately. These are existing
opened partitions. Do not inspect reserved fresh confirmation sources.

Use the existing `grammar_core::features::words` normalization: NFC-normalized
Unicode letters with attached combining marks and internal apostrophes. Map the
four supported apostrophe variants to ASCII, lowercase each word and normalize
again. Digits and other separators, including hyphens, separate words. Enumerate
maximal identical contiguous runs
of at least eight normalized words. Preserve both normalized-word ranges and
report all matches. Runs of at least 20 words warrant source-overlap review;
shorter runs remain visible as possible formulaic or title fragments. These
thresholds select review evidence, not a definitive plagiarism or leakage label.
Read any cross-family matches in context before attributing their cause.

Recheck input hashes after processing. Keep counts for every split and origin,
including records with no match. A negative result rules out only these exact
normalized runs against this particular digest and the bound archives. It does
not prove independence of the entire corpus, rule out paraphrased overlap, or
repair the distinction between article and compilation source families.

Run on the Studio without inference or provider calls. Preserve the current
labels, splits, admissions and fits regardless of the result. Record any future
family-grouping or extraction change as a separate data version.
