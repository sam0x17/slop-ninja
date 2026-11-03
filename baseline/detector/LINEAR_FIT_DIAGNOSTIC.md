# Linear control fit diagnostic

The first grammar and combined controls selected epoch zero because their
predictions became overconfident on development data. A diagnostic replay on
2026-09-11 reproduced that selection. Reducing the learning rate from 0.03 to
0.001 allowed both controls to improve development log loss. This motivates a
new controlled experiment; it does not supply new test results or a replacement
reference artifact.

The replay used the original 333 training and 36 development rows, frozen
vocabularies and RMS scales. It loaded no calibration or test rows. NumPy FP64
reproduced the full-batch Adam updates, L2 penalty and stopping rule, with a
potential difference in floating-point reduction order from Rust. All three
original selected epochs matched, and the word control's first development loss
matched the released value to floating-point precision.

| Control | Original first-step development loss | Original selected epoch | Smaller-step selected epoch | Smaller-step development loss |
| --- | ---: | ---: | ---: | ---: |
| Word | 0.6502 | 1 | 44 | 0.5610 |
| Grammar | 3.8036 | 0 | 7 | 0.8412 |
| Combined | 2.8381 | 0 | 9 | 0.6322 |

The uniform predictor has loss 1.0986. With the original step size, the grammar
control reached 94.6% training accuracy after one update and 58.3% development
accuracy, but its development loss rose to 3.8036. It later fit every training
label while development loss increased further. The combined control showed the
same pattern. The selection rule correctly retained the uniform checkpoint.

Each control has up to 8,192 coordinates. RMS scaling leaves nonzero coordinates
uncentered, and the first Adam update changes many coefficients together. The
smaller-step replay reduced early confidence and development loss while keeping
the coordinates and other settings fixed. These observations support excessive
confidence and overfitting as causes of the failed recipe. They do not establish
the best optimizer, learning rate or normalization.

The smaller-step run was a follow-up diagnosis on already used development
data. It is not a prespecified generalization comparison. The combined control
also has fewer lexical coordinates than the word control, so these results
cannot isolate the value of adding grammar. A future comparison should retain
per-epoch losses, hold lexical coverage fixed and use fresh confirmation data.
The separately frozen style-diverse encoder pilot keeps its original protocol.

The [aggregate replay records](results/linear-fit-diagnostic-v1.json) retain
input and artifact hashes, the two recipes and per-epoch metrics. Raw feature
rows remain local. The replay created no deployable model and changed no
released weights or calibration.
