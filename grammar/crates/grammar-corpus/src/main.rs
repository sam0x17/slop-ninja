use anyhow::Result;
use clap::{Parser, Subcommand};
use grammar_corpus::{DatePolicy, export, import};
use std::path::PathBuf;

#[derive(Parser)]
#[command(about = "Import local author corpora and export reproducible reference samples")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Read the original Blog Authorship ZIP without extracting archive paths.
    ImportBlog {
        zip: PathBuf,
        #[arg(long)]
        out: PathBuf,
        /// Accept valid supplied dates, or reproduce the historical 1999 floor.
        #[arg(long, value_enum, default_value = "valid-calendar")]
        date_policy: DatePolicy,
    },
    /// Select eligible authors by seeded hash, then split their posts by date.
    ExportAuthors {
        database: PathBuf,
        #[arg(long)]
        out: PathBuf,
        #[arg(long, default_value_t = 100)]
        authors: usize,
        #[arg(long, default_value_t = 10)]
        min_posts: usize,
        #[arg(long, default_value_t = 150)]
        min_post_words: u64,
        #[arg(long, default_value_t = 1200)]
        max_post_words: u64,
        #[arg(long, default_value_t = 20)]
        max_posts_per_author: usize,
        #[arg(long, default_value_t = 3000)]
        min_total_words: u64,
        #[arg(long, default_value = "blog-author-pilot-v1")]
        seed: String,
        /// Include complete posts containing the dataset's urlLink placeholder.
        #[arg(long)]
        allow_placeholders: bool,
        /// Exclude author IDs in prior export summaries; repeat for multiple cohorts.
        #[arg(long)]
        exclude_authors_from: Vec<PathBuf>,
    },
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Command::ImportBlog {
            zip,
            out,
            date_policy,
        } => {
            let summary = import::import_blog_with_date_policy(&zip, &out, date_policy)?;
            println!("{}", serde_json::to_string_pretty(&summary)?);
        }
        Command::ExportAuthors {
            database,
            out,
            authors,
            min_posts,
            min_post_words,
            max_post_words,
            max_posts_per_author,
            min_total_words,
            seed,
            allow_placeholders,
            exclude_authors_from,
        } => {
            let summary = export::export_authors(
                &database,
                &out,
                &export::Options {
                    authors,
                    min_posts,
                    min_post_words,
                    max_post_words,
                    max_posts_per_author,
                    min_total_words,
                    seed,
                    allow_placeholders,
                    exclude_authors_from,
                },
            )?;
            println!("{}", serde_json::to_string_pretty(&summary)?);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn import_defaults_to_calendar_validity_and_legacy_is_explicit() {
        for (extra, expected) in [
            (Vec::new(), DatePolicy::ValidCalendar),
            (vec!["--date-policy", "legacy-1999"], DatePolicy::Legacy1999),
        ] {
            let mut arguments = vec![
                "unslop-corpus",
                "import-blog",
                "blogs.zip",
                "--out",
                "new-import",
            ];
            arguments.extend(extra);
            match Cli::try_parse_from(arguments).unwrap().command {
                Command::ImportBlog { date_policy, .. } => assert_eq!(date_policy, expected),
                _ => panic!("wrong command"),
            }
        }
    }

    #[test]
    fn exclusion_flag_accepts_one_or_repeated_summary_paths() {
        for paths in [vec!["first.json"], vec!["first.json", "second.json"]] {
            let mut arguments = vec![
                "unslop-corpus",
                "export-authors",
                "corpus.sqlite",
                "--out",
                "fresh",
            ];
            for path in &paths {
                arguments.extend(["--exclude-authors-from", path]);
            }
            let parsed = Cli::try_parse_from(arguments).unwrap();
            match parsed.command {
                Command::ExportAuthors {
                    exclude_authors_from,
                    ..
                } => {
                    assert_eq!(
                        exclude_authors_from,
                        paths.iter().map(PathBuf::from).collect::<Vec<_>>()
                    );
                }
                _ => panic!("wrong command"),
            }
        }
    }
}
