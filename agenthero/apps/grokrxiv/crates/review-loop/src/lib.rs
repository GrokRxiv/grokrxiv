use serde_json::json;
use uuid::Uuid;

pub fn build_semantic_ir(
    review_id: Uuid,
    claims_value: &serde_json::Value,
    knowledge_graph: &serde_json::Value,
) -> serde_json::Value {
    let claims = claims_value
        .get("claims")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let mut theorem_candidates = Vec::new();
    let mut definitions = Vec::new();
    let mut assumptions = Vec::new();
    let limitations = Vec::<serde_json::Value>::new();
    let mut nonformal_review_claims = Vec::new();

    for claim in claims {
        let id = claim_id(&claim);
        let role = claim
            .get("role")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let text = claim_text(&claim);
        if text.trim().is_empty() || is_review_metadata_only(&text) {
            continue;
        }
        let lower = text.to_ascii_lowercase();
        let source_span = json!({
            "artifact": "review_loop/claims.json",
            "claim_id": id,
            "role": role,
            "text_excerpt": truncate(&text, 260),
        });
        let is_formal_math = looks_like_formal_math_statement(&text, &lower);
        if !is_formal_math && is_nonformal_review_role(role) {
            nonformal_review_claims.push(nonformal_claim(
                &id,
                text,
                source_span,
                "review_or_evidence_claim_not_a_formal_mathematical_statement",
            ));
            continue;
        }
        if looks_like_assumption(&lower) {
            assumptions.push(json!({
                "id": format!("assumption_{id}"),
                "statement": text,
                "source_claim_id": id,
                "source_span": source_span,
            }));
        } else if looks_like_definition(&lower) {
            definitions.push(json!({
                "id": format!("definition_{id}"),
                "statement": text,
                "source_claim_id": id,
                "source_span": source_span,
            }));
        } else if is_formal_math {
            let theorem_id = format!("theorem_{id}");
            theorem_candidates.push(json!({
                "id": theorem_id,
                "kind": formal_math_kind(&text, &lower),
                "formalization_class": "formal_math",
                "statement": text,
                "source_claim_id": id,
                "source_span": source_span,
                "typed_transcription": {
                    "status": "needs_haskell_transcription",
                    "source_text": text,
                    "math_objects": [],
                    "binders": [],
                    "assumptions": [],
                    "conclusion": null
                },
                "dependencies": [],
                "formalization_target": {
                    "lean_declaration": format!("{}_formalized", lean_identifier(&theorem_id)),
                    "expected_shape": "theorem",
                    "proof_policy": "closed_proof_no_sorry_admit_axiom"
                }
            }));
        } else {
            nonformal_review_claims.push(nonformal_claim(
                &id,
                text,
                source_span,
                "no_formal_math_statement_detected",
            ));
        }
    }

    json!({
        "schema_version": "1.0.0",
        "review_id": review_id,
        "source": "review_loop/claims.json",
        "formalization_policy": {
            "requires_theorem_level_lean": true,
            "lean_verifies_only_formal_math": true,
            "reject_metadata_only_models": true,
            "reject_review_role_histograms": true,
            "forbidden_lean_terms": ["sorry", "admit", "axiom"]
        },
        "definitions": definitions,
        "assumptions": assumptions,
        "theorem_candidates": theorem_candidates,
        "nonformal_review_claims": nonformal_review_claims,
        "limitations": limitations,
        "knowledge_graph_summary": {
            "nodes": knowledge_graph["nodes"].as_array().map(Vec::len).unwrap_or(0),
            "edges": knowledge_graph["edges"].as_array().map(Vec::len).unwrap_or(0)
        }
    })
}

pub fn build_proof_obligations(
    review_id: Uuid,
    semantic_ir: &serde_json::Value,
    haskell_results: &serde_json::Value,
) -> serde_json::Value {
    let haskell_status = haskell_results
        .get("status")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    if haskell_status != "pass" {
        return json!({
            "schema_version": "1.0.0",
            "review_id": review_id,
            "source": "review_loop/semantic_ir.json",
            "haskell_status": haskell_status,
            "obligations": [
                {
                    "id": "semantic_gap_haskell_model_failed",
                    "kind": "semantic_gap",
                    "statement": "Haskell mathematical IR generation did not pass; Lean verification is blocked.",
                    "lean_declaration": null,
                    "severity": "blocking",
                    "upstream_artifact": "review_loop/haskell/results.json",
                    "upstream_summary": review_fix_loop_summary(haskell_results),
                }
            ]
        });
    }

    let theorem_candidates = semantic_ir
        .get("theorem_candidates")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let mut obligations = Vec::new();
    for theorem in theorem_candidates {
        if theorem.get("formalization_class").and_then(|v| v.as_str()) != Some("formal_math") {
            continue;
        }
        let theorem_id = theorem
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("theorem");
        let lean_declaration = theorem
            .get("formalization_target")
            .and_then(|target| target.get("lean_declaration"))
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| format!("{}_formalized", lean_identifier(theorem_id)));
        obligations.push(json!({
            "id": format!("formalize_{theorem_id}"),
            "kind": "theorem_formalization",
            "statement": theorem.get("statement").cloned().unwrap_or_else(|| json!("")),
            "source_claim_id": theorem.get("source_claim_id").cloned().unwrap_or_else(|| json!(null)),
            "source_span": theorem.get("source_span").cloned().unwrap_or_else(|| json!(null)),
            "lean_declaration": lean_declaration,
            "severity": "blocking",
            "expected_proof": "closed Lean theorem proof with no sorry, admit, or unapproved axiom",
        }));
    }
    if obligations.is_empty() {
        obligations.push(json!({
            "id": "semantic_gap_no_formal_math_statements",
            "kind": "semantic_gap",
            "statement": "No paper-derived formal mathematical statements were extracted for Lean verification.",
            "lean_declaration": null,
            "severity": "blocking",
        }));
    }
    json!({
        "schema_version": "1.0.0",
        "review_id": review_id,
        "source": "review_loop/semantic_ir.json",
        "haskell_status": haskell_status,
        "obligations": obligations,
    })
}

pub fn proof_obligations_require_lean(proof_obligations: &serde_json::Value) -> bool {
    proof_obligations
        .get("obligations")
        .and_then(|v| v.as_array())
        .map(|items| {
            items.iter().any(|item| {
                item.get("kind").and_then(|v| v.as_str()) == Some("theorem_formalization")
            })
        })
        .unwrap_or(false)
}

pub fn build_lean_targets(proof_obligations: &serde_json::Value) -> serde_json::Value {
    let targets = proof_obligations
        .get("obligations")
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
        .filter(|item| item.get("kind").and_then(|v| v.as_str()) == Some("theorem_formalization"))
        .map(|item| {
            json!({
                "obligation_id": item.get("id").cloned().unwrap_or_else(|| json!(null)),
                "lean_declaration": item.get("lean_declaration").cloned().unwrap_or_else(|| json!(null)),
                "statement": item.get("statement").cloned().unwrap_or_else(|| json!("")),
                "source_claim_id": item.get("source_claim_id").cloned().unwrap_or_else(|| json!(null)),
                "source_span": item.get("source_span").cloned().unwrap_or_else(|| json!(null)),
            })
        })
        .collect::<Vec<_>>();
    json!({
        "schema_version": "1.0.0",
        "source": "review_loop/proof_obligations.json",
        "targets": targets,
    })
}

pub fn build_theorem_map(
    proof_obligations: &serde_json::Value,
    lean_results: &serde_json::Value,
) -> serde_json::Value {
    let obligations = proof_obligations
        .get("obligations")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let entries = obligations
        .into_iter()
        .map(|obligation| {
            let kind = obligation
                .get("kind")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            let status = lean_entry_status(kind, lean_results);
            json!({
                "obligation_id": obligation.get("id").cloned().unwrap_or_else(|| json!(null)),
                "kind": kind,
                "source_claim_id": obligation.get("source_claim_id").cloned().unwrap_or_else(|| json!(null)),
                "source_span": obligation.get("source_span").cloned().unwrap_or_else(|| json!(null)),
                "lean_declaration": obligation.get("lean_declaration").cloned().unwrap_or_else(|| json!(null)),
                "status": status,
                "statement": obligation.get("statement").cloned().unwrap_or_else(|| json!("")),
            })
        })
        .collect::<Vec<_>>();
    let top_status = entries
        .iter()
        .map(|entry| {
            entry
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("FAILED")
        })
        .find(|status| *status != "PROVED")
        .unwrap_or("PROVED");
    json!({
        "schema_version": "1.0.0",
        "source": "review_loop/proof_obligations.json",
        "lean_results": "review_loop/lean/results.json",
        "status": top_status,
        "entries": entries,
    })
}

pub fn build_semantic_adequacy(
    semantic_ir: &serde_json::Value,
    theorem_map: &serde_json::Value,
) -> serde_json::Value {
    let theorem_candidates = semantic_ir
        .get("theorem_candidates")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let theorem_entries = theorem_map
        .get("entries")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let mut verdicts = Vec::new();
    for theorem in theorem_candidates {
        let source_claim_id = theorem
            .get("source_claim_id")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        let matching_entry = theorem_entries.iter().find(|entry| {
            entry.get("source_claim_id").and_then(|v| v.as_str()) == Some(source_claim_id)
        });
        let proof_status = matching_entry
            .and_then(|entry| entry.get("status"))
            .and_then(|v| v.as_str())
            .unwrap_or("FAILED");
        let verdict = if proof_status == "PROVED" {
            "MATCHES"
        } else {
            "OVERCLAIMED"
        };
        verdicts.push(json!({
            "claim_id": source_claim_id,
            "theorem_id": theorem.get("id").cloned().unwrap_or_else(|| json!(null)),
            "lean_declaration": matching_entry
                .and_then(|entry| entry.get("lean_declaration").cloned())
                .unwrap_or_else(|| json!(null)),
            "proof_status": proof_status,
            "verdict": verdict,
            "statement": theorem.get("statement").cloned().unwrap_or_else(|| json!("")),
        }));
    }
    let pass = !verdicts.is_empty()
        && verdicts
            .iter()
            .all(|verdict| verdict.get("verdict").and_then(|v| v.as_str()) == Some("MATCHES"));
    json!({
        "schema_version": "1.0.0",
        "source": "review_loop/semantic_ir.json",
        "theorem_map": "review_loop/lean/theorem_map.json",
        "status": if pass { "pass" } else { "fail" },
        "verdicts": verdicts,
    })
}

pub fn validate_generated_code(
    target_id: &str,
    code: &str,
    base_artifact: &serde_json::Value,
) -> Vec<String> {
    match target_id {
        "haskell" => validate_haskell_semantic_model_code(
            code,
            base_artifact
                .get("semantic_ir")
                .unwrap_or(&serde_json::Value::Null),
        ),
        "lean" => validate_lean_proof_code(
            code,
            base_artifact
                .get("proof_obligations")
                .unwrap_or(&serde_json::Value::Null),
        ),
        _ => Vec::new(),
    }
}

pub fn validate_haskell_semantic_model_code(
    code: &str,
    semantic_ir: &serde_json::Value,
) -> Vec<String> {
    let mut issues = Vec::new();
    if code.contains("data ReviewRole")
        || code.contains("categoryCounts")
        || code.contains("publisherReadyLowerBound")
    {
        issues.push(
            "Generated Haskell looks like a review-claim inventory; encode typed paper mathematics instead."
                .to_string(),
        );
    }
    for required_type in [
        "SourceSpan",
        "MathType",
        "Term",
        "Proposition",
        "Binder",
        "TheoremIR",
        "ClaimIR",
        "Definition",
        "Assumption",
        "ProofObligation",
        "LeanTarget",
    ] {
        if !code.contains(required_type) {
            issues.push(format!(
                "SemanticModel.hs must define typed mathematical IR type {required_type}."
            ));
        }
    }
    for required_function in [
        "categoryToObligations",
        "claimToObligations",
        "obligationToLean",
    ] {
        if !code.contains(required_function) {
            issues.push(format!(
                "SemanticModel.hs must define mathematical IR mapping function {required_function}."
            ));
        }
    }
    if !(code.contains("SourceSpan") || code.contains("sourceSpan")) {
        issues.push("SemanticModel.hs must preserve source spans for paper math.".to_string());
    }
    if !(code.contains("Assumption") || code.contains("assumptions")) {
        issues.push("SemanticModel.hs must model theorem assumptions.".to_string());
    }
    let theorem_candidates = semantic_ir
        .get("theorem_candidates")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    for theorem in theorem_candidates {
        if theorem.get("formalization_class").and_then(|v| v.as_str()) != Some("formal_math") {
            issues.push(
                "SemanticModel.hs input contains a Lean theorem candidate that is not classified as formal_math."
                    .to_string(),
            );
        }
        if let Some(lean_decl) = theorem
            .get("formalization_target")
            .and_then(|target| target.get("lean_declaration"))
            .and_then(|v| v.as_str())
        {
            if !code.contains(lean_decl) {
                issues.push(format!(
                    "SemanticModel.hs must include Lean target declaration {lean_decl}."
                ));
            }
        }
    }
    issues
}

pub fn validate_lean_proof_code(code: &str, obligations: &serde_json::Value) -> Vec<String> {
    let mut issues = Vec::new();
    let lower = code.to_ascii_lowercase();
    for forbidden in ["sorry", "admit", "axiom"] {
        if lower.contains(forbidden) {
            issues.push(format!("Lean proof uses forbidden term {forbidden}."));
        }
    }
    let obligation_items = obligations
        .get("obligations")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let theorem_obligations = obligation_items
        .iter()
        .filter(|item| item.get("kind").and_then(|v| v.as_str()) == Some("theorem_formalization"))
        .collect::<Vec<_>>();
    if theorem_obligations.is_empty() {
        issues.push("Lean obligations contain no theorem formalization targets.".to_string());
    }
    for obligation in theorem_obligations {
        if let Some(decl) = obligation.get("lean_declaration").and_then(|v| v.as_str()) {
            let theorem_decl = format!("theorem {decl}");
            let lemma_decl = format!("lemma {decl}");
            if !code.contains(&theorem_decl) && !code.contains(&lemma_decl) {
                issues.push(format!(
                    "Lean proof is metadata-only or missing theorem declaration {decl}."
                ));
            }
        }
    }
    if lower.contains("claimcount") || lower.contains("claim_count") {
        issues.push(
            "Lean proof is metadata-only; claim counts are not theorem formalization.".to_string(),
        );
    }
    issues
}

fn nonformal_claim(
    id: &str,
    statement: String,
    source_span: serde_json::Value,
    reason: &str,
) -> serde_json::Value {
    json!({
        "id": format!("nonformal_{id}"),
        "kind": "nonformal_review_claim",
        "statement": statement,
        "source_claim_id": id,
        "source_span": source_span,
        "lean_eligible": false,
        "reason": reason,
    })
}

fn claim_id(claim: &serde_json::Value) -> String {
    claim
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("claim")
        .to_string()
}

fn claim_text(claim: &serde_json::Value) -> String {
    claim
        .get("text")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .trim()
        .to_string()
}

fn is_review_metadata_only(text: &str) -> bool {
    let normalized = text.trim().to_ascii_lowercase().replace('-', "_");
    matches!(
        normalized.as_str(),
        "major_revision"
            | "minor_revision"
            | "accept"
            | "reject"
            | "prior_art"
            | "builds_on"
            | "significant"
            | "questionable"
    )
}

fn is_nonformal_review_role(role: &str) -> bool {
    role.contains("summary")
        || role.contains("meta")
        || role.contains("citation")
        || role.contains("novelty")
        || role.contains("reproducibility")
}

fn looks_like_assumption(lower: &str) -> bool {
    lower.contains("assumes")
        || lower.contains("assume ")
        || lower.contains("assuming")
        || lower.contains("under the assumption")
        || lower.contains("condition")
        || lower.contains("requires")
}

fn looks_like_definition(lower: &str) -> bool {
    lower.contains("definition")
        || lower.contains("defines")
        || lower.contains("defined as")
        || lower.contains("structure")
}

fn looks_like_formal_math_statement(text: &str, lower: &str) -> bool {
    let has_quantifier = lower.contains(" for all ")
        || lower.contains("forall")
        || text.contains('∀')
        || lower.contains(" exists ")
        || text.contains('∃');
    let has_relation = text.contains('=')
        || text.contains('≤')
        || text.contains('≥')
        || lower.contains("\\le")
        || lower.contains("\\ge")
        || lower.contains(" less than ")
        || lower.contains(" greater than ");
    let has_named_statement = ["theorem", "lemma", "proposition", "corollary"]
        .iter()
        .any(|marker| lower.contains(marker))
        && (text.contains(':') || has_quantifier || has_relation);
    let has_structural_math = ["invariant", "bound", "equivalence", "unique", "exists"]
        .iter()
        .any(|marker| lower.contains(marker))
        && (has_quantifier || has_relation || text.contains(':'));

    has_named_statement || has_quantifier || has_relation || has_structural_math
}

fn formal_math_kind(text: &str, lower: &str) -> &'static str {
    if lower.contains("lemma") {
        "lemma"
    } else if lower.contains("equivalence") || lower.contains("equivalent") {
        "equivalence"
    } else if lower.contains("invariant") {
        "invariant"
    } else if lower.contains("bound") || text.contains('≤') || text.contains('≥') {
        "bound"
    } else if text.contains('=') {
        "equation"
    } else {
        "theorem"
    }
}

fn lean_identifier(raw: &str) -> String {
    let mut out = String::new();
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    if out.chars().next().is_some_and(|ch| ch.is_ascii_digit()) {
        out.insert(0, '_');
    }
    while out.contains("__") {
        out = out.replace("__", "_");
    }
    out.trim_matches('_').to_string()
}

fn review_fix_loop_summary(results: &serde_json::Value) -> String {
    let status = results
        .get("status")
        .and_then(|value| value.as_str())
        .unwrap_or("unknown");
    let attempts = results
        .get("attempts")
        .and_then(|value| value.as_array())
        .map(Vec::len)
        .unwrap_or(0);
    let last_issue = results
        .get("attempts")
        .and_then(|value| value.as_array())
        .and_then(|items| items.last())
        .and_then(|attempt| {
            attempt
                .pointer("/semantic_validation/issues/0")
                .or_else(|| attempt.pointer("/compile/stderr"))
                .or_else(|| attempt.pointer("/codex_review/issues/0/message"))
        })
        .and_then(|value| value.as_str())
        .unwrap_or("");
    if last_issue.is_empty() {
        format!("status={status} attempts={attempts}")
    } else {
        format!(
            "status={status} attempts={attempts} issue={}",
            truncate(last_issue, 180)
        )
    }
}

fn lean_entry_status(kind: &str, lean_results: &serde_json::Value) -> &'static str {
    if kind == "semantic_gap" {
        return "SEMANTIC_GAP";
    }
    if lean_results.get("status").and_then(|v| v.as_str()) == Some("pass") {
        return "PROVED";
    }
    let diagnostics = lean_results.to_string().to_ascii_lowercase();
    if diagnostics.contains("sorry") {
        "USES_SORRY"
    } else if diagnostics.contains("axiom") {
        "USES_UNAPPROVED_AXIOM"
    } else if diagnostics.contains("unknown constant")
        || diagnostics.contains("unknown identifier")
        || diagnostics.contains("failed to synthesize")
    {
        "MISSING_DEFINITION"
    } else if diagnostics.contains("type mismatch")
        || diagnostics.contains("application type mismatch")
        || diagnostics.contains("unsolved goals")
    {
        "TYPE_ERROR"
    } else {
        "FAILED"
    }
}

fn truncate(value: &str, max: usize) -> String {
    if value.chars().count() <= max {
        return value.to_string();
    }
    let mut out = value
        .chars()
        .take(max.saturating_sub(1))
        .collect::<String>();
    out.push_str("...");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semantic_ir_only_marks_formal_math_for_lean() {
        let review_id = Uuid::parse_str("76665eba-7670-47ef-b69d-42a0af86eba7").unwrap();
        let claims = json!({
            "review_id": review_id,
            "claims": [
                {
                    "id": "claim_math",
                    "role": "technical_correctness",
                    "text": "The paper proves Theorem 2.1: for all n in N, n + 0 = n.",
                    "verifier_status": "pass"
                },
                {
                    "id": "claim_broad",
                    "role": "summary",
                    "text": "The paper extends Weyl's theorem to non-Lorentzian geometries.",
                    "verifier_status": "pass"
                },
                {
                    "id": "claim_review",
                    "role": "meta_reviewer",
                    "text": "The paper is a significant contribution but requires clearer definitions.",
                    "verifier_status": "warn"
                }
            ]
        });
        let knowledge_graph = json!({"nodes": [], "edges": []});

        let semantic_ir = build_semantic_ir(review_id, &claims, &knowledge_graph);
        let theorem_candidates = semantic_ir["theorem_candidates"]
            .as_array()
            .expect("theorem candidates");
        let nonformal_claims = semantic_ir["nonformal_review_claims"]
            .as_array()
            .expect("nonformal claims");

        assert_eq!(theorem_candidates.len(), 1);
        assert_eq!(theorem_candidates[0]["source_claim_id"], "claim_math");
        assert_eq!(theorem_candidates[0]["formalization_class"], "formal_math");
        assert!(nonformal_claims
            .iter()
            .any(|claim| claim["source_claim_id"] == "claim_broad"));
        assert!(nonformal_claims
            .iter()
            .any(|claim| claim["source_claim_id"] == "claim_review"));
    }

    #[test]
    fn haskell_validator_accepts_math_ir_without_literal_candidate_ids() {
        let semantic_ir = json!({
            "schema_version": "1.0.0",
            "theorem_candidates": [
                {
                    "id": "theorem_claim_3",
                    "kind": "theorem",
                    "formalization_class": "formal_math",
                    "statement": "For all n in N, n + 0 = n.",
                    "source_claim_id": "claim_3",
                    "source_span": {"artifact": "review_loop/claims.json", "claim_id": "claim_3"},
                    "formalization_target": {
                        "lean_declaration": "add_zero_claim",
                        "expected_shape": "theorem"
                    }
                }
            ]
        });
        let math_ir_module = r#"
module SemanticModel where

data SourceSpan = SourceSpan { artifact :: String, claimId :: String } deriving (Eq, Show)
data SemanticCategory = PlainTheorem | Equation | AlgebraicIdentity deriving (Eq, Show)
data MathType = NatType | PropType | CustomType String deriving (Eq, Show)
data Term = Var String | NatLit Integer | Add Term Term deriving (Eq, Show)
data Proposition = Forall String MathType Proposition | Equals Term Term deriving (Eq, Show)
data Binder = Binder { binderName :: String, binderType :: MathType } deriving (Eq, Show)
data Definition = Definition { definitionName :: String } deriving (Eq, Show)
data Assumption = Assumption { assumptionText :: String } deriving (Eq, Show)
data LeanTarget = LeanTarget { leanDeclaration :: String, leanStatement :: Proposition } deriving (Eq, Show)
data ProofObligation = ProofObligation { obligationStatement :: Proposition, leanTarget :: LeanTarget } deriving (Eq, Show)
data TheoremIR = TheoremIR
  { theoremName :: String
  , theoremSpan :: SourceSpan
  , binders :: [Binder]
  , theoremAssumptions :: [Proposition]
  , conclusion :: Proposition
  } deriving (Eq, Show)
data ClaimIR = ClaimIR
  { rawText :: String
  , sourceSpan :: SourceSpan
  , category :: SemanticCategory
  , theoremIR :: TheoremIR
  , assumptions :: [Assumption]
  } deriving (Eq, Show)

categoryToObligations :: SemanticCategory -> ClaimIR -> [ProofObligation]
categoryToObligations _ claim =
  [ProofObligation (conclusion (theoremIR claim)) (obligationToLean (conclusion (theoremIR claim)))]

claimToObligations :: ClaimIR -> [ProofObligation]
claimToObligations claim = categoryToObligations (category claim) claim

obligationToLean :: Proposition -> LeanTarget
obligationToLean prop = LeanTarget "add_zero_claim" prop
"#;

        let issues = validate_haskell_semantic_model_code(math_ir_module, &semantic_ir);

        assert!(issues.is_empty(), "{issues:?}");
    }

    #[test]
    fn haskell_validator_rejects_claim_inventory_module() {
        let semantic_ir = json!({
            "schema_version": "1.0.0",
            "theorem_candidates": [
                {
                    "id": "theorem_claim_1",
                    "kind": "theorem",
                    "formalization_class": "formal_math",
                    "statement": "For all n in N, n + 0 = n.",
                    "source_claim_id": "claim_1",
                    "source_span": {"artifact": "review_loop/claims.json", "claim_id": "claim_1"}
                }
            ]
        });
        let claim_inventory_module = r#"
module SemanticModel where
data ReviewRole = Citation | MetaReviewer | Novelty | Summary | TechnicalCorrectness deriving (Eq, Show)
claimCount :: Int
claimCount = 43
categoryCounts :: [(ReviewRole, Int)]
categoryCounts = [(Citation, 12)]
publisherReadyLowerBound :: Bool
publisherReadyLowerBound = claimCount == 43
"#;

        let issues = validate_haskell_semantic_model_code(claim_inventory_module, &semantic_ir);

        assert!(issues.iter().any(|issue| issue.contains("ClaimIR")));
        assert!(issues
            .iter()
            .any(|issue| issue.contains("typed paper mathematics")));
    }

    #[test]
    fn proof_obligations_only_include_formal_math_targets() {
        let review_id = Uuid::parse_str("76665eba-7670-47ef-b69d-42a0af86eba7").unwrap();
        let semantic_ir = json!({
            "schema_version": "1.0.0",
            "theorem_candidates": [
                {
                    "id": "theorem_claim_1",
                    "kind": "equation",
                    "formalization_class": "formal_math",
                    "statement": "For all n in N, n + 0 = n.",
                    "source_claim_id": "claim_1",
                    "formalization_target": {
                        "lean_declaration": "add_zero_claim",
                        "expected_shape": "theorem"
                    }
                },
                {
                    "id": "theorem_claim_2",
                    "kind": "review",
                    "formalization_class": "nonformal_review_claim",
                    "statement": "The paper is significant.",
                    "source_claim_id": "claim_2"
                }
            ]
        });

        let obligations =
            build_proof_obligations(review_id, &semantic_ir, &json!({"status": "pass"}));
        let obligation_items = obligations["obligations"].as_array().expect("obligations");

        assert_eq!(obligation_items.len(), 1);
        assert_eq!(obligation_items[0]["kind"], "theorem_formalization");
        assert_eq!(obligation_items[0]["lean_declaration"], "add_zero_claim");
    }

    #[test]
    fn haskell_failure_blocks_theorem_formalization_obligations() {
        let review_id = Uuid::parse_str("76665eba-7670-47ef-b69d-42a0af86eba7").unwrap();
        let semantic_ir = json!({
            "schema_version": "1.0.0",
            "theorem_candidates": [
                {
                    "id": "theorem_claim_1",
                    "kind": "theorem",
                    "formalization_class": "formal_math",
                    "statement": "For all n in N, n + 0 = n.",
                    "source_claim_id": "claim_1",
                    "formalization_target": {
                        "lean_declaration": "add_zero_claim",
                        "expected_shape": "theorem"
                    }
                }
            ]
        });

        let obligations = build_proof_obligations(
            review_id,
            &semantic_ir,
            &json!({
                "status": "fail",
                "attempts": [{
                    "semantic_validation": {
                        "issues": ["SemanticModel.hs must define typed mathematical IR type TheoremIR."]
                    }
                }]
            }),
        );
        let obligation_items = obligations["obligations"].as_array().expect("obligations");

        assert_eq!(obligations["haskell_status"], "fail");
        assert_eq!(obligation_items.len(), 1);
        assert_eq!(obligation_items[0]["kind"], "semantic_gap");
        assert_eq!(
            obligation_items[0]["id"],
            "semantic_gap_haskell_model_failed"
        );
    }

    #[test]
    fn lean_validator_rejects_metadata_only_proofs() {
        let obligations = json!({
            "obligations": [
                {
                    "id": "formalize_theorem_claim_1",
                    "kind": "theorem_formalization",
                    "lean_declaration": "add_zero_claim",
                    "statement": "For all n in N, n + 0 = n."
                }
            ]
        });
        let metadata_only = r#"
namespace GrokRxiv
def claimCount : Nat := 43
theorem claimCount_nonnegative : 0 <= claimCount := by
  simp [claimCount]
end GrokRxiv
"#;

        let issues = validate_lean_proof_code(metadata_only, &obligations);

        assert!(issues.iter().any(|issue| issue.contains("add_zero_claim")));
        assert!(issues.iter().any(|issue| issue.contains("metadata-only")));
    }
}
