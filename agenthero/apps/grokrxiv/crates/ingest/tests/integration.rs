//! Network-free integration tests for the ingest crate.

use grokrxiv_ingest::{extract_bibliography, parse_atom, split_sections};

const ATOM_FIXTURE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <link href="http://arxiv.org/api/query?id_list=2401.12345" rel="self" type="application/atom+xml"/>
  <title type="html">ArXiv Query: search_query=&amp;id_list=2401.12345</title>
  <id>http://arxiv.org/api/abc</id>
  <updated>2024-01-01T00:00:00-05:00</updated>
  <opensearch:totalResults xmlns:opensearch="http://a9.com/-/spec/opensearch/1.1/">1</opensearch:totalResults>
  <entry>
    <id>http://arxiv.org/abs/2401.12345v1</id>
    <updated>2024-01-15T00:00:00Z</updated>
    <published>2024-01-15T00:00:00Z</published>
    <title>Quantum Gravity and Modular Composition of Physical Laws</title>
    <summary>We introduce a modular framework that composes size-aware, thermal, quantum, and gravitational laws hierarchically.</summary>
    <author>
      <name>Alice Researcher</name>
      <arxiv:affiliation xmlns:arxiv="http://arxiv.org/schemas/atom">MIT</arxiv:affiliation>
    </author>
    <author>
      <name>Bob Scientist</name>
    </author>
    <link href="http://arxiv.org/abs/2401.12345v1" rel="alternate" type="text/html"/>
    <link title="pdf" href="http://arxiv.org/pdf/2401.12345v1" rel="related" type="application/pdf"/>
    <arxiv:primary_category xmlns:arxiv="http://arxiv.org/schemas/atom" term="hep-th" scheme="http://arxiv.org/schemas/atom"/>
    <category term="hep-th" scheme="http://arxiv.org/schemas/atom"/>
    <category term="gr-qc" scheme="http://arxiv.org/schemas/atom"/>
  </entry>
</feed>
"#;

#[test]
fn parses_arxiv_atom_fixture() {
    let meta = parse_atom("2401.12345", ATOM_FIXTURE).expect("parse atom");
    assert_eq!(meta.arxiv_id, "2401.12345");
    assert_eq!(
        meta.title,
        "Quantum Gravity and Modular Composition of Physical Laws"
    );
    assert_eq!(meta.authors.len(), 2);
    assert_eq!(meta.authors[0].name, "Alice Researcher");
    assert_eq!(meta.authors[0].affiliation.as_deref(), Some("MIT"));
    assert_eq!(meta.authors[1].name, "Bob Scientist");
    assert!(meta
        .abstract_text
        .starts_with("We introduce a modular framework"));
    assert_eq!(
        meta.pdf_url.as_deref(),
        Some("http://arxiv.org/pdf/2401.12345v1")
    );
    assert_eq!(
        meta.source_url.as_deref(),
        Some("http://arxiv.org/abs/2401.12345v1")
    );
    assert!(meta.categories.contains(&"hep-th".to_string()));
    assert!(meta.categories.contains(&"gr-qc".to_string()));
}

#[test]
fn section_splitter_handles_simple_layout() {
    let txt = "Introduction\nThis section sets the stage.\n\nMethods\nWe describe approach.\n\nResults\nWe observe X.";
    let secs = split_sections(txt);
    let headings: Vec<_> = secs.iter().map(|s| s.heading.as_str()).collect();
    assert_eq!(headings, vec!["Introduction", "Methods", "Results"]);
}

#[test]
fn bibliography_extracts_doi_and_arxiv_id() {
    let txt = "Body of paper.\n\nReferences\n[1] Smith J. Title. doi:10.1234/foo.bar (2020).\n[2] Doe A. Other. arXiv:2401.12345 (2024).\n";
    let cites = extract_bibliography(txt);
    assert_eq!(cites.len(), 2);
    assert_eq!(cites[0].doi.as_deref(), Some("10.1234/foo.bar"));
    assert_eq!(cites[1].arxiv_id.as_deref(), Some("2401.12345"));
}
