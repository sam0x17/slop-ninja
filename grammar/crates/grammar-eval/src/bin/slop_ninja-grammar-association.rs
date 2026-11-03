#[path = "../association_projection.rs"]
mod association_projection;
#[path = "../association_retrieval.rs"]
mod association_retrieval;
#[allow(dead_code)]
#[path = "../function_word_roles.rs"]
mod function_word_roles;
#[path = "../grammar_association.rs"]
mod grammar_association;
#[allow(dead_code)]
#[path = "../lexical_context.rs"]
mod lexical_context;
#[allow(dead_code)]
#[path = "../predicate_operators.rs"]
mod predicate_operators;

fn main() -> anyhow::Result<()> {
    grammar_association::main()
}
