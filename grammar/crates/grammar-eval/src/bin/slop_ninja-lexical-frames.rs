//! Inspect anchored lexical frames from a pinned VerbNet resource.
#[path = "../lexical_context.rs"]
pub mod lexical_context;
#[path = "../lexical_frame_observation.rs"]
pub mod lexical_frame_observation;
#[path = "../lexical_frame_study.rs"]
mod lexical_frame_study;
#[path = "../predicate_operators.rs"]
pub mod predicate_operators;
#[path = "../verbnet_resource.rs"]
pub mod verbnet_resource;

fn main() -> anyhow::Result<()> {
    lexical_frame_study::main()
}
