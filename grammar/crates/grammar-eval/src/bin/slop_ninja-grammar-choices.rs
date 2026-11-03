//! Local grammar-choice measurements and synthetic edit-utility checks.

#[path = "../function_word_roles.rs"]
pub mod function_word_roles;
#[path = "../grammar_choice_audit.rs"]
mod grammar_choice_audit;
#[path = "../lexical_context.rs"]
pub mod lexical_context;
#[path = "../predicate_operators.rs"]
pub mod predicate_operators;

fn main() -> anyhow::Result<()> {
    grammar_choice_audit::main()
}
