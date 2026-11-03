//! Source-aligned claim checks and a frozen synthetic evaluation.
#[path = "../claim_guard.rs"]
pub mod claim_guard;
#[path = "../claim_guard_evaluation.rs"]
mod claim_guard_evaluation;
#[path = "../edit_utility_scoring.rs"]
mod edit_utility_scoring;
#[path = "../lexical_context.rs"]
pub mod lexical_context;
#[path = "../predicate_operators.rs"]
pub mod predicate_operators;

fn main() -> anyhow::Result<()> {
    claim_guard_evaluation::main()
}
