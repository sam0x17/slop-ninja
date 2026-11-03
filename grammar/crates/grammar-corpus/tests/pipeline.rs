use grammar_corpus::{DatePolicy, export, hash, import};
use rusqlite::Connection;
use serde_json::Value;
use std::{collections::BTreeMap, fs, io::Write, path::Path};
use zip::{ZipWriter, write::SimpleFileOptions};

fn archive(path: &Path, entries: &[(&str, Vec<u8>)]) {
    let mut archive = ZipWriter::new(fs::File::create_new(path).unwrap());
    for (name, bytes) in entries {
        archive
            .start_file(*name, SimpleFileOptions::default())
            .unwrap();
        archive.write_all(bytes).unwrap();
    }
    archive.finish().unwrap();
}

fn post(day: u32, text: &str) -> String {
    format!("<date>{day},May,2004</date>\r\n<post>\r\n{text}\r\n</post>\r\n")
}

fn author_posts(author: &str) -> String {
    let mut output = String::from("<Blog>\r\n");
    for (index, label) in [
        "alpha", "bravo", "charlie", "delta", "echo", "foxtrot", "golf", "hotel",
    ]
    .iter()
    .enumerate()
    {
        output.push_str(&post(
            index as u32 + 1,
            &format!("{author} wrote the {label} account. Its punctuation stays: yes, exactly!"),
        ));
    }
    output
}

#[test]
fn imports_provenance_retracts_cross_author_counts_and_exports_repeatably() {
    let temp = tempfile::tempdir().unwrap();
    let zip = temp.path().join("blogs.zip");
    let mut first = author_posts("First");
    first.push_str(&post(
        9,
        "A copied passage shared between two different authors.",
    ));
    first.push_str(&post(
        10,
        " First   wrote the alpha account. Its punctuation stays: yes, exactly! ",
    ));
    first.push_str("<date>31,December,1998</date>\n<post>\nEarlier blog entry.\n</post>\n");
    first.push_str("<date>31,April,2004</date>\n<post>\nInvalid calendar.\n</post>\n</Blog>\n");
    let mut second = author_posts("Second");
    second.push_str(&post(
        9,
        "A copied passage shared between two different authors.",
    ));
    second.push_str(&post(
        10,
        "The urlLink placeholder stays here and this entire post is excluded from pilots.",
    ));
    let mut second = second.into_bytes();
    second.extend(b"<date>11,May,2004</date>\n<post>\nThis post has an encoding artifact at the end.\x85\n</post>\n</Blog>\n");
    // ZIP order deliberately differs from canonical filename order.
    archive(&zip, &[
        ("blogs/2.male.25.indUnk.Leo.xml", second),
        ("blogs/1.female.25.indUnk.Leo.xml", first.into_bytes()),
        ("blogs/3.male.25.indUnk.Leo.xml", b"<?xml encoding='UTF-8'?>\n<date>1,May,2004</date>\n<post>\nInvalid\xff\n</post>\n".to_vec()),
        ("../4.male.25.indUnk.Leo.xml", post(1, "Must not extract this path.").into_bytes()),
        ("blogs/5.male.25.indUnk.Leo.xml", b"<Blog>\n<post attr='bad'>Hidden malformed body</post>\n</Blog>".to_vec()),
    ]);
    let imported = temp.path().join("import");
    let summary = import::import_blog(&zip, &imported).unwrap();
    assert_eq!(summary.post_status_counts["cross_author_duplicate"], 2);
    assert_eq!(summary.post_status_counts["duplicate_text"], 1);
    assert!(!summary.post_status_counts.contains_key("before_1999"));
    assert_eq!(summary.date_policy, DatePolicy::ValidCalendar);
    assert_eq!(summary.original_writing_date_minimum, None);
    assert_eq!(summary.minimum_retained_date.as_deref(), Some("1998-12-31"));
    assert_eq!(summary.post_status_counts["malformed_date"], 1);
    assert_eq!(summary.post_status_counts["invalid_declared_utf8"], 1);
    assert_eq!(summary.entry_status_counts["invalid_author_filename"], 1);
    assert_eq!(summary.entry_status_counts["no_recognized_posts"], 1);
    assert_eq!(summary.authors_with_retained_posts, 2);
    assert_eq!(summary.retained_posts, 19);
    assert_eq!(summary.retained_posts_with_placeholder, 1);
    assert_eq!(summary.retained_posts_with_c1_controls, 1);
    assert!(!temp.path().join("4.male.25.indUnk.Leo.xml").exists());
    let database = imported.join("corpus.sqlite");
    let db = Connection::open(&database).unwrap();
    let copied: u64 = db
        .query_row(
            "SELECT COUNT(*) FROM corpus_words WHERE word='copied'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        copied, 0,
        "cross-author duplicates must not enter any profile"
    );
    let totals: u64 = db
        .query_row(
            "SELECT SUM(word_count) FROM posts WHERE status='retained'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(totals, summary.retained_words);
    let counter_mismatches: u64 = db.query_row(
        "SELECT COUNT(*) FROM authors a WHERE a.total_words<>(SELECT COALESCE(SUM(occurrences),0) FROM author_words w WHERE w.author_id=a.author_id)",
        [], |row| row.get(0)).unwrap();
    assert_eq!(counter_mismatches, 0);
    let options = export::Options {
        authors: 2,
        min_posts: 3,
        min_post_words: 5,
        max_post_words: 100,
        max_posts_per_author: 6,
        min_total_words: 10,
        seed: "fixture-seed".into(),
        allow_placeholders: false,
        exclude_authors_from: Vec::new(),
    };
    let output = temp.path().join("authors");
    let exported = export::export_authors(&database, &output, &options).unwrap();
    assert_eq!(exported.selected_authors, 2);
    assert_eq!(exported.selected_posts, 12);
    assert_eq!(exported.full_pool_eligible_authors, 2);
    let repeated = temp.path().join("authors-repeat");
    let second_export = export::export_authors(&database, &repeated, &options).unwrap();
    assert_eq!(exported.authors, second_export.authors);
    assert_eq!(
        fs::read(output.join("summary.json")).unwrap(),
        fs::read(repeated.join("summary.json")).unwrap()
    );
    for author in &exported.authors {
        let manifest = output.join(&author.manifest);
        let repeated_manifest = repeated.join(&author.manifest);
        assert_eq!(
            fs::read(&manifest).unwrap(),
            fs::read(repeated_manifest).unwrap()
        );
        let mut dates = BTreeMap::new();
        for line in fs::read_to_string(&manifest).unwrap().lines() {
            let row: Value = serde_json::from_str(line).unwrap();
            let path = manifest
                .parent()
                .unwrap()
                .join(row["path"].as_str().unwrap());
            let bytes = fs::read(&path).unwrap();
            assert_eq!(hash(&bytes), row["text_sha256"].as_str().unwrap());
            assert_eq!(row["text_sha256"], row["provenance"]["excerpt_sha256"]);
            assert_eq!(row["authorship"], "human");
            assert_eq!(row["provenance"]["has_placeholder"], false);
            assert_eq!(row["provenance"]["has_c1_controls"], false);
            let date = row["provenance"]["supplied_composition_date"]
                .as_str()
                .unwrap()
                .to_owned();
            let split = row["split"].as_str().unwrap().to_owned();
            if let Some(previous) = dates.insert(date, split.clone()) {
                assert_eq!(previous, split);
            }
        }
        let chronological: Vec<_> = dates.values().map(String::as_str).collect();
        assert!(
            chronological.contains(&"train")
                && chronological.contains(&"dev")
                && chronological.contains(&"test")
        );
        let indices: Vec<_> = chronological
            .iter()
            .map(|split| match *split {
                "train" => 0,
                "dev" => 1,
                "test" => 2,
                _ => unreachable!(),
            })
            .collect();
        assert!(indices.windows(2).all(|pair| pair[0] <= pair[1]));
    }
    assert!(export::export_authors(&database, &output, &options).is_err());
    assert!(import::import_blog(&zip, &imported).is_err());
    let repeated_import = temp.path().join("import-repeat");
    let repeated_summary = import::import_blog(&zip, &repeated_import).unwrap();
    assert_eq!(summary, repeated_summary);
}

#[test]
fn rejects_authors_without_three_dates_after_caps() {
    let temp = tempfile::tempdir().unwrap();
    let zip = temp.path().join("blogs.zip");
    let text = [
        "One distinct story.",
        "Another distinct story.",
        "Third distinct story.",
    ]
    .iter()
    .map(|text| post(1, text))
    .collect::<String>();
    archive(
        &zip,
        &[("blogs/1.female.25.indUnk.Leo.xml", text.into_bytes())],
    );
    let imported = temp.path().join("import");
    import::import_blog(&zip, &imported).unwrap();
    let options = export::Options {
        authors: 1,
        min_posts: 3,
        min_post_words: 1,
        max_post_words: 100,
        max_posts_per_author: 3,
        min_total_words: 3,
        seed: "fixture".into(),
        allow_placeholders: false,
        exclude_authors_from: Vec::new(),
    };
    let out = temp.path().join("no-authors");
    assert!(export::export_authors(&imported.join("corpus.sqlite"), &out, &options).is_err());
    assert!(
        !out.exists(),
        "ineligible selection should create no output run"
    );
}

fn three_author_fixture(root: &Path) -> (std::path::PathBuf, export::Options) {
    let zip = root.join("blogs.zip");
    archive(
        &zip,
        &[
            (
                "blogs/1.female.25.indUnk.Leo.xml",
                author_posts("First").into_bytes(),
            ),
            (
                "blogs/2.male.25.indUnk.Leo.xml",
                author_posts("Second").into_bytes(),
            ),
            (
                "blogs/3.male.25.indUnk.Leo.xml",
                author_posts("Third").into_bytes(),
            ),
        ],
    );
    let imported = root.join("import");
    import::import_blog(&zip, &imported).unwrap();
    (
        imported.join("corpus.sqlite"),
        export::Options {
            authors: 1,
            min_posts: 3,
            min_post_words: 5,
            max_post_words: 100,
            max_posts_per_author: 6,
            min_total_words: 10,
            seed: "fixture-seed".into(),
            allow_placeholders: false,
            exclude_authors_from: Vec::new(),
        },
    )
}

#[test]
fn excluded_cohorts_are_disjoint_reproducible_and_pin_the_prior_summary() {
    let temp = tempfile::tempdir().unwrap();
    let (database, mut options) = three_author_fixture(temp.path());
    let prior_out = temp.path().join("prior");
    let prior = export::export_authors(&database, &prior_out, &options).unwrap();
    let summary_path = prior_out.join("summary.json");
    options.authors = 2;
    options.exclude_authors_from = vec![summary_path.clone()];
    options.seed = "blog-author-replication-v1".into();
    let fresh_out = temp.path().join("fresh");
    let fresh = export::export_authors(&database, &fresh_out, &options).unwrap();
    assert_eq!(fresh.selected_authors, 2);
    assert!(
        fresh
            .authors
            .iter()
            .all(|author| author.author_id != prior.authors[0].author_id)
    );
    let exclusions = fresh.exclusions.as_ref().unwrap();
    assert_eq!(exclusions.eligible_authors_removed, 1);
    assert_eq!(exclusions.eligible_authors_remaining, 2);
    assert_eq!(
        exclusions.author_ids,
        vec![prior.authors[0].author_id.clone()]
    );
    assert_eq!(
        exclusions.source_summary_sha256,
        Some(hash(&fs::read(&summary_path).unwrap()))
    );
    assert_eq!(
        exclusions.author_ids_sha256,
        hash(&serde_json::to_vec(&exclusions.author_ids).unwrap())
    );
    let repeated_out = temp.path().join("repeat");
    export::export_authors(&database, &repeated_out, &options).unwrap();
    assert_eq!(
        fs::read(fresh_out.join("summary.json")).unwrap(),
        fs::read(repeated_out.join("summary.json")).unwrap()
    );
    options.authors = 3;
    let short_out = temp.path().join("short");
    let error = export::export_authors(&database, &short_out, &options).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("only 2 eligible authors remain after exclusions")
    );
    assert!(!short_out.exists());
}

#[test]
fn invalid_exclusion_identity_or_counts_fail_before_output_creation() {
    let temp = tempfile::tempdir().unwrap();
    let (database, mut options) = three_author_fixture(temp.path());
    let prior_out = temp.path().join("prior");
    export::export_authors(&database, &prior_out, &options).unwrap();
    let prior: Value =
        serde_json::from_slice(&fs::read(prior_out.join("summary.json")).unwrap()).unwrap();
    for (case, expected) in [
        ("schema", "unsupported exclusion summary schema"),
        ("archive_sha256", "archive hash mismatch"),
        ("database_sha256", "database hash mismatch"),
        ("selected_authors", "author count mismatch"),
        ("selected_posts", "post count mismatch"),
        ("selected_words", "word count mismatch"),
        ("unsafe_author", "unsafe excluded author ID"),
        ("unknown_author", "excluded author ID missing"),
        ("duplicate_author", "duplicate excluded author ID"),
        ("author_posts", "excluded author post count mismatch"),
        ("author_words", "excluded author word count mismatch"),
    ] {
        let mut altered = prior.clone();
        match case {
            "schema" | "archive_sha256" | "database_sha256" => altered[case] = "wrong".into(),
            "selected_authors" | "selected_posts" | "selected_words" => altered[case] = 0.into(),
            "unsafe_author" => altered["authors"][0]["author_id"] = "../escape".into(),
            "unknown_author" => altered["authors"][0]["author_id"] = "999".into(),
            "duplicate_author" => {
                let duplicate = altered["authors"][0].clone();
                altered["authors"].as_array_mut().unwrap().push(duplicate);
                for count in ["selected_authors", "selected_posts", "selected_words"] {
                    altered[count] = (altered[count].as_u64().unwrap() * 2).into();
                }
            }
            "author_posts" => altered["authors"][0]["split_posts"]["train"] = 0.into(),
            "author_words" => altered["authors"][0]["split_words"]["train"] = 0.into(),
            _ => unreachable!(),
        }
        let path = temp.path().join(format!("{case}.json"));
        fs::write(&path, serde_json::to_vec(&altered).unwrap()).unwrap();
        options.exclude_authors_from = vec![path];
        let output = temp.path().join(format!("output-{case}"));
        let error = export::export_authors(&database, &output, &options).unwrap_err();
        assert!(error.to_string().contains(expected), "{case}: {error}");
        assert!(!output.exists());
    }
}

#[test]
fn multiple_exclusions_bind_each_summary_and_deduplicate_the_author_union() {
    let temp = tempfile::tempdir().unwrap();
    let (database, mut options) = three_author_fixture(temp.path());
    let first_out = temp.path().join("first");
    let first = export::export_authors(&database, &first_out, &options).unwrap();
    let first_path = first_out.join("summary.json");
    options.exclude_authors_from = vec![first_path.clone()];
    let second_out = temp.path().join("second");
    let second = export::export_authors(&database, &second_out, &options).unwrap();
    let second_path = second_out.join("summary.json");
    // The single-input JSON form is unchanged, so old exported summaries load.
    let single = serde_json::to_value(&options).unwrap();
    assert!(single["exclude_authors_from"].is_string());
    assert_eq!(
        serde_json::from_value::<export::Options>(single)
            .unwrap()
            .exclude_authors_from,
        vec![first_path.clone()]
    );
    options.exclude_authors_from =
        vec![first_path.clone(), second_path.clone(), first_path.clone()];
    let third = export::export_authors(&database, &temp.path().join("third"), &options).unwrap();
    let exclusions = third.exclusions.as_ref().unwrap();
    assert_eq!(third.selected_authors, 1);
    assert_ne!(third.authors[0].author_id, first.authors[0].author_id);
    assert_ne!(third.authors[0].author_id, second.authors[0].author_id);
    assert_eq!(exclusions.author_ids.len(), 2);
    assert_eq!(exclusions.eligible_authors_removed, 2);
    assert_eq!(exclusions.eligible_authors_remaining, 1);
    assert!(exclusions.source_summary.is_none());
    assert!(exclusions.source_summary_sha256.is_none());
    assert_eq!(exclusions.source_summaries.len(), 3);
    for (source, path) in exclusions
        .source_summaries
        .iter()
        .zip(&options.exclude_authors_from)
    {
        assert_eq!(&source.source_summary, path);
        assert_eq!(source.source_summary_sha256, hash(&fs::read(path).unwrap()));
        assert_eq!(
            source.author_ids_sha256,
            hash(&serde_json::to_vec(&source.author_ids).unwrap())
        );
    }
    assert_eq!(
        exclusions.author_ids_sha256,
        hash(&serde_json::to_vec(&exclusions.author_ids).unwrap())
    );
    let multiple = serde_json::to_value(&options).unwrap();
    assert!(multiple["exclude_authors_from"].is_array());
    assert_eq!(
        serde_json::from_value::<export::Options>(multiple)
            .unwrap()
            .exclude_authors_from,
        options.exclude_authors_from
    );
    let serialized = serde_json::to_vec(&third).unwrap();
    let deserialized: export::Summary = serde_json::from_slice(&serialized).unwrap();
    assert_eq!(deserialized.exclusions.unwrap().source_summaries.len(), 3);

    // Every source is checked, including a later one after a valid first input.
    let mut altered: Value = serde_json::from_slice(&fs::read(&second_path).unwrap()).unwrap();
    altered["archive_sha256"] = "different-corpus".into();
    let invalid_path = temp.path().join("invalid-second.json");
    fs::write(&invalid_path, serde_json::to_vec(&altered).unwrap()).unwrap();
    options.exclude_authors_from = vec![first_path, invalid_path];
    let output = temp.path().join("invalid-output");
    let error = export::export_authors(&database, &output, &options).unwrap_err();
    assert!(error.to_string().contains("archive hash mismatch"));
    assert!(!output.exists());
}

#[test]
fn default_and_legacy_imports_record_distinct_reproducible_date_policies() {
    let temp = tempfile::tempdir().unwrap();
    let zip = temp.path().join("dates.zip");
    let text = b"<post>\nAn undated post stays excluded.\n</post>\n<date>31,December,1998</date>\n<post>\nAn earlier dated blog post.\n</post>\n<date>1,January,1999</date>\n<post>\nAnother dated blog post.\n</post>\n<date>29,February,1900</date>\n<post>\nAn invalid leap day stays excluded.\n</post>\n<date>1,January,0000</date>\n<post>\nYear zero stays excluded.\n</post>\n";
    archive(&zip, &[("blogs/1.female.25.indUnk.Leo.xml", text.to_vec())]);
    for (name, policy, expected_posts) in [
        ("default", DatePolicy::ValidCalendar, 2),
        ("legacy", DatePolicy::Legacy1999, 1),
    ] {
        let output = temp.path().join(name);
        let summary = import::import_blog_with_date_policy(&zip, &output, policy).unwrap();
        assert_eq!(summary.retained_posts, expected_posts);
        assert_eq!(summary.post_status_counts["missing_date"], 1);
        assert_eq!(summary.post_status_counts["malformed_date"], 2);
        assert_eq!(summary.date_policy, policy);
        assert_eq!(
            summary.original_writing_date_minimum.as_deref(),
            policy.minimum()
        );
        let source: Value =
            serde_json::from_slice(&fs::read(output.join("source.json")).unwrap()).unwrap();
        assert_eq!(source["date_policy"]["version"], policy.identity());
        assert_eq!(
            source["date_policy"]["minimum_inclusive"],
            serde_json::to_value(policy.minimum()).unwrap()
        );
        let db = Connection::open(output.join("corpus.sqlite")).unwrap();
        let saved_policy: String = db
            .query_row(
                "SELECT value FROM metadata WHERE key='date_policy'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(saved_policy, policy.identity());
        let saved_schema: String = db
            .query_row("SELECT value FROM metadata WHERE key='schema'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(saved_schema, "unslop-blog-corpus-v1");
        if policy == DatePolicy::Legacy1999 {
            assert_eq!(summary.post_status_counts["before_1999"], 1);
            let mut old_summary = serde_json::to_value(&summary).unwrap();
            old_summary.as_object_mut().unwrap().remove("date_policy");
            let restored: import::Summary = serde_json::from_value(old_summary).unwrap();
            assert_eq!(
                restored, summary,
                "old summaries imply their original 1999 policy"
            );
        } else {
            assert!(!summary.post_status_counts.contains_key("before_1999"));
        }
    }
}
