//! Integration tests for the render crate.

use grokrxiv_render::{build_zip, render_html, render_latex, render_markdown, AgentRecord};
use grokrxiv_schemas::{
    Author, Citation, MetaReview, PaperExtract, Recommendation, Section, VerifierResult,
    VerifierStatus,
};
use serde_json::json;

fn fixture() -> (MetaReview, PaperExtract, Vec<AgentRecord>) {
    let paper = PaperExtract {
        arxiv_id: "2401.12345v1".into(),
        title: "Modular Composition of Physical Laws".into(),
        authors: vec![
            Author {
                name: "Alice Researcher".into(),
                affiliation: Some("MIT".into()),
                email: None,
            },
            Author {
                name: "Bob Scientist".into(),
                affiliation: None,
                email: None,
            },
        ],
        abstract_: "We propose a modular framework composing size-aware, thermal, quantum, and gravitational laws.".into(),
        field: Some("hep-th".into()),
        sections: vec![Section {
            heading: "1 Introduction".into(),
            body_markdown: "We motivate the modular framework.".into(),
        }],
        figures: vec![],
        bibliography: vec![Citation {
            raw: "Smith J. Foundations. doi:10.1234/abc.".into(),
            doi: Some("10.1234/abc".into()),
            arxiv_id: None,
            title: None,
        }],
        source_format: Some("pdf".into()),
    };

    let meta = MetaReview {
        summary: "The paper composes four laws hierarchically with strong evidence.".into(),
        strengths: vec![
            "Clear hierarchical structure".into(),
            "Reproducible math".into(),
        ],
        weaknesses: vec!["Limited empirical scope".into()],
        questions: vec!["How does this extend to non-equilibrium regimes?".into()],
        revision_targets: vec![],
        recommendation: Recommendation::MinorRevision,
        confidence: 0.82,
    };

    let agents = vec![
        AgentRecord {
            role: "summary".to_string(),
            model: "claude-opus-4-7".into(),
            output: json!({"plain_summary": "Short summary."}),
            verifier: VerifierResult {
                status: VerifierStatus::Pass,
                notes: json!({}),
            },
        },
        AgentRecord {
            role: "technical_correctness".to_string(),
            model: "claude-opus-4-7".into(),
            output: json!({"soundness_score": 4}),
            verifier: VerifierResult {
                status: VerifierStatus::Warn,
                notes: json!({"note": "minor"}),
            },
        },
    ];
    (meta, paper, agents)
}

/// Policy lock: the legal disclaimer must NEVER appear in rendered HTML/MD/TeX
/// review artifacts. The single source of truth is the `/legal` page on the
/// web app. Tests below assert *absence* across all three renderers.
const DISCLAIMER_NEEDLE: &str = "Not official peer review";

#[test]
fn html_omits_disclaimer_and_shows_title() {
    let (meta, paper, agents) = fixture();
    let html = render_html(&meta, &paper, &agents).expect("render html");
    assert!(
        !html.contains(DISCLAIMER_NEEDLE),
        "render_html must NOT inline the legal disclaimer; it belongs on /legal only"
    );
    assert!(html.contains("Modular Composition of Physical Laws"));
    assert!(html.contains("arXiv:2401.12345v1"));
    assert!(html.contains("Corrections"));
    assert!(html.contains("summary"));
    assert!(html.contains("technical_correctness"));
    // Snapshot the body (deterministic).
    insta::assert_snapshot!(html);
}

#[test]
fn markdown_omits_disclaimer_and_shows_corrections() {
    let (meta, paper, agents) = fixture();
    let md = render_markdown(&meta, &paper, &agents);
    assert!(
        !md.contains(DISCLAIMER_NEEDLE),
        "render_markdown must NOT inline the legal disclaimer"
    );
    assert!(md.contains("## Corrections"));
}

#[test]
fn local_source_artifacts_do_not_render_arxiv_prefix() {
    let (meta, mut paper, agents) = fixture();
    paper.arxiv_id = "local-pdf-d96363843fd8".into();
    paper.source_format = Some("pdf".into());

    let html = render_html(&meta, &paper, &agents).expect("render html");
    let md = render_markdown(&meta, &paper, &agents);
    let tex = render_latex(&meta, &paper, &agents);

    for artifact in [&html, &md, &tex] {
        assert!(artifact.contains("local-pdf-d96363843fd8"));
        assert!(!artifact.contains("arXiv:local-pdf-d96363843fd8"));
        assert!(!artifact.contains("arxiv.org/abs/local-pdf-d96363843fd8"));
    }
}

#[test]
fn html_renders_meta_reviewer_as_human_text_not_json() {
    let (meta, paper, _) = fixture();
    let agents = vec![AgentRecord {
        role: "meta_reviewer".to_string(),
        model: "preview".into(),
        output: serde_json::to_value(&meta).expect("meta review json"),
        verifier: VerifierResult {
            status: VerifierStatus::Fail,
            notes: json!({ "preview": true }),
        },
    }];

    let html = render_html(&meta, &paper, &agents).expect("render html");

    assert!(html.contains("meta_reviewer"));
    assert!(html.contains("Recommendation: <strong>Minor revision</strong>"));
    assert!(html.contains("Clear hierarchical structure"));
    assert!(html.contains("How does this extend to non-equilibrium regimes?"));
    assert!(
        !html.contains("&quot;recommendation&quot;"),
        "meta reviewer output should not be shown as raw JSON in HTML"
    );
}

#[test]
fn latex_omits_disclaimer_and_balanced_braces() {
    let (meta, paper, agents) = fixture();
    let tex = render_latex(&meta, &paper, &agents);
    assert!(
        !tex.contains(DISCLAIMER_NEEDLE),
        "render_latex must NOT inline the legal disclaimer"
    );
    assert!(tex.contains("\\begin{document}"));
    assert!(tex.contains("\\end{document}"));
    // `\begin{}` count == `\end{}` count.
    let begins = tex.matches("\\begin{").count();
    let ends = tex.matches("\\end{").count();
    assert_eq!(begins, ends, "unbalanced LaTeX environments");
}

#[test]
fn latex_escapes_agent_json_instead_of_using_raw_verbatim() {
    let (meta, paper, _) = fixture();
    let agents = vec![AgentRecord {
        role: "malicious".to_string(),
        model: "test".into(),
        output: json!({
            "payload": "\\end{verbatim}\n\\input{/tmp/evil}\n\\begin{verbatim}"
        }),
        verifier: VerifierResult {
            status: VerifierStatus::Warn,
            notes: json!({}),
        },
    }];

    let tex = render_latex(&meta, &paper, &agents);

    assert!(!tex.contains("\\begin{verbatim}"));
    assert!(!tex.contains("\\end{verbatim}"));
    assert!(!tex.contains("\\input{/tmp/evil}"));
    assert!(tex.contains("\\textbackslash{}input\\{/tmp/evil\\}"));
}

#[test]
fn disclaimer_lives_on_the_web_legal_page() {
    // Companion to the negative renderer tests. We don't try to load Next.js
    // here — we just verify the disclaimer text is present in the `/legal`
    // source file so a careless future edit that strips it from `/legal`
    // (the single surface that should carry it) fails the test suite.
    let legal_src = include_str!("../../../web/app/legal/page.tsx");
    assert!(
        legal_src.contains("GrokRxiv reviews are AI-generated"),
        "/legal page must carry the AI-generated disclosure"
    );
    assert!(
        legal_src.contains("not endorsed by arXiv"),
        "/legal page must carry the arXiv-non-endorsement statement"
    );
}

#[test]
fn zip_bundle_contains_expected_entries() {
    let (meta, paper, agents) = fixture();
    let html = render_html(&meta, &paper, &agents).unwrap();
    let md = render_markdown(&meta, &paper, &agents);
    let tex = render_latex(&meta, &paper, &agents);
    let agent_jsons: Vec<_> = agents
        .iter()
        .map(|a| (a.filename(), serde_json::to_vec_pretty(&a.output).unwrap()))
        .collect();
    let metadata = json!({"arxiv_id": paper.arxiv_id, "title": paper.title});

    let bytes = build_zip(&html, &md, &tex, None, &agent_jsons, &metadata).expect("zip");
    assert!(bytes.starts_with(&[0x50, 0x4B])); // PK..

    // Re-read with zip crate to verify entries.
    let reader = std::io::Cursor::new(&bytes);
    let mut zip = zip::ZipArchive::new(reader).expect("zip archive");
    let names: Vec<String> = (0..zip.len())
        .map(|i| zip.by_index(i).unwrap().name().to_string())
        .collect();
    assert!(names.contains(&"review.html".to_string()));
    assert!(names.contains(&"review.md".to_string()));
    assert!(names.contains(&"review.tex".to_string()));
    assert!(names.contains(&"metadata.json".to_string()));
    assert!(names.iter().any(|n| n.starts_with("agents/")));
}
