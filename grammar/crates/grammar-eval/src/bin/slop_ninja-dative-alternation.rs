//! Generate and inspect reversible, resource-scoped dative alternatives.
#[path = "../dative_alternation.rs"]
pub mod dative_alternation;
#[path = "../dative_alternation_study.rs"]
mod dative_alternation_study;
#[path = "../dative_vector_scoring.rs"]
pub mod dative_vector_scoring;
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
    dative_alternation_study::main()
}
