use anyhow::Result;
use clap::{Parser, Subcommand, ValueEnum};
use slop_ninja_detector::{
    acquire, dataset,
    features::{FeatureConfig, FeatureMode},
    model::TrainConfig,
};
use std::path::PathBuf;

mod pipeline;

#[derive(Clone, Copy, ValueEnum)]
enum Mode {
    Word,
    Grammar,
    Combined,
}

impl From<Mode> for FeatureMode {
    fn from(mode: Mode) -> Self {
        match mode {
            Mode::Word => Self::Word,
            Mode::Grammar => Self::Grammar,
            Mode::Combined => Self::Combined,
        }
    }
}

#[derive(Clone, Copy, ValueEnum)]
enum EvaluationSplit {
    Development,
    Calibration,
    Test,
}

impl From<EvaluationSplit> for dataset::Split {
    fn from(split: EvaluationSplit) -> Self {
        match split {
            EvaluationSplit::Development => Self::Development,
            EvaluationSplit::Calibration => Self::Calibration,
            EvaluationSplit::Test => Self::Test,
        }
    }
}

#[derive(Parser)]
#[command(
    name = "slop_ninja_detector",
    about = "Slop Ninja reference detector and admitted-corpus pipeline"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Validate and summarize a provenance-bearing origin-record manifest.
    Audit {
        #[arg(long)]
        input: PathBuf,
    },
    /// Freeze source-family partitions; refuses already assigned records.
    Split {
        #[arg(long)]
        input: PathBuf,
        #[arg(long)]
        output: PathBuf,
        #[arg(long)]
        seed: String,
    },
    /// Collect the bounded PLOS/Wikinews rights and provenance review sample.
    Acquire {
        #[arg(long)]
        output_dir: PathBuf,
        #[arg(long, default_value_t = 100)]
        plos: usize,
        #[arg(long, default_value_t = 100)]
        wikinews: usize,
    },
    /// Export already admitted, frozen records for the separate encoder runner.
    ExportShards {
        #[arg(long)]
        input: PathBuf,
        #[arg(long)]
        output_dir: PathBuf,
    },
    /// Extract text-free feature records; grammar/combined require spaCy.
    Featurize {
        #[arg(long)]
        input: PathBuf,
        #[arg(long)]
        output: PathBuf,
        #[arg(long, value_enum, default_value = "word")]
        mode: Mode,
        #[arg(long)]
        python: Option<String>,
        #[arg(long, default_value_t = 128)]
        batch_size: usize,
    },
    /// Export Train-fitted sparse coordinates for separate neural experiments.
    ExportVectors {
        #[arg(long)]
        features: PathBuf,
        #[arg(long)]
        output_dir: PathBuf,
        #[arg(long, value_enum, default_value = "combined")]
        mode: Mode,
        #[arg(long, default_value_t = 16384)]
        max_coordinates: usize,
        #[arg(long)]
        word_coordinate_cap: Option<usize>,
        #[arg(long, default_value_t = 2)]
        min_document_frequency: usize,
    },
    /// Fit training coordinates/weights, development checkpoint and calibration.
    Train {
        #[arg(long)]
        features: PathBuf,
        #[arg(long, value_enum, default_value = "word")]
        mode: Mode,
        #[arg(long)]
        output_dir: PathBuf,
        #[arg(long, default_value_t = 8192)]
        max_coordinates: usize,
        /// In combined mode, reserve this many coordinates for words/bigrams.
        #[arg(long)]
        word_coordinate_cap: Option<usize>,
        #[arg(long, default_value_t = 2)]
        min_document_frequency: usize,
        #[arg(long, default_value_t = 300)]
        epochs: usize,
        #[arg(long, default_value_t = 0.03)]
        learning_rate: f64,
        #[arg(long, default_value_t = 0.01)]
        l2: f64,
        #[arg(long, default_value_t = 25)]
        patience: usize,
    },
    /// Evaluate frozen weights; final testing requires an explicit opening flag.
    Evaluate {
        #[arg(long)]
        features: PathBuf,
        #[arg(long)]
        artifact: PathBuf,
        #[arg(long)]
        output: PathBuf,
        #[arg(long, value_enum, default_value = "test")]
        split: EvaluationSplit,
        #[arg(long)]
        open_final_test: bool,
    },
    /// Run local inference on exact UTF-8 text, without labels or source metadata.
    Project {
        #[arg(long)]
        feature_space: PathBuf,
        #[arg(long)]
        text_file: PathBuf,
        #[arg(long)]
        python: Option<String>,
    },
    /// Parse request JSONL once and project into compatible frozen spaces.
    ProjectBatch {
        #[arg(long, num_args = 1..)]
        feature_spaces: Vec<PathBuf>,
        #[arg(long)]
        requests: PathBuf,
        #[arg(long)]
        output_dir: PathBuf,
        #[arg(long)]
        python: Option<String>,
        #[arg(long, default_value_t = 128)]
        batch_size: usize,
    },
    /// Run local inference on exact UTF-8 text, without labels or source metadata.
    Infer {
        #[arg(long)]
        artifact: PathBuf,
        #[arg(long)]
        text_file: PathBuf,
        #[arg(long)]
        python: Option<String>,
    },
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Command::ProjectBatch {
            feature_spaces,
            requests,
            output_dir,
            python,
            batch_size,
        } => println!(
            "{}",
            serde_json::to_string_pretty(&pipeline::project_requests(
                &feature_spaces,
                &requests,
                &output_dir,
                python.as_deref(),
                batch_size
            )?)?
        ),
        Command::Project {
            feature_space,
            text_file,
            python,
        } => println!(
            "{}",
            serde_json::to_string_pretty(&pipeline::project_text(
                &feature_space,
                &text_file,
                python.as_deref()
            )?)?
        ),
        Command::Audit { input } => {
            let records = dataset::read_records(&input)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&dataset::summarize(&records))?
            );
        }
        Command::Split {
            input,
            output,
            seed,
        } => {
            let mut records = dataset::read_records(&input)?;
            let report = dataset::assign_splits(&mut records, &seed)?;
            dataset::write_records(&output, &records)?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        Command::Acquire {
            output_dir,
            plos,
            wikinews,
        } => {
            println!(
                "{}",
                serde_json::to_string_pretty(&acquire::acquire(&output_dir, plos, wikinews)?)?
            );
        }
        Command::ExportShards { input, output_dir } => println!(
            "{}",
            serde_json::to_string_pretty(&pipeline::export_shards(&input, &output_dir)?)?
        ),
        Command::Featurize {
            input,
            output,
            mode,
            python,
            batch_size,
        } => println!(
            "{}",
            serde_json::to_string_pretty(&pipeline::featurize(
                &input,
                &output,
                mode.into(),
                python.as_deref(),
                batch_size
            )?)?
        ),
        Command::ExportVectors {
            features,
            output_dir,
            mode,
            max_coordinates,
            word_coordinate_cap,
            min_document_frequency,
        } => {
            let config = FeatureConfig {
                mode: mode.into(),
                max_coordinates,
                word_coordinate_cap,
                min_document_frequency,
                ..Default::default()
            };
            println!(
                "{}",
                serde_json::to_string_pretty(&pipeline::export_vectors(
                    &features,
                    &output_dir,
                    &config
                )?)?
            );
        }
        Command::Train {
            features,
            mode,
            output_dir,
            max_coordinates,
            word_coordinate_cap,
            min_document_frequency,
            epochs,
            learning_rate,
            l2,
            patience,
        } => {
            let config = TrainConfig {
                features: FeatureConfig {
                    mode: mode.into(),
                    max_coordinates,
                    word_coordinate_cap,
                    min_document_frequency,
                    ..Default::default()
                },
                epochs,
                learning_rate,
                l2,
                patience,
            };
            println!(
                "{}",
                serde_json::to_string_pretty(&pipeline::train(&features, &output_dir, &config)?)?
            );
        }
        Command::Evaluate {
            features,
            artifact,
            output,
            split,
            open_final_test,
        } => println!(
            "{}",
            serde_json::to_string_pretty(&pipeline::evaluate(
                &features,
                &artifact,
                &output,
                split.into(),
                open_final_test
            )?)?
        ),
        Command::Infer {
            artifact,
            text_file,
            python,
        } => println!(
            "{}",
            serde_json::to_string_pretty(&pipeline::infer(
                &artifact,
                &text_file,
                python.as_deref()
            )?)?
        ),
    }
    Ok(())
}
