use unslop::features;

#[test]
fn installed_spacy_bridge_preserves_rust_word_counts_and_adds_parse_features() {
    let python = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(".venv/bin/python");
    if !python.exists() {
        eprintln!("spaCy integration skipped: local .venv absent");
        return;
    }
    let text = "The sample was measured by the researcher. We didn't change the method.";
    let lexical = features::extract(text, false, python.to_str().unwrap()).unwrap();
    let grammar = features::extract(text, true, python.to_str().unwrap()).unwrap();
    assert_eq!(lexical["families"]["word"], grammar["families"]["word"]);
    assert_eq!(grammar["families"]["construction"]["passive_sentence"], 1);
    assert_eq!(grammar["families"]["construction"]["negation_sentence"], 1);
    assert!(grammar["extractor"].as_str().unwrap().contains("spacy="));
}
