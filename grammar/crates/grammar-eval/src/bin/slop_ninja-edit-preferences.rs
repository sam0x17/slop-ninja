//! Conditional grammatical-choice prediction and synthetic edit applications.
#[path = "../claim_guard.rs"]
pub mod claim_guard;
#[path = "../conditional_edit_preferences.rs"]
mod conditional_edit_preferences;
#[path = "../edit_choice_observation.rs"]
pub mod edit_choice_observation;
#[path = "../edit_preference_model.rs"]
pub mod edit_preference_model;
#[path = "../lexical_context.rs"]
pub mod lexical_context;
#[path = "../predicate_operators.rs"]
pub mod predicate_operators;

fn main() -> anyhow::Result<()> {
    conditional_edit_preferences::main()
}
