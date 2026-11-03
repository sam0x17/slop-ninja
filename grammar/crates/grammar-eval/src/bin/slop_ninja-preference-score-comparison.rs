//! Compare author proximity and empirical choice preferences on exact edits.
#[path = "../claim_guard.rs"]
pub mod claim_guard;
#[path = "../edit_choice_observation.rs"]
pub mod edit_choice_observation;
#[path = "../edit_preference_model.rs"]
pub mod edit_preference_model;
#[path = "../edit_utility_scoring.rs"]
mod edit_utility_scoring;
#[path = "../lexical_context.rs"]
pub mod lexical_context;
#[path = "../predicate_operators.rs"]
pub mod predicate_operators;
#[path = "../preference_score_comparison.rs"]
mod preference_score_comparison;

fn main() -> anyhow::Result<()> {
    preference_score_comparison::main()
}
