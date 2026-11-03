//! Compare versioned lexical-frame mappings on fixed annotations.
#[path = "../lexical_context.rs"]
pub mod lexical_context;
#[path = "../lexical_frame_dative_study.rs"]
mod lexical_frame_dative_study;
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
    lexical_frame_dative_study::main()
}
