//! Repeat identical full texts under fixed parser process and order conditions.
#[path = "../lexical_context.rs"]
pub mod lexical_context;
#[path = "../parser_repeat_observation.rs"]
pub mod parser_repeat_observation;
#[path = "../parser_repeat_study.rs"]
mod parser_repeat_study;
#[path = "../verbnet_resource.rs"]
pub mod verbnet_resource;

fn main() -> anyhow::Result<()> {
    parser_repeat_study::main()
}
