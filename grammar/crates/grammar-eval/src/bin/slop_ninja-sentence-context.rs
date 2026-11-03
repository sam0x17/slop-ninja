//! Compare full-post sentence projections with isolated sentence parses.
#[path = "../dative_alternation.rs"]
pub mod dative_alternation;
#[path = "../lexical_context.rs"]
pub mod lexical_context;
#[path = "../lexical_frame_observation.rs"]
pub mod lexical_frame_observation;
#[path = "../parser_repeat_observation.rs"]
pub mod parser_repeat_observation;
#[path = "../predicate_operators.rs"]
pub mod predicate_operators;
#[path = "../sentence_context_comparison.rs"]
pub mod sentence_context_comparison;
#[path = "../sentence_context_projection.rs"]
pub mod sentence_context_projection;
#[path = "../sentence_context_study.rs"]
mod sentence_context_study;
#[path = "../verbnet_resource.rs"]
pub mod verbnet_resource;

fn main() -> anyhow::Result<()> {
    sentence_context_study::main()
}
