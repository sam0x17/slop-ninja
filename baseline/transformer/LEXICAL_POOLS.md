# Word-choice pools

`lexical prepare` generates exact word substitutions from Princeton WordNet 3.0
and measures the relevant word frequencies in the source and reference works.
Rust reads the dictionary, enumerates choices and computes statistics. spaCy
supplies annotations. The LLM can then inspect meanings and select nomination
IDs without writing a replacement passage.

Start from an existing B menu plan, whose source, intent, claims, references and
detector identity remain the bindings for the new pool:

```sh
slop_ninja_transformer lexical prepare \
  --plan /path/to/menu/plan.json \
  --dictionary /path/to/WordNet-3.0 \
  --python /path/to/spacy/python --max-candidates 256 \
  --out /path/to/new-pool
slop_ninja_transformer lexical import --pool /path/to/new-pool/pool.json \
  --choice-id NOMINATION_ID --choice-id ANOTHER_ID \
  --max-depth 2 --max-states 4 --out /path/to/new-input.json
slop_ninja_transformer menu prepare --input /path/to/new-input.json \
  --python /path/to/spacy/python --out /path/to/new-menu
```

The importer replaces the choice list with the specified hypotheses and retains
the source and all other task bindings. It does not accept a revised text. For
a continuing walk, the ordinary parent-selection validation still applies.
Every nomination has an exact `start_byte`, so repeated words can be addressed
individually. The menu checks those UTF-8 spans and reports incompatible edits.

Each nomination records its WordNet sense IDs and glosses, parsed POS/dependency
context, and per-reference word counts with denominators. The target frequency
is the mean of the per-work frequencies; a longer sample does not receive more
weight. Context diagnostics count words under the same tag, dependency relation
and head POS, retaining each work's opportunity count, including zero.

For ranking, compute the change in squared Hellinger distance between the source
word distribution and that reference target after one substitution. This is
only the unigram component. Full word and grammar vectors, actual detector
effects and editorial judgments are measured later through the existing menu.
Keep these quantities separate. A frequent reference word can still be the
wrong word for the argument.

Enumeration takes turns across source positions. Within each position, rank by
unigram improvement, breaking ties by nomination ID. The pool reports how many
positions and alternatives were available before the explicit 1-512 limit.
Several applicable senses for the same replacement remain together in its
evidence. The generator follows membership within a synset; it does not traverse
antonym or broader related-word links.

Repeat `--at-byte POSITION` to request alternatives at particular token starts.
This lets the selector explore the full sense inventory for a few words after
reviewing the brief or measured edit effects. Duplicate or nonexistent token
positions fail. Positions without eligible alternatives remain distinguishable
from the reported positions with alternatives. Without this option, enumerate
across the complete source. Broad dictionary membership can include irrelevant
senses or register changes; a high reference-frequency gain does not resolve them.

This first vocabulary uses lowercase ASCII base forms with NN, VB, VBP, JJ or RB
tags. It generates no inflections, multiword phrases or proper names. WordNet's
attributive and predicative adjective markers constrain their observed positions;
immediately postnominal forms are skipped. Recognized quotation/code delimiters
in the sentence are skipped as an initial surface check. These restrictions limit
coverage and do not prove syntactic or semantic substitutability. No word-sense
disambiguator or learned conditional lexical model is implemented. Review
predicate frames, technical terms, scope, register and context explicitly.

Use the official [WordNet 3.0 archive](https://wordnetcode.princeton.edu/3.0/WordNet-3.0.tar.gz).
The command verifies pinned hashes for `LICENSE` and the four `dict/data.*`
files, copies the license and embeds its notices with the extracted evidence.
Preserve those notices when moving or distributing pools. Princeton documents
the [database format](https://wordnet.princeton.edu/documentation/wndb5wn) and
[license terms](https://wordnet.princeton.edu/license-and-commercial-use).
The dictionary is an optional local resource, not bundled model weights or
evidence about an author's writing. No network or detector calls occur in
either lexical command. Run parsing and experiments on the Studio.
