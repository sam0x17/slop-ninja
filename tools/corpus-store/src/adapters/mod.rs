//! Source adapters. Each turns the lines of its inputs into records, events,
//! observations, views and split memberships. Archiving of the exact bytes is
//! done by the runner for every input regardless of adapter.

pub mod frontier;
pub mod generic;
pub mod local;
pub mod pangram;
pub mod predictions;
pub mod util;

use crate::archive::Line;
use crate::plan::SourceSpec;
use crate::runner::Input;
use crate::sink::Sink;
use anyhow::{Result, bail};

pub trait Adapter {
    fn line(&mut self, sink: &mut Sink, input: &Input, line: &Line) -> Result<()>;
    /// Runs inside one transaction after each input file.
    fn end_file(&mut self, _sink: &mut Sink, _input: &Input) -> Result<()> {
        Ok(())
    }
    /// Runs inside one transaction after every input.
    fn finish(&mut self, _sink: &mut Sink) -> Result<()> {
        Ok(())
    }
    /// True when the adapter joins across files and must see every input again
    /// after an interruption.
    fn needs_all_files(&self) -> bool {
        false
    }
}

/// Archive only: the bytes are kept and nothing is interpreted.
pub struct ArchiveOnly;
impl Adapter for ArchiveOnly {
    fn line(&mut self, _: &mut Sink, _: &Input, _: &Line) -> Result<()> {
        Ok(())
    }
}

pub fn version(name: &str) -> Result<i64> {
    Ok(match name {
        "archive" => 1,
        "generic" => 1,
        "frontier_wave" => 1,
        "frontier_export" => 1,
        "local_journal" => 1,
        "local_records" => 1,
        "pangram" => 1,
        "predictions" => 1,
        x => bail!("unknown adapter {x}"),
    })
}

pub fn make(name: &str, spec: &SourceSpec) -> Result<Box<dyn Adapter>> {
    let p = &spec.params;
    Ok(match name {
        "archive" => Box::new(ArchiveOnly),
        "generic" => Box::new(generic::Generic::new(p)?),
        "frontier_wave" => Box::new(frontier::Wave::new(p)?),
        "frontier_export" => Box::new(frontier::Export::new(p)?),
        "local_journal" => Box::new(local::Journal::new(p)?),
        "local_records" => Box::new(local::Records::new(p)?),
        "pangram" => Box::new(pangram::Pangram::new(p)?),
        "predictions" => Box::new(predictions::Predictions::new(p)?),
        x => bail!("unknown adapter {x}"),
    })
}
