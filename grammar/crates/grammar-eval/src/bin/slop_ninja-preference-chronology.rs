//! Fixed later-date conditional-choice evaluation.
#[path = "../edit_choice_observation.rs"]
pub mod edit_choice_observation;
#[path = "../edit_preference_model.rs"]
pub mod edit_preference_model;
#[path = "../preference_chronology.rs"]
mod preference_chronology;
#[path = "../preference_chronology_evaluation.rs"]
pub mod preference_chronology_evaluation;

fn main() -> anyhow::Result<()> {
    preference_chronology::main()
}
