//! Audit dative construction opportunities in bound author-writing samples.
#[path = "../dative_alternation.rs"]
pub mod dative_alternation;
#[path = "../dative_corpus_observation.rs"]
pub mod dative_corpus_observation;
#[path = "../dative_corpus_study.rs"]
mod dative_corpus_study;
#[path = "../dative_corpus_support.rs"]
pub mod dative_corpus_support;
#[path = "../lexical_context.rs"]
pub mod lexical_context;
#[path = "../lexical_frame_observation.rs"]
pub mod lexical_frame_observation;
#[allow(dead_code)]
#[path = "../lexical_frame_study.rs"]
mod lexical_frame_study;
#[path = "../predicate_operators.rs"]
pub mod predicate_operators;
#[path = "../verbnet_resource.rs"]
pub mod verbnet_resource;

fn main() -> anyhow::Result<()> {
    dative_corpus_study::main()
}
