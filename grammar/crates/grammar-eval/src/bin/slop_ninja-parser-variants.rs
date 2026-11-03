//! Compare fixed parser environments on bound synthetic boundary fixtures.
#[path = "../boundary_choice_observation.rs"]
pub mod boundary_choice_observation;
#[path = "../claim_guard.rs"]
pub mod claim_guard;
#[path = "../lexical_context.rs"]
pub mod lexical_context;
#[path = "../parser_variant_probe.rs"]
mod parser_variant_probe;
#[path = "../predicate_operators.rs"]
pub mod predicate_operators;

fn main() -> anyhow::Result<()> {
    parser_variant_probe::main()
}
