//! Registry for Rust-native DAG tool handlers.
//!
//! The generic DAG manifest layer stores tool ids and handler names as data.
//! This module is the Rust side of that contract: a handler must appear here
//! before a manifest can claim it is executable by the orchestrator.

use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RustToolDescriptor {
    pub handler: &'static str,
    pub module: &'static str,
    pub description: &'static str,
}

const RUST_TOOL_HANDLERS: &[RustToolDescriptor] = &[
    RustToolDescriptor {
        handler: "source_to_body",
        module: "ingest_pipeline",
        description: "Convert source/PDF inputs into body.md and semantic_ast.json.",
    },
    RustToolDescriptor {
        handler: "derive_metadata",
        module: "ingest_pipeline",
        description: "Derive metadata.json from the staged paper extract.",
    },
    RustToolDescriptor {
        handler: "derive_sections",
        module: "ingest_pipeline",
        description: "Derive sections.json from body.md.",
    },
    RustToolDescriptor {
        handler: "derive_equations",
        module: "ingest_pipeline",
        description: "Derive equations.json from Markdown or semantic AST.",
    },
    RustToolDescriptor {
        handler: "derive_references",
        module: "ingest_pipeline",
        description: "Derive references.json and citation contexts.",
    },
    RustToolDescriptor {
        handler: "derive_theorems",
        module: "ingest_pipeline",
        description: "Derive theorem_graph.json from Markdown or semantic AST.",
    },
    RustToolDescriptor {
        handler: "citation_validation::bibtex_reference_parser",
        module: "citation_validation",
        description: "Parse BibTeX/reference entries and normalize reference keys.",
    },
    RustToolDescriptor {
        handler: "citation_validation::doi_resolver",
        module: "citation_validation",
        description: "Resolve DOI/OpenAlex/Crossref metadata for parsed references.",
    },
    RustToolDescriptor {
        handler: "citation_validation::semantic_similarity_check",
        module: "citation_validation",
        description: "Score citation context similarity against resolved metadata.",
    },
    RustToolDescriptor {
        handler: "citation_validation::metadata_consistency_validator",
        module: "citation_validation",
        description: "Compare BibTeX metadata against resolver metadata.",
    },
    RustToolDescriptor {
        handler: "citation_validation::citation_graph_validation",
        module: "citation_validation",
        description: "Validate cited/uncited/unmatched citation graph consistency.",
    },
    RustToolDescriptor {
        handler: "publisher::upload_review_bundle",
        module: "publisher",
        description: "Upload rendered review artifacts and metadata for publication.",
    },
    RustToolDescriptor {
        handler: "publisher::open_review_pr",
        module: "publisher",
        description: "Open or update the publication pull request for an approved review.",
    },
    RustToolDescriptor {
        handler: "review_loop::claim_extractor",
        module: "review_loop",
        description: "Extract load-bearing claims from persisted review outputs.",
    },
    RustToolDescriptor {
        handler: "review_loop::knowledge_graph_builder",
        module: "review_loop",
        description: "Build the review-loop knowledge graph artifact from extracted claims.",
    },
    RustToolDescriptor {
        handler: "review_loop::semantic_category_mapper",
        module: "review_loop",
        description: "Materialize the Haskell semantic model and JSON semantic model.",
    },
    RustToolDescriptor {
        handler: "review_loop::review_fix_code",
        module: "review_loop",
        description: "Bounded generate, compile/verify, review, fix, and retry primitive.",
    },
    RustToolDescriptor {
        handler: "review_loop::proof_obligation_generator",
        module: "review_loop",
        description: "Generate Lean proof obligations from semantic-model evidence.",
    },
    RustToolDescriptor {
        handler: "review_loop::pr_fixer",
        module: "review_loop",
        description: "Create corrected PR artifacts in an isolated artifact worktree.",
    },
    RustToolDescriptor {
        handler: "review_loop::policy_gate",
        module: "review_loop",
        description: "Apply deterministic review-loop publication policy.",
    },
    RustToolDescriptor {
        handler: "review_loop::review_loop_report",
        module: "review_loop",
        description: "Persist the final review-loop report artifact.",
    },
    RustToolDescriptor {
        handler: "review_loop::publish_decision",
        module: "review_loop",
        description: "Convert deterministic policy output into an explicit publish decision.",
    },
];

#[derive(Debug, Clone)]
pub(crate) struct ToolCatalog {
    handlers: BTreeMap<&'static str, RustToolDescriptor>,
}

impl ToolCatalog {
    pub(crate) fn default_rust() -> Self {
        let handlers = RUST_TOOL_HANDLERS
            .iter()
            .map(|handler| (handler.handler, *handler))
            .collect();
        Self { handlers }
    }

    pub(crate) fn get(&self, handler: &str) -> Option<RustToolDescriptor> {
        self.handlers.get(handler).copied()
    }

    pub(crate) fn names(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.handlers.keys().copied()
    }
}

pub(crate) fn is_known_rust_tool_handler(handler: &str) -> bool {
    ToolCatalog::default_rust().get(handler).is_some()
}

pub(crate) fn known_rust_tool_handlers() -> Vec<&'static str> {
    ToolCatalog::default_rust().names().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_contains_builtin_and_citation_validation_handlers() {
        assert!(is_known_rust_tool_handler("derive_references"));
        assert!(is_known_rust_tool_handler(
            "citation_validation::metadata_consistency_validator"
        ));
        assert!(is_known_rust_tool_handler("review_loop::policy_gate"));
        assert!(!is_known_rust_tool_handler("citation_validation::missing"));
    }

    #[test]
    fn catalog_describes_handlers_for_future_executor_dispatch() {
        let catalog = ToolCatalog::default_rust();
        let descriptor = catalog
            .get("citation_validation::doi_resolver")
            .expect("doi resolver registered");
        assert_eq!(descriptor.module, "citation_validation");
        assert!(descriptor.description.contains("Crossref"));
    }
}
