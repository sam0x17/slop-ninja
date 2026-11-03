//! An additional declared view; retain the original complete-word diagnostics.
use grammar_core::features::{Family, Features};
use std::collections::BTreeMap;

const WORDS: &str = "a an the and or but nor so yet for if unless although though while whereas because since as than that which who whom whose what when where why how this these those it its itself i me my mine myself we us our ours ourselves you your yours yourself yourselves he him his himself she her hers herself they them their theirs themselves am is are was were be being been have has had having do does did doing can could may might must shall should will would not no only also even just still already again here there then now all any both each either every few many more most much neither other same some such another about above after against along among around at before behind below beneath beside between beyond by despite down during except from in inside into near of off on onto out outside over past per through throughout till to toward under until up upon via with within without don't doesn't didn't can't couldn't won't wouldn't isn't aren't wasn't weren't it's that's there's they're we're you're i'll you'll we'll they've we've you've i'd you'd we'd he's she's";

pub fn function_grammar(features: &Features) -> Features {
    let mut out = features.clone();
    let Family::Distribution {
        counts,
        opportunities,
    } = &features.families["word"]
    else {
        unreachable!("Default word family is a distribution")
    };
    let mut selected = BTreeMap::new();
    for (word, count) in counts {
        let key = if WORDS.split_whitespace().any(|w| w == word) {
            word.as_str()
        } else {
            "__other_word__"
        };
        *selected.entry(key.to_owned()).or_insert(0) += count;
    }
    out.families.remove("word");
    out.families.remove("word_bigram");
    out.families.insert(
        "function_word_v1".into(),
        Family::Distribution {
            counts: selected,
            opportunities: *opportunities,
        },
    );
    out.schema.push_str(";b-function-word-residual-v1");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn content_words_share_residual_mass_without_losing_the_denominator() {
        let f = Features {
            schema: "fixture".into(),
            parser_identity: "fixture".into(),
            source_sha256: "fixture".into(),
            families: BTreeMap::from([(
                "word".into(),
                Family::Distribution {
                    counts: BTreeMap::from([
                        ("the".into(), 2),
                        ("bench".into(), 1),
                        ("station".into(), 1),
                    ]),
                    opportunities: 4,
                },
            )]),
        };
        let view = function_grammar(&f);
        assert_eq!(
            view.families["function_word_v1"],
            Family::Distribution {
                counts: BTreeMap::from([("the".into(), 2), ("__other_word__".into(), 2)]),
                opportunities: 4
            }
        );
        assert_ne!(view.schema, f.schema);
        assert!(f.families.contains_key("word"));
    }
}
