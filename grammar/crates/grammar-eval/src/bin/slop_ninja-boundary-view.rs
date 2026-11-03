//! Compare explicit boundary views on frozen synthetic fixtures.
#[path = "../boundary_choice_observation.rs"]
pub mod boundary_choice_observation;
#[path = "../boundary_view.rs"]
pub mod boundary_view;
#[path = "../boundary_view_study.rs"]
mod boundary_view_study;
#[path = "../claim_guard.rs"]
pub mod claim_guard;
#[path = "../lexical_context.rs"]
pub mod lexical_context;
#[path = "../predicate_operators.rs"]
pub mod predicate_operators;

fn main() -> anyhow::Result<()> {
    boundary_view_study::main()
}
