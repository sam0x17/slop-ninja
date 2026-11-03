//! Bounded, cached public-source collection. No model or detector calls.
use anyhow::{Context, Result, ensure};
use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use unslop::{
    global_voices::{self as gv, Author},
    util::digest,
};

#[derive(Parser)]
struct Args {
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    Directory {
        #[arg(long)]
        html: PathBuf,
    },
    Collect {
        #[arg(long)]
        plan: PathBuf,
        #[arg(long)]
        out: PathBuf,
        /// Re-extract saved responses without making any network requests.
        #[arg(long)]
        cache_only: bool,
    },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Plan {
    schema: String,
    authors: Vec<Author>,
    archive_pages: usize,
    min_articles: usize,
    max_articles: usize,
    min_words: usize,
    max_words: usize,
    selection: String,
    directory_sha256: String,
}

struct Fetcher {
    root: PathBuf,
    client: reqwest::blocking::Client,
    last: Option<Instant>,
    cache_only: bool,
    cached_responses: usize,
    network_requests: usize,
}
impl Fetcher {
    fn get(&mut self, url: &str) -> Result<String> {
        gv::source_url(url)?;
        let key = digest(url);
        let body_path = self.root.join(format!("{key}.body"));
        let meta_path = self.root.join(format!("{key}.json"));
        ensure!(
            body_path.exists() == meta_path.exists(),
            "Incomplete cached response; inspect {key}"
        );
        if body_path.exists() {
            let body = fs::read_to_string(body_path)?;
            let meta: Value = serde_json::from_slice(&fs::read(meta_path)?)?;
            ensure!(
                meta["url"] == url && meta["sha256"] == digest(&body) && meta["status"] == 200,
                "Cached response differs"
            );
            self.cached_responses += 1;
            return Ok(body);
        }
        ensure!(
            !self.cache_only,
            "Missing response in cache-only mode: {key}"
        );
        if let Some(last) = self.last {
            std::thread::sleep(Duration::from_secs(10).saturating_sub(last.elapsed()));
        }
        self.last = Some(Instant::now());
        self.network_requests += 1;
        let response = self.client.get(url).send()?;
        let status = response.status().as_u16();
        let headers: BTreeMap<_, _> = response
            .headers()
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("non-UTF8").to_owned()))
            .collect();
        let mut bytes = Vec::new();
        response.take(8 * 1024 * 1024 + 1).read_to_end(&mut bytes)?;
        ensure!(bytes.len() <= 8 * 1024 * 1024, "Response exceeds limit");
        let body = String::from_utf8(bytes)?;
        fs::write(&body_path, &body)?;
        fs::write(
            &meta_path,
            serde_json::to_vec_pretty(
                &json!({"url":url,"sha256":digest(&body),"status":status,"retrieved_at":chrono::Utc::now(),"headers":headers}),
            )?,
        )?;
        ensure!(
            status == 200,
            "Publisher returned HTTP {status}; stop without retry"
        );
        Ok(body)
    }
}

fn write_json(path: &Path, value: &impl Serialize) -> Result<()> {
    fs::write(path, serde_json::to_vec_pretty(value)?)?;
    Ok(())
}

fn collect(plan_path: &Path, out: &Path, cache_only: bool) -> Result<()> {
    let plan_bytes = fs::read(plan_path)?;
    let plan: Plan = serde_json::from_slice(&plan_bytes)?;
    ensure!(
        plan.schema == "slopninja-global-voices-plan-v1"
            && (1..=40).contains(&plan.authors.len())
            && (1..=3).contains(&plan.archive_pages)
            && plan.min_articles >= 6
            && plan.max_articles >= plan.min_articles
            && plan.min_words >= 100
            && plan.max_words >= plan.min_words,
        "Invalid bounded collection plan"
    );
    fs::create_dir_all(out.join("raw"))?;
    let saved_plan = out.join("plan.json");
    if saved_plan.exists() {
        ensure!(
            fs::read(&saved_plan)? == plan_bytes,
            "Plan changed; use a new output directory"
        );
    } else {
        fs::write(&saved_plan, &plan_bytes)?;
    }
    let mut fetch = Fetcher {
        root: out.join("raw"),
        client: reqwest::blocking::Client::builder()
            .user_agent("slopninja-research/0.2 (+https://github.com/sam0x17/slop-ninja)")
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .timeout(Duration::from_secs(30))
            .build()?,
        last: None,
        cache_only,
        cached_responses: 0,
        network_requests: 0,
    };
    let robots = fetch.get("https://globalvoices.org/robots.txt")?;
    ensure!(
        robots.contains("Crawl-delay: 10"),
        "Publisher robots policy changed; review before collection"
    );
    let license_url = "https://globalvoices.org/about/global-voices-attribution-policy/";
    let license = fetch.get(license_url)?;
    let mut records = Vec::new();
    let mut summaries = Vec::new();
    for (index, author) in plan.authors.iter().enumerate() {
        let mut archive_records = BTreeMap::new();
        let mut native_id = None;
        for page in 1..=plan.archive_pages {
            let url = if page == 1 {
                author.url.clone()
            } else {
                format!("{}page/{page}/", author.url)
            };
            let html = fetch.get(&url)?;
            let (id, posts) = gv::archive(&html)?;
            ensure!(
                native_id.is_none_or(|old| old == id),
                "Author ID changed across pages"
            );
            native_id = Some(id);
            for post in posts {
                archive_records.entry(post.url.clone()).or_insert((
                    post,
                    url.clone(),
                    digest(&html),
                ));
            }
        }
        // The archive supplies custom writer/translator credits omitted from
        // ordinary WordPress author IDs. Join by exact canonical story URL.
        let slugs = archive_records
            .keys()
            .map(|url| url.trim_end_matches('/').rsplit('/').next().unwrap())
            .collect::<Vec<_>>()
            .join(",");
        let mut url = gv::source_url("https://globalvoices.org/wp-json/wp/v2/posts")?;
        url.query_pairs_mut()
            .append_pair("slug", &slugs)
            .append_pair("per_page", "100")
            .append_pair("orderby", "date")
            .append_pair("order", "asc");
        let raw = fetch.get(url.as_str())?;
        let api: Vec<Value> = serde_json::from_str(&raw)?;
        let mut seen = BTreeSet::new();
        for post in api {
            let link = post["link"].as_str().context("Missing API story URL")?;
            let (archive, archive_url, archive_sha) = archive_records
                .get(link)
                .context("API returned an unrequested story")?;
            ensure!(seen.insert(link.to_owned()), "API repeated a story");
            let date = post["date_gmt"]
                .as_str()
                .context("Missing publication date")?;
            let parsed_date = chrono::NaiveDateTime::parse_from_str(date, "%Y-%m-%dT%H:%M:%S")?;
            let content = post["content"]["rendered"]
                .as_str()
                .context("Missing API content")?;
            let extracted = gv::extract_content(content);
            let word_count = unslop::features::words(&extracted.text).len();
            let mut reasons = Vec::new();
            if !archive.sole_english_writer(&author.url) {
                reasons.push("archive_credit_not_single_english_writer");
            }
            if post["author"].as_u64() != native_id {
                reasons.push("api_author_differs");
            }
            // Language taxonomy can describe source material. Its absence
            // does not override the archive's observed writer/translator roles.
            if !extracted.origin_flags.is_empty() {
                reasons.push("origin_notice_requires_review");
            }
            if post["content"]["protected"] != false || post["status"] != "publish" {
                reasons.push("not_public_unprotected_post");
            }
            if word_count < plan.min_words || word_count > plan.max_words {
                reasons.push("retained_length_outside_plan");
            }
            if parsed_date.and_utc() > chrono::Utc::now() {
                reasons.push("future_publication");
            }
            records.push(json!({"id":format!("gv:{}",post["id"]),"author_id":author.url,"source_group":format!("gv:{}",post["id"]),"date":parsed_date.date().to_string(),"text":extracted.text,"text_sha256":digest(&extracted.text),"words":word_count,"reasons":reasons,"paragraphs":extracted.paragraphs,"provenance":{"source_url":link,"title":archive.title,"author_name":author.name,"credits":archive.credits,"publication_gmt":date,"modification_gmt":post["modified_gmt"],"taxonomy":post["class_list"],"content_html_sha256":digest(content),"archive_url":archive_url,"archive_sha256":archive_sha,"api_url":url.as_str(),"api_sha256":digest(&raw),"license":"CC-BY-3.0 publisher default; individual exceptions require review","license_url":"https://creativecommons.org/licenses/by/3.0/","license_evidence_url":license_url,"license_evidence_sha256":digest(&license),"extraction":gv::EXTRACTION,"origin_flags":extracted.origin_flags,"authorship_basis":"English-page single-writer credits and matching API author; paragraphs passing quotation heuristics; original language and human-only production unverified; article header/footer not individually cross-checked","production_history":"unknown; publisher attribution does not prove human-only"}}));
        }
        let missing: Vec<_> = archive_records
            .keys()
            .filter(|url| !seen.contains(*url))
            .cloned()
            .collect();
        let accepted = records
            .iter()
            .filter(|r| r["author_id"] == author.url && r["reasons"].as_array().unwrap().is_empty())
            .count();
        summaries.push(json!({"author":author,"archive_stories":archive_records.len(),"api_stories":seen.len(),"missing_from_api":missing,"provisionally_eligible_articles":accepted}));
        write_json(&out.join("records.json"), &records)?;
        write_json(&out.join("authors.json"), &summaries)?;
        eprintln!(
            "Author {}/{}: {} provisionally eligible of {} archive stories",
            index + 1,
            plan.authors.len(),
            accepted,
            archive_records.len()
        );
    }
    // Distinct articles sharing extracted text are excluded. Seeing one article
    // in several contributor archives does not create a new source work.
    let mut hashes = BTreeMap::<String, BTreeSet<String>>::new();
    for row in &records {
        hashes
            .entry(row["text_sha256"].as_str().unwrap().to_owned())
            .or_default()
            .insert(row["id"].as_str().unwrap().to_owned());
    }
    for row in &mut records {
        if hashes[row["text_sha256"].as_str().unwrap()].len() > 1 {
            row["reasons"]
                .as_array_mut()
                .unwrap()
                .push(json!("duplicate_extracted_text"));
        }
    }
    for summary in &mut summaries {
        let count = records
            .iter()
            .filter(|r| {
                r["author_id"] == summary["author"]["url"]
                    && r["reasons"].as_array().unwrap().is_empty()
            })
            .count();
        summary["final_eligible_after_duplicate_filter"] = json!(count);
    }
    write_json(&out.join("authors.json"), &summaries)?;
    let mut output = fs::File::create(out.join("transfer.jsonl"))?;
    let mut included = Vec::new();
    let mut source_manifest = Vec::new();
    for author in &plan.authors {
        let mut mine: Vec<_> = records
            .iter()
            .filter(|r| r["author_id"] == author.url && r["reasons"].as_array().unwrap().is_empty())
            .cloned()
            .collect();
        mine.sort_by(|a, b| {
            a["date"]
                .as_str()
                .cmp(&b["date"].as_str())
                .then_with(|| a["id"].as_str().cmp(&b["id"].as_str()))
        });
        if mine.len() < plan.min_articles {
            continue;
        }
        mine.truncate(plan.max_articles);
        let dates: Vec<_> = mine
            .iter()
            .map(|r| r["date"].as_str().unwrap())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        if dates.len() < 4 {
            continue;
        }
        let boundary = dates[(dates.len() * 2 / 3).max(2)].to_owned();
        let mut counts = BTreeMap::<String, usize>::new();
        for mut row in mine {
            let split = if row["date"].as_str().unwrap() < boundary.as_str() {
                "reference"
            } else {
                "query"
            };
            *counts.entry(split.to_owned()).or_default() += 1;
            row["split"] = json!(split);
            row["provenance"]["retained_words"] = row["words"].clone();
            row.as_object_mut().unwrap().remove("paragraphs");
            row.as_object_mut().unwrap().remove("words");
            row.as_object_mut().unwrap().remove("reasons");
            let p = &row["provenance"];
            source_manifest.push(json!({"id":row["id"],"author_id":row["author_id"],"author_name":p["author_name"],"source_url":p["source_url"],"date":row["date"],"split":split,"text_sha256":row["text_sha256"],"retained_words":p["retained_words"],"content_html_sha256":p["content_html_sha256"],"archive_sha256":p["archive_sha256"],"extraction":p["extraction"],"license":p["license"],"license_url":p["license_url"],"license_evidence_sha256":p["license_evidence_sha256"]}));
            writeln!(output, "{}", serde_json::to_string(&row)?)?;
        }
        included.push(json!({"author_id":author.url,"counts":counts,"query_start_date":boundary}));
    }
    write_json(&out.join("records.json"), &records)?;
    write_json(
        &out.join("source-manifest.json"),
        &json!({"schema":"slopninja-global-voices-source-manifest-v1","notice":"Metadata only; source text, paragraphs and fitted profiles are excluded. Extraction removes marked quotations and nonprose paragraphs and normalizes whitespace. Individual rights and production history remain unverified; this is not a commercial training release.","articles":source_manifest}),
    )?;
    let summary = json!({"schema":"slopninja-global-voices-collection-v2","extraction":gv::EXTRACTION,"collector_source_sha256":digest(include_str!("collect_global_voices.rs")),"extractor_source_sha256":digest(include_str!("../src/global_voices.rs")),"cached_responses":fetch.cached_responses,"network_requests":fetch.network_requests,"plan_sha256":digest(std::str::from_utf8(&plan_bytes)?),"records":records.len(),"authors_requested":plan.authors.len(),"authors_eligible":included.len(),"included":included,"transfer_sha256":digest(&fs::read_to_string(out.join("transfer.jsonl"))?),"article_level_manual_review_completed":false,"commercial_training_release":false});
    write_json(&out.join("summary.json"), &summary)?;
    println!("{}", serde_json::to_string_pretty(&summary)?);
    Ok(())
}

fn main() -> Result<()> {
    match Args::parse().command {
        Command::Directory { html } => println!(
            "{}",
            serde_json::to_string_pretty(&gv::directory(&fs::read_to_string(html)?)?)?
        ),
        Command::Collect {
            plan,
            out,
            cache_only,
        } => collect(&plan, &out, cache_only)?,
    }
    Ok(())
}
