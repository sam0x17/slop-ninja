//! Measure paired availability of period and semicolon edits.
#[path = "../boundary_choice_observation.rs"]
pub mod boundary_choice_observation;
#[path = "../boundary_eligibility.rs"]
mod boundary_eligibility;
#[path = "../claim_guard.rs"]
pub mod claim_guard;
#[path = "../lexical_context.rs"]
pub mod lexical_context;
#[path = "../predicate_operators.rs"]
pub mod predicate_operators;

fn main() -> anyhow::Result<()> {
    boundary_eligibility::main()
}
