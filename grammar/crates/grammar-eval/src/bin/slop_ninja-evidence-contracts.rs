//! Compare versioned structural evidence with exact legacy observations.
#[path = "../dative_alternation.rs"]
pub mod dative_alternation;
#[path = "../evidence_contract_study.rs"]
mod evidence_contract_study;
#[path = "../experiment_io.rs"]
pub mod experiment_io;
#[path = "../lexical_context.rs"]
pub mod lexical_context;
#[path = "../lexical_frame_observation.rs"]
pub mod lexical_frame_observation;
#[path = "../morphology_evidence.rs"]
pub mod morphology_evidence;
#[path = "../orthographic_evidence.rs"]
pub mod orthographic_evidence;
#[path = "../predicate_operators.rs"]
pub mod predicate_operators;
#[path = "../sentence_membership_evidence.rs"]
pub mod sentence_membership_evidence;
#[path = "../structural_evidence.rs"]
pub mod structural_evidence;
#[path = "../verbnet_resource.rs"]
pub mod verbnet_resource;
fn main() -> anyhow::Result<()> {
    evidence_contract_study::main()
}
