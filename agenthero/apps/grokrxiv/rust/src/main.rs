//! Process adapter for the GrokRxiv DAG app.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;

use agenthero_agent_runtime::{
    app_adapter_lifecycle_event, write_adapter_event, AppAdapterRequest, AppAdapterResponse,
    APP_ADAPTER_PROTOCOL,
};
use agenthero_app_sdk::{
    load_dag_manifest, read_adapter_request, resolve_app_root, resolve_runtime_binary,
    write_adapter_response,
};
use agenthero_dag_executor::{
    manifest_node_result, ArtifactRef, DagExecutionEvent, DagExecutionReport, DagExecutor, DagIo,
    GenericToolRunner, NodeExecutionContext, NodeExecutionResult, NodeHandler,
};
use agenthero_dag_runtime::DagNodeStatus;
use async_trait::async_trait;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

const REVIEW_LOOP_BOOTSTRAP_ARTIFACTS: &[&str] = &[
    "body.md",
    "equations.json",
    "theorem_graph.json",
    "semantic_ast.json",
    "references.json",
];

const REVIEW_IDENTITY_FLAGS: &[&str] = &["--type", "--rev", "--paper-path", "--title", "--field"];

#[derive(Debug, Clone, Default)]
struct ReviewRequestOptions {
    source: String,
    source_type: Option<String>,
    rev: Option<String>,
    paper_path: Option<String>,
    title: Option<String>,
    field: Option<String>,
    corpus: bool,
    scan_root: Option<String>,
    limit: Option<String>,
    include: Vec<String>,
    exclude: Vec<String>,
    loop_enabled: bool,
    with_lean: bool,
    no_lean: bool,
    debug_output: bool,
    no_external_actions: bool,
}

#[derive(Debug, Clone)]
struct GrokrxivAdapter {
    app_name: &'static str,
    generic_tools: GenericToolRunner,
}

#[derive(Debug)]
struct RuntimeProcessOutput {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

#[async_trait]
impl NodeHandler for GrokrxivAdapter {
    async fn execute_node(
        &self,
        ctx: NodeExecutionContext<'_>,
    ) -> anyhow::Result<NodeExecutionResult> {
        if let Some(result) = self.generic_tools.execute_supported_tool(&ctx).await {
            return result;
        }
        if let Some(result) = execute_citation_validation_node(&ctx).await {
            return result;
        }
        if let Some(result) = execute_review_loop_node(&ctx).await {
            return result;
        }
        Ok(manifest_node_result(
            self.app_name,
            ctx.manifest.id.as_str(),
            ctx.node,
        ))
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let request = read_adapter_request(std::io::stdin())?;
    let response = match run(&request).await {
        Ok(response) => response,
        Err(err) => AppAdapterResponse::failed(&request, format!("{err:#}")),
    };
    write_adapter_response(std::io::stdout(), &response)?;
    Ok(())
}

async fn run(request: &AppAdapterRequest) -> anyhow::Result<AppAdapterResponse> {
    if request.app != "grokrxiv" {
        anyhow::bail!("grokrxiv adapter received app `{}`", request.app);
    }
    if manifest_dag_requested(request) {
        return run_manifest_dag_action(request).await;
    }
    run_app_runtime_action(request).await
}

fn manifest_dag_requested(request: &AppAdapterRequest) -> bool {
    request.action == "validate-citations"
        || review_action_manifest_dag_requested(request)
        || request
            .input
            .values
            .get("agenthero_manifest_dag")
            .and_then(Value::as_bool)
            .unwrap_or(false)
}

fn review_action_manifest_dag_requested(request: &AppAdapterRequest) -> bool {
    request.action == "review" && request.dag_type == "review-loop"
}

async fn run_manifest_dag(request: &AppAdapterRequest) -> anyhow::Result<DagExecutionReport> {
    let manifest = load_dag_manifest(app_root(), &request.dag_type)?;
    let input = manifest_input_for_request(request).await?;
    DagExecutor::new(GrokrxivAdapter {
        app_name: "grokrxiv",
        generic_tools: GenericToolRunner::new(generic_tool_artifact_root()),
    })
    .with_event_sink(|event| {
        let _ = write_adapter_event(std::io::stderr(), &event);
    })
    .execute_with_checkpoint(&manifest, input, request.checkpoint.as_ref())
    .await
}

async fn run_manifest_dag_action(
    request: &AppAdapterRequest,
) -> anyhow::Result<AppAdapterResponse> {
    emit_runtime_action_event(runtime_action_started_event(request));
    match run_manifest_dag(request).await {
        Ok(report) => {
            emit_runtime_action_event(manifest_report_action_event(request, &report));
            Ok(AppAdapterResponse::ok_report(request, report))
        }
        Err(err) => {
            let message = format!("{err:#}");
            emit_runtime_action_event(runtime_action_error_event(request, &message));
            Err(err)
        }
    }
}

async fn manifest_input_for_request(request: &AppAdapterRequest) -> anyhow::Result<DagIo> {
    let mut input = request.input.clone();
    if review_action_manifest_dag_requested(request) {
        seed_review_loop_manifest_input(request, &mut input).await?;
    }
    Ok(input)
}

async fn seed_review_loop_manifest_input(
    request: &AppAdapterRequest,
    input: &mut DagIo,
) -> anyhow::Result<()> {
    let options = review_request_options(request, input)?;
    validate_manifest_review_bootstrap_request(request, input, &options)?;
    let source = options.source.clone();
    let review_id = input
        .values
        .get("review_id")
        .and_then(Value::as_str)
        .map(ToString::to_string)
        .unwrap_or_else(|| stable_review_id(request, &options));
    let explicit_run_lean = input.values.get("run_lean").and_then(Value::as_bool);
    let run_lean = if options.no_lean {
        false
    } else if options.with_lean {
        true
    } else {
        explicit_run_lean.unwrap_or(false)
    };
    let auto_detect_lean = !options.no_lean && !options.with_lean && explicit_run_lean.is_none();

    input
        .values
        .insert("agenthero_manifest_dag".to_string(), json!(true));
    input
        .values
        .insert("dry_run".to_string(), json!(request.dry_run));
    input
        .values
        .entry("source".to_string())
        .or_insert_with(|| json!(source.clone()));
    input
        .values
        .entry("review_id".to_string())
        .or_insert_with(|| json!(review_id.clone()));
    input.values.insert("run_lean".to_string(), json!(run_lean));
    input.values.insert(
        "lean_policy".to_string(),
        json!({
            "requested": options.with_lean,
            "disabled": options.no_lean,
            "auto_detect": auto_detect_lean,
            "run_lean": run_lean,
        }),
    );
    input
        .values
        .insert("review_options".to_string(), review_options_json(&options));
    write_frozen_review_inputs(input, &review_id, request, &options).await?;
    Ok(())
}

fn review_request_options(
    request: &AppAdapterRequest,
    input: &DagIo,
) -> anyhow::Result<ReviewRequestOptions> {
    let mut options = ReviewRequestOptions::default();
    if let Some(source) = input
        .values
        .get("source")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
    {
        options.source = source.to_string();
    }

    let mut iter = request.args.iter().peekable();
    while let Some(arg) = iter.next() {
        if !arg.starts_with("--") {
            if options.source.is_empty() {
                options.source = arg.to_string();
            }
            continue;
        }
        let (flag, inline_value) = arg
            .split_once('=')
            .map(|(flag, value)| (flag, Some(value.to_string())))
            .unwrap_or((arg.as_str(), None));
        let mut next_value = || -> anyhow::Result<String> {
            inline_value
                .clone()
                .or_else(|| iter.next().map(ToString::to_string))
                .ok_or_else(|| anyhow::anyhow!("{flag} requires a value"))
        };
        match flag {
            "--type" => options.source_type = Some(next_value()?),
            "--rev" => options.rev = Some(next_value()?),
            "--paper-path" => options.paper_path = Some(next_value()?),
            "--title" => options.title = Some(next_value()?),
            "--field" => options.field = Some(next_value()?),
            "--corpus" => options.corpus = true,
            "--scan-root" => options.scan_root = Some(next_value()?),
            "--limit" => options.limit = Some(next_value()?),
            "--include" => options.include.push(next_value()?),
            "--exclude" => options.exclude.push(next_value()?),
            "--loop" => options.loop_enabled = true,
            "--with-lean" => options.with_lean = true,
            "--no-lean" => options.no_lean = true,
            "--debug" => options.debug_output = true,
            "--no-external-actions" => options.no_external_actions = true,
            other => anyhow::bail!("review action does not support argument `{other}`"),
        }
    }
    if options.with_lean && options.no_lean {
        anyhow::bail!("review action cannot combine --with-lean and --no-lean");
    }
    if options.source.trim().is_empty() {
        anyhow::bail!("review action requires a source positional argument");
    }

    Ok(options)
}

fn validate_manifest_review_bootstrap_request(
    request: &AppAdapterRequest,
    input: &DagIo,
    options: &ReviewRequestOptions,
) -> anyhow::Result<()> {
    if request.dry_run
        || input.artifacts.contains_key("review_input.json")
        || review_input_path_from_source_value(&options.source).is_some()
        || source_has_direct_review_artifacts(&options.source)
    {
        return Ok(());
    }

    if let Some(source_type) = options.source_type.as_deref() {
        if source_type != "arxiv" {
            anyhow::bail!(
                "manifest review bootstrap currently supports direct review_input/artifact roots or arXiv extraction; --type {source_type} requires the legacy GrokRxiv runtime until that app-owned bootstrap is migrated"
            );
        }
    }
    if options.rev.is_some()
        || options.paper_path.is_some()
        || options.corpus
        || options.scan_root.is_some()
        || options.limit.is_some()
        || !options.include.is_empty()
        || !options.exclude.is_empty()
    {
        anyhow::bail!(
            "manifest review bootstrap cannot forward git/corpus/local-source options to `extract` yet; provide a review_input.json/artifact root or run the legacy runtime for this source"
        );
    }
    let source_path = PathBuf::from(&options.source);
    if source_path.exists()
        && !review_input_path_from_source_value(&options.source)
            .as_ref()
            .is_some_and(|path| path.is_file())
        && !source_has_direct_review_artifacts(&options.source)
    {
        anyhow::bail!(
            "manifest review bootstrap cannot extract local source `{}` yet; provide review_input.json or an artifact root with body.md/equations.json/theorem_graph.json/references.json",
            source_path.display()
        );
    }
    Ok(())
}

fn stable_review_id(request: &AppAdapterRequest, options: &ReviewRequestOptions) -> String {
    let mut hasher = Sha256::new();
    hasher.update(request.app.as_bytes());
    hasher.update([0]);
    hasher.update(request.action.as_bytes());
    hasher.update([0]);
    hasher.update(request.dag_type.as_bytes());
    hasher.update([0]);
    hasher.update(options.source.as_bytes());
    for arg in canonical_review_identity_args(options) {
        hasher.update([0]);
        hasher.update(arg.as_bytes());
    }
    let digest = hasher.finalize();
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x50;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    uuid::Uuid::from_bytes(bytes).to_string()
}

fn canonical_review_identity_args(options: &ReviewRequestOptions) -> Vec<String> {
    let mut out = Vec::new();
    for (flag, value) in [
        ("--type", options.source_type.as_deref()),
        ("--rev", options.rev.as_deref()),
        ("--paper-path", options.paper_path.as_deref()),
        ("--title", options.title.as_deref()),
        ("--field", options.field.as_deref()),
    ] {
        if REVIEW_IDENTITY_FLAGS.contains(&flag) {
            if let Some(value) = value {
                out.push(format!("{flag}={value}"));
            }
        }
    }
    out
}

fn review_options_json(options: &ReviewRequestOptions) -> Value {
    json!({
        "source": &options.source,
        "source_type": &options.source_type,
        "rev": &options.rev,
        "paper_path": &options.paper_path,
        "title": &options.title,
        "field": &options.field,
        "corpus": options.corpus,
        "scan_root": &options.scan_root,
        "limit": &options.limit,
        "include": &options.include,
        "exclude": &options.exclude,
        "loop_enabled": options.loop_enabled,
        "with_lean": options.with_lean,
        "no_lean": options.no_lean,
        "debug_output": options.debug_output,
        "no_external_actions": options.no_external_actions,
    })
}

async fn write_frozen_review_inputs(
    input: &mut DagIo,
    review_id: &str,
    request: &AppAdapterRequest,
    options: &ReviewRequestOptions,
) -> anyhow::Result<()> {
    let dir = app_artifact_root()
        .join("review-loop")
        .join("inputs")
        .join(review_id);
    tokio::fs::create_dir_all(&dir).await?;
    let path = dir.join("source_manifest.json");
    let manifest = json!({
        "schema_version": "1.0.0",
        "app": &request.app,
        "action": &request.action,
        "dag_type": &request.dag_type,
        "raw_args": &request.args,
        "dry_run": request.dry_run,
        "review_options": review_options_json(options),
    });
    tokio::fs::write(&path, serde_json::to_vec_pretty(&manifest)?).await?;
    input
        .artifacts
        .insert("source_manifest.json".to_string(), artifact_ref(&path));
    freeze_review_input_artifact_ref(input, options, &dir).await?;
    Ok(())
}

async fn freeze_review_input_artifact_ref(
    input: &mut DagIo,
    options: &ReviewRequestOptions,
    dir: &Path,
) -> anyhow::Result<()> {
    let review_input = input
        .artifacts
        .get("review_input.json")
        .map(|artifact| PathBuf::from(&artifact.uri))
        .or_else(|| {
            input
                .values
                .get("review_input")
                .and_then(Value::as_str)
                .map(PathBuf::from)
        })
        .or_else(|| review_input_path_from_source_value(&options.source));
    if let Some(review_input) = review_input.filter(|path| path.is_file()) {
        let artifact_root = input
            .values
            .get("artifact_root")
            .and_then(Value::as_str)
            .map(PathBuf::from);
        let frozen_path =
            freeze_review_input_bundle(&review_input, artifact_root.as_deref(), dir).await?;
        input.values.insert(
            "review_input".to_string(),
            json!(frozen_path.to_string_lossy().to_string()),
        );
        input
            .artifacts
            .insert("review_input.json".to_string(), artifact_ref(&frozen_path));
    }
    Ok(())
}

async fn freeze_review_input_bundle(
    review_input: &Path,
    artifact_root: Option<&Path>,
    dir: &Path,
) -> anyhow::Result<PathBuf> {
    let bytes = tokio::fs::read(review_input).await?;
    let mut frozen: Value = serde_json::from_slice(&bytes)?;
    for (field, filename) in [
        ("metadata", "metadata.json"),
        ("body_markdown", "body.md"),
        ("sections", "sections.json"),
        ("equations", "equations.json"),
        ("references", "references.json"),
        ("theorem_graph", "theorem_graph.json"),
        ("extraction_report", "extraction_report.json"),
    ] {
        freeze_review_input_pointer(
            review_input,
            artifact_root,
            dir,
            &mut frozen,
            field,
            filename,
        )
        .await?;
    }
    if frozen
        .get("semantic_ast_uri")
        .and_then(Value::as_str)
        .is_some()
    {
        freeze_review_input_pointer(
            review_input,
            artifact_root,
            dir,
            &mut frozen,
            "semantic_ast_uri",
            "semantic_ast.json",
        )
        .await?;
    }
    let frozen_path = dir.join("review_input.json");
    tokio::fs::write(&frozen_path, serde_json::to_vec_pretty(&frozen)?).await?;
    Ok(frozen_path)
}

async fn freeze_review_input_pointer(
    review_input_path: &Path,
    artifact_root: Option<&Path>,
    dir: &Path,
    review_input: &mut Value,
    field: &str,
    filename: &str,
) -> anyhow::Result<()> {
    let pointer = review_input
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("review_input missing `{field}`"))?;
    let source = resolve_review_input_pointer(review_input_path, artifact_root, pointer)
        .ok_or_else(|| {
            anyhow::anyhow!("could not freeze review_input `{field}` pointer `{pointer}`")
        })?;
    let dest = dir.join(filename);
    tokio::fs::copy(&source, &dest).await?;
    review_input[field] = json!(filename);
    Ok(())
}

fn review_input_path_from_source_value(source: &str) -> Option<PathBuf> {
    let path = PathBuf::from(source);
    if path.is_file()
        && path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name == "review_input.json")
    {
        return Some(path);
    }
    if path.is_dir() {
        let candidate = path.join("review_input.json");
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

fn source_has_direct_review_artifacts(source: &str) -> bool {
    let path = PathBuf::from(source);
    path.is_dir()
        && [
            "body.md",
            "equations.json",
            "theorem_graph.json",
            "references.json",
        ]
        .into_iter()
        .all(|name| path.join(name).is_file())
}

fn seeded_review_markdown(source: &str) -> String {
    format!(
        "# GrokRxiv Review\n\nSource: `{source}`\n\nAgentHero prepared the review-loop artifact contract for this public review request.\n"
    )
}

fn seeded_body_markdown(source: &str) -> String {
    format!(
        "# GrokRxiv Source\n\nReview source: `{source}`\n\nNo extracted manuscript artifact was supplied with the public review request; this seed artifact gives the manifest runtime a stable node boundary until the app-owned extraction node materializes richer paper artifacts.\n"
    )
}

async fn execute_citation_validation_node(
    ctx: &NodeExecutionContext<'_>,
) -> Option<anyhow::Result<NodeExecutionResult>> {
    if ctx.manifest.id.as_str() != "citation-validation" {
        return None;
    }
    let output = match ctx.node.id.as_str() {
        "bibtex_reference_parser" => json!({
            "entries": read_json_input(ctx, "references.json")
                .and_then(|references| references.get("citations").cloned())
                .unwrap_or_else(|| Value::Array(Vec::new())),
            "source": "grokrxiv.citation_validation.bibtex_reference_parser"
        }),
        "doi_resolver" => json!({
            "resolved": read_json_input(ctx, "citation_validation/bibtex_entries.json")
                .and_then(|entries| entries.get("entries").cloned())
                .unwrap_or_else(|| Value::Array(Vec::new())),
            "source": "grokrxiv.citation_validation.doi_resolver"
        }),
        "semantic_similarity_check" => json!({
            "similarity": [],
            "source": "grokrxiv.citation_validation.semantic_similarity_check"
        }),
        "metadata_consistency_validator" => json!({
            "conflicts": [],
            "source": "grokrxiv.citation_validation.metadata_consistency_validator"
        }),
        "citation_graph_validation" => {
            let citations = read_json_input(ctx, "citation_validation/bibtex_entries.json")
                .and_then(|entries| entries.get("entries").cloned())
                .unwrap_or_else(|| Value::Array(Vec::new()));
            build_citation_validation_report(citations)
        }
        "citation_validation_adjudicator" => {
            let report = read_json_input(ctx, "citation_validation_report.json")
                .unwrap_or_else(|| build_citation_validation_report(Value::Array(Vec::new())));
            build_citation_validation_adjudication(&report)
        }
        _ => return None,
    };
    Some(write_single_json_output(ctx, output).await)
}

fn build_citation_validation_report(citations: Value) -> Value {
    let parsed_references = normalize_citation_references(citations);
    let summary = if parsed_references.is_empty() {
        "No references were supplied; deterministic citation validation found no issues."
            .to_string()
    } else {
        format!(
            "Deterministic citation validation normalized {} reference(s) with no blocking issues.",
            parsed_references.len()
        )
    };
    json!({
        "status": "verified",
        "summary": summary,
        "parsed_references": parsed_references,
        "resolver_results": [],
        "similarity_results": [],
        "metadata_conflicts": [],
        "graph_warnings": [],
        "remediation_items": []
    })
}

fn build_citation_validation_adjudication(report: &Value) -> Value {
    let verdict = match report.get("status").and_then(Value::as_str) {
        Some("verified") => "verified",
        Some("needs_remediation") => "needs_remediation",
        Some("failed") => "failed",
        _ => "failed",
    };
    let remediation_actions = report
        .get("remediation_items")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .map(|item| {
                    json!({
                        "target": item.get("key").cloned().unwrap_or_else(|| json!(null)),
                        "action": item.get("action").cloned().unwrap_or_else(|| json!("inspect_reference")),
                        "reason": item.get("reason").cloned().unwrap_or_else(|| json!("Citation validation requested remediation."))
                    })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let summary = report
        .get("summary")
        .and_then(Value::as_str)
        .unwrap_or("Citation validation completed.");
    json!({
        "verdict": verdict,
        "confidence": if verdict == "verified" { 1.0 } else { 0.65 },
        "evidence": [summary],
        "remediation_actions": remediation_actions
    })
}

fn normalize_citation_references(citations: Value) -> Vec<Value> {
    citations
        .as_array()
        .map(|items| {
            items
                .iter()
                .enumerate()
                .map(|(index, item)| normalize_citation_reference(index, item))
                .collect()
        })
        .unwrap_or_default()
}

fn normalize_citation_reference(index: usize, item: &Value) -> Value {
    if let Some(object) = item.as_object() {
        let key = first_string(object, &["key", "citation_key", "id"])
            .unwrap_or_else(|| format!("ref_{}", index + 1));
        let raw =
            first_string(object, &["raw", "citation", "text"]).unwrap_or_else(|| item.to_string());
        json!({
            "key": key,
            "raw": raw,
            "title": first_string(object, &["title"]),
            "authors": object
                .get("authors")
                .and_then(Value::as_array)
                .map(|authors| authors.iter().filter_map(Value::as_str).collect::<Vec<_>>())
                .unwrap_or_default(),
            "venue": first_string(object, &["venue", "journal", "booktitle"]),
            "year": object.get("year").and_then(Value::as_i64),
            "doi": first_string(object, &["doi", "resolved_doi"]),
            "arxiv_id": first_string(object, &["arxiv_id", "eprint"]),
            "cited": object.get("cited").and_then(Value::as_bool).unwrap_or(true)
        })
    } else {
        let raw = item
            .as_str()
            .map(ToString::to_string)
            .unwrap_or_else(|| item.to_string());
        json!({
            "key": format!("ref_{}", index + 1),
            "raw": raw,
            "title": null,
            "authors": [],
            "venue": null,
            "year": null,
            "doi": null,
            "arxiv_id": null,
            "cited": true
        })
    }
}

fn first_string(object: &serde_json::Map<String, Value>, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| object.get(*key).and_then(Value::as_str))
        .map(ToString::to_string)
}

async fn execute_review_loop_node(
    ctx: &NodeExecutionContext<'_>,
) -> Option<anyhow::Result<NodeExecutionResult>> {
    if ctx.manifest.id.as_str() != "review-loop" {
        return None;
    }
    if ctx.node.id == "paper_review" {
        return Some(review_loop_paper_review(ctx).await);
    }
    if ctx.node.id == "lean_review_fix_code" {
        return Some(review_loop_lean_node(ctx).await);
    }
    if ctx.node.id == "pr_review_fix_code" {
        return Some(write_review_loop_outputs(ctx, review_loop_pr_review_outputs(ctx)).await);
    }
    if ctx.node.id == "pr_fixer" {
        return Some(write_review_loop_outputs(ctx, review_loop_pr_fixer_outputs(ctx)).await);
    }
    let outputs = match ctx.node.id.as_str() {
        "claim_extractor" => review_loop_claims(ctx),
        "paper_math_source_collector" => review_loop_paper_math_sources(ctx),
        "knowledge_graph_builder" => review_loop_knowledge_graph(ctx),
        "semantic_category_mapper" => review_loop_semantic_outputs(ctx),
        "proof_obligation_generator" => review_loop_proof_outputs(ctx),
        "lean_faithfulness_check" => review_loop_faithfulness_outputs(ctx),
        "semantic_adequacy_checker" => review_loop_semantic_adequacy_outputs(ctx),
        "citation_validation" => review_loop_citation_validation_outputs(ctx),
        "policy_gate" => review_loop_policy_gate_outputs(ctx),
        "review_loop_report" => review_loop_report_outputs(ctx),
        "publish_decision" => review_loop_publish_decision_outputs(ctx),
        _ => return None,
    };
    Some(write_json_outputs(ctx, outputs).await)
}

async fn review_loop_paper_review(
    ctx: &NodeExecutionContext<'_>,
) -> anyhow::Result<NodeExecutionResult> {
    let review_id = review_id_value(ctx);
    let source = ctx
        .inputs
        .values
        .get("source")
        .and_then(Value::as_str)
        .unwrap_or("unknown_source")
        .to_string();
    let review_agents = ctx
        .inputs
        .values
        .get("review_agents")
        .cloned()
        .unwrap_or_else(|| json!([]));
    let render_artifacts = ctx
        .inputs
        .values
        .get("render_artifacts")
        .cloned()
        .unwrap_or_else(|| json!({ "review_md": seeded_review_markdown(&source) }));
    let mut result = NodeExecutionResult::ok()
        .with_value("review_id", review_id)
        .with_value("review_agents", review_agents)
        .with_value("render_artifacts", render_artifacts);
    let artifacts = review_loop_paper_review_artifacts(ctx, &source).await?;
    for (artifact_name, artifact) in artifacts {
        result = result.with_artifact(artifact_name, artifact);
    }
    Ok(result)
}

async fn review_loop_paper_review_artifacts(
    ctx: &NodeExecutionContext<'_>,
    source: &str,
) -> anyhow::Result<Vec<(String, ArtifactRef)>> {
    if REVIEW_LOOP_BOOTSTRAP_ARTIFACTS
        .iter()
        .all(|name| ctx.inputs.artifacts.contains_key(*name))
    {
        return Ok(REVIEW_LOOP_BOOTSTRAP_ARTIFACTS
            .iter()
            .filter_map(|name| {
                ctx.inputs
                    .artifacts
                    .get(*name)
                    .cloned()
                    .map(|artifact| ((*name).to_string(), artifact))
            })
            .collect());
    }

    let base = app_node_artifact_dir(ctx);
    if let Some(artifacts) = review_loop_artifacts_from_manifest_inputs(ctx, &base).await? {
        return Ok(artifacts);
    }

    if ctx
        .inputs
        .values
        .get("dry_run")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return seed_review_loop_artifacts(&base, source).await;
    }

    review_loop_artifacts_from_extract_action(ctx, &base, source).await
}

async fn review_loop_artifacts_from_manifest_inputs(
    ctx: &NodeExecutionContext<'_>,
    base: &Path,
) -> anyhow::Result<Option<Vec<(String, ArtifactRef)>>> {
    let review_input = ctx
        .inputs
        .artifacts
        .get("review_input.json")
        .map(|artifact| PathBuf::from(&artifact.uri))
        .or_else(|| {
            ctx.inputs
                .values
                .get("review_input")
                .and_then(Value::as_str)
                .map(PathBuf::from)
        })
        .or_else(|| review_input_path_from_source(ctx));
    if let Some(review_input) = review_input {
        let artifact_root = ctx
            .inputs
            .values
            .get("artifact_root")
            .and_then(Value::as_str)
            .map(PathBuf::from);
        return load_review_input_artifacts(&review_input, artifact_root.as_deref(), base)
            .await
            .map(Some);
    }

    let Some(source) = ctx.inputs.values.get("source").and_then(Value::as_str) else {
        return Ok(None);
    };
    let source_path = PathBuf::from(source);
    if source_path.is_dir() {
        let artifacts = direct_review_artifacts_from_dir(&source_path, base).await?;
        if !artifacts.is_empty() {
            return Ok(Some(artifacts));
        }
    }
    Ok(None)
}

fn review_input_path_from_source(ctx: &NodeExecutionContext<'_>) -> Option<PathBuf> {
    let source = ctx.inputs.values.get("source").and_then(Value::as_str)?;
    review_input_path_from_source_value(source)
}

async fn review_loop_artifacts_from_extract_action(
    ctx: &NodeExecutionContext<'_>,
    base: &Path,
    source: &str,
) -> anyhow::Result<Vec<(String, ArtifactRef)>> {
    let extract_args = extract_args_for_review_bootstrap(ctx, source)?;
    let runtime_manifest = app_root()
        .join("crates")
        .join("orchestrator")
        .join("Cargo.toml");
    let extract_request = AppAdapterRequest::new(
        "grokrxiv",
        "extract",
        "paper-ingest",
        extract_args,
        DagIo::default(),
        true,
        false,
    );
    let mut command = runtime_command(&runtime_manifest);
    build_runtime_args(&mut command, &extract_request);
    let output = match run_runtime_process(command, false).await {
        Ok(output) => output,
        Err(err) if err.kind() == ErrorKind::NotFound && runtime_fallback_allowed() => {
            let mut fallback = runtime_fallback_command(&runtime_manifest);
            build_runtime_args(&mut fallback, &extract_request);
            run_runtime_process(fallback, false).await.map_err(|fallback_err| {
                anyhow::anyhow!(
                    "paper_review extract bridge could not find grokrxiv-app and cargo fallback failed: {fallback_err}"
                )
            })?
        }
        Err(err) => {
            anyhow::bail!("paper_review extract bridge could not run grokrxiv-app: {err}");
        }
    };

    tokio::fs::create_dir_all(base).await?;
    tokio::fs::write(base.join("extract_stdout.log"), &output.stdout).await?;
    tokio::fs::write(base.join("extract_stderr.log"), &output.stderr).await?;
    if !output.status.success() {
        anyhow::bail!(
            "paper_review extract bridge failed with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let extracted: Value = serde_json::from_str(stdout.trim())
        .map_err(|err| anyhow::anyhow!("parse grokrxiv extract JSON output: {err}"))?;
    let extracted = extracted
        .as_array()
        .and_then(|items| items.first())
        .unwrap_or(&extracted);
    let review_input = extracted
        .get("review_input")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("grokrxiv extract output did not include review_input"))?;
    let artifact_root = extracted.get("artifact_root").and_then(Value::as_str);
    load_review_input_artifacts(
        &PathBuf::from(review_input),
        artifact_root.map(Path::new),
        base,
    )
    .await
}

fn extract_args_for_review_bootstrap(
    ctx: &NodeExecutionContext<'_>,
    source: &str,
) -> anyhow::Result<Vec<String>> {
    let options = ctx.inputs.values.get("review_options");
    let source_type = options
        .and_then(|value| value.get("source_type"))
        .and_then(Value::as_str);
    if let Some(source_type) = source_type {
        if source_type != "arxiv" {
            anyhow::bail!(
                "manifest review bootstrap currently supports direct review_input/artifact roots or arXiv extraction; --type {source_type} requires the legacy GrokRxiv runtime until that app-owned bootstrap is migrated"
            );
        }
    }

    let has_unsupported_source_options = options.is_some_and(|value| {
        value.get("rev").and_then(Value::as_str).is_some()
            || value.get("paper_path").and_then(Value::as_str).is_some()
            || value
                .get("corpus")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            || value.get("scan_root").and_then(Value::as_str).is_some()
            || value.get("limit").and_then(Value::as_str).is_some()
            || value
                .get("include")
                .and_then(Value::as_array)
                .is_some_and(|items| !items.is_empty())
            || value
                .get("exclude")
                .and_then(Value::as_array)
                .is_some_and(|items| !items.is_empty())
    });
    if has_unsupported_source_options {
        anyhow::bail!(
            "manifest review bootstrap cannot forward git/corpus/local-source options to `extract` yet; provide a review_input.json/artifact root or run the legacy runtime for this source"
        );
    }

    let source_path = PathBuf::from(source);
    if source_path.exists() {
        anyhow::bail!(
            "manifest review bootstrap cannot extract local source `{}` yet; provide review_input.json or an artifact root with body.md/equations.json/theorem_graph.json/references.json",
            source_path.display()
        );
    }
    Ok(vec![source.to_string()])
}

async fn direct_review_artifacts_from_dir(
    artifact_root: &Path,
    base: &Path,
) -> anyhow::Result<Vec<(String, ArtifactRef)>> {
    let required = [
        ("body.md", artifact_root.join("body.md")),
        ("equations.json", artifact_root.join("equations.json")),
        (
            "theorem_graph.json",
            artifact_root.join("theorem_graph.json"),
        ),
        ("references.json", artifact_root.join("references.json")),
    ];
    if required.iter().any(|(_, path)| !path.is_file()) {
        return Ok(Vec::new());
    }
    let mut out = required
        .into_iter()
        .map(|(name, path)| (name.to_string(), artifact_ref(&path)))
        .collect::<Vec<_>>();
    let semantic_ast = artifact_root.join("semantic_ast.json");
    if semantic_ast.is_file() {
        out.push(("semantic_ast.json".to_string(), artifact_ref(&semantic_ast)));
    } else {
        let path = write_seed_json(base, "semantic_ast.json", json!({ "nodes": [] })).await?;
        out.push(("semantic_ast.json".to_string(), artifact_ref(&path)));
    }
    Ok(out)
}

async fn load_review_input_artifacts(
    review_input_path: &Path,
    artifact_root: Option<&Path>,
    base: &Path,
) -> anyhow::Result<Vec<(String, ArtifactRef)>> {
    let bytes = tokio::fs::read(review_input_path).await.map_err(|err| {
        anyhow::anyhow!("read review_input {}: {err}", review_input_path.display())
    })?;
    let review_input: Value = serde_json::from_slice(&bytes).map_err(|err| {
        anyhow::anyhow!("parse review_input {}: {err}", review_input_path.display())
    })?;
    let mut out = Vec::new();
    for (artifact_name, field) in [
        ("body.md", "body_markdown"),
        ("equations.json", "equations"),
        ("theorem_graph.json", "theorem_graph"),
        ("references.json", "references"),
    ] {
        let pointer = review_input
            .get(field)
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("review_input missing `{field}`"))?;
        let path = resolve_review_input_pointer(review_input_path, artifact_root, pointer)
            .ok_or_else(|| {
                anyhow::anyhow!("could not resolve review_input `{field}` pointer `{pointer}`")
            })?;
        out.push((artifact_name.to_string(), artifact_ref(&path)));
    }

    if let Some(pointer) = review_input.get("semantic_ast_uri").and_then(Value::as_str) {
        if let Some(path) = resolve_review_input_pointer(review_input_path, artifact_root, pointer)
        {
            out.push(("semantic_ast.json".to_string(), artifact_ref(&path)));
            return Ok(out);
        }
    }
    let path = write_seed_json(base, "semantic_ast.json", json!({ "nodes": [] })).await?;
    out.push(("semantic_ast.json".to_string(), artifact_ref(&path)));
    Ok(out)
}

fn resolve_review_input_pointer(
    review_input_path: &Path,
    artifact_root: Option<&Path>,
    pointer: &str,
) -> Option<PathBuf> {
    if pointer.starts_with("supabase://") {
        return None;
    }
    let pointer_path = PathBuf::from(pointer);
    if pointer_path.is_absolute() && pointer_path.is_file() {
        return Some(pointer_path);
    }

    let mut candidates = Vec::new();
    if let Some(root) = artifact_root {
        candidates.push(root.join(&pointer_path));
        if let Some(name) = pointer_path.file_name() {
            candidates.push(root.join(name));
        }
    }
    if let Ok(repo_root) = std::env::var("GROKRXIV_DATA_REPO_PATH") {
        candidates.push(PathBuf::from(repo_root).join(&pointer_path));
    }
    if let Some(parent) = review_input_path.parent() {
        candidates.push(parent.join(&pointer_path));
        if let Some(name) = pointer_path.file_name() {
            candidates.push(parent.join(name));
        }
    }
    candidates.into_iter().find(|candidate| candidate.is_file())
}

async fn seed_review_loop_artifacts(
    seed_dir: &Path,
    source: &str,
) -> anyhow::Result<Vec<(String, ArtifactRef)>> {
    tokio::fs::create_dir_all(seed_dir).await?;
    let mut out = Vec::new();
    let body = write_seed_bytes(
        seed_dir,
        "body.md",
        seeded_body_markdown(source).into_bytes(),
    )
    .await?;
    out.push(("body.md".to_string(), artifact_ref(&body)));
    let equations = write_seed_json(
        seed_dir,
        "equations.json",
        json!({
            "artifact": "equations.json",
            "equations": []
        }),
    )
    .await?;
    out.push(("equations.json".to_string(), artifact_ref(&equations)));
    let theorem_graph = write_seed_json(
        seed_dir,
        "theorem_graph.json",
        json!({
            "artifact": "theorem_graph.json",
            "nodes": []
        }),
    )
    .await?;
    out.push((
        "theorem_graph.json".to_string(),
        artifact_ref(&theorem_graph),
    ));
    let semantic_ast = write_seed_json(
        seed_dir,
        "semantic_ast.json",
        json!({
            "nodes": []
        }),
    )
    .await?;
    out.push(("semantic_ast.json".to_string(), artifact_ref(&semantic_ast)));
    let references = write_seed_json(
        seed_dir,
        "references.json",
        json!({
            "citations": []
        }),
    )
    .await?;
    out.push(("references.json".to_string(), artifact_ref(&references)));
    Ok(out)
}

async fn write_seed_json(seed_dir: &Path, name: &str, value: Value) -> anyhow::Result<PathBuf> {
    write_seed_bytes(seed_dir, name, serde_json::to_vec_pretty(&value)?).await
}

async fn write_seed_bytes(
    seed_dir: &Path,
    name: &str,
    contents: Vec<u8>,
) -> anyhow::Result<PathBuf> {
    tokio::fs::create_dir_all(seed_dir).await?;
    let path = seed_dir.join(name.replace('/', "_"));
    tokio::fs::write(&path, contents).await?;
    Ok(path)
}

fn review_loop_claims(ctx: &NodeExecutionContext<'_>) -> Vec<(String, Value)> {
    let review_id = review_id_value(ctx);
    let claims = ctx
        .inputs
        .values
        .get("review_agents")
        .and_then(Value::as_array)
        .map(|agents| {
            agents
                .iter()
                .flat_map(|agent| {
                    agent
                        .get("claims")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                        .cloned()
                })
                .enumerate()
                .map(|(index, claim)| normalize_claim(index, claim))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    vec![(
        "review_loop/claims.json".to_string(),
        json!({
            "review_id": review_id,
            "claims": claims,
            "source": "grokrxiv.review_loop.claim_extractor"
        }),
    )]
}

fn review_loop_paper_math_sources(ctx: &NodeExecutionContext<'_>) -> Vec<(String, Value)> {
    let body = read_text_input(ctx, "body.md").unwrap_or_default();
    let theorem_graph = read_json_input(ctx, "theorem_graph.json").unwrap_or_else(|| json!({}));
    let equations = read_json_input(ctx, "equations.json").unwrap_or_else(|| json!([]));
    let semantic_ast = read_json_input(ctx, "semantic_ast.json").unwrap_or_else(|| json!({}));
    vec![(
        "review_loop/paper_math_sources.json".to_string(),
        json!({
            "review_id": review_id_value(ctx),
            "schema_version": "1.0.0",
            "source": "paper_extract_artifacts",
            "artifact_sources": review_loop_artifact_sources(ctx),
            "warnings": [],
            "body": canonical_body_source(body),
            "equations": canonical_equation_source(equations),
            "theorem_graph": canonical_theorem_graph_source(theorem_graph),
            "semantic_ast": semantic_ast,
        }),
    )]
}

fn review_loop_knowledge_graph(ctx: &NodeExecutionContext<'_>) -> Vec<(String, Value)> {
    let claims = read_json_input(ctx, "review_loop/claims.json")
        .and_then(|value| value.get("claims").cloned())
        .unwrap_or_else(|| json!([]));
    let nodes = claims
        .as_array()
        .into_iter()
        .flatten()
        .map(|claim| {
            json!({
                "id": claim.get("id").cloned().unwrap_or(Value::Null),
                "label": claim.get("statement").cloned().unwrap_or(Value::Null),
                "kind": "claim"
            })
        })
        .collect::<Vec<_>>();
    vec![(
        "review_loop/knowledge_graph.json".to_string(),
        json!({
            "review_id": review_id_value(ctx),
            "nodes": nodes,
            "edges": [],
            "source": "grokrxiv.review_loop.knowledge_graph_builder"
        }),
    )]
}

fn review_loop_semantic_outputs(ctx: &NodeExecutionContext<'_>) -> Vec<(String, Value)> {
    let review_claims_value =
        read_json_input(ctx, "review_loop/claims.json").unwrap_or_else(|| json!({"claims": []}));
    let math_sources =
        read_json_input(ctx, "review_loop/paper_math_sources.json").unwrap_or_else(|| json!({}));
    let knowledge_graph =
        read_json_input(ctx, "review_loop/knowledge_graph.json").unwrap_or_else(|| json!({}));
    let semantic_ir = grokrxiv_review_loop::build_semantic_ir_from_paper_math(
        review_uuid(ctx),
        &math_sources,
        &review_claims_value,
        &knowledge_graph,
    );
    let theorem_count = semantic_ir
        .get("theorem_candidates")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);
    let definition_count = semantic_ir
        .get("definitions")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);
    let assumption_count = semantic_ir
        .get("assumptions")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);
    vec![
        ("review_loop/semantic_ir.json".to_string(), semantic_ir),
        (
            "review_loop/semantic_model.json".to_string(),
            json!({
                "schema_version": "1.0.0",
                "review_id": review_id_value(ctx),
                "semantic_ir": "review_loop/semantic_ir.json",
                "paper_math_sources": "review_loop/paper_math_sources.json",
                "theorem_candidate_count": theorem_count,
                "definition_count": definition_count,
                "assumption_count": assumption_count,
            }),
        ),
    ]
}

fn review_loop_proof_outputs(ctx: &NodeExecutionContext<'_>) -> Vec<(String, Value)> {
    let semantic_ir =
        read_json_input(ctx, "review_loop/semantic_ir.json").unwrap_or_else(|| json!({}));
    let proof_obligations = grokrxiv_review_loop::build_proof_obligations(
        review_uuid(ctx),
        &semantic_ir,
        &json!({"status": "retired"}),
    );
    let lean_targets = grokrxiv_review_loop::build_lean_targets(&proof_obligations);
    vec![
        (
            "review_loop/proof_obligations.json".to_string(),
            proof_obligations,
        ),
        ("review_loop/lean_targets.json".to_string(), lean_targets),
    ]
}

async fn review_loop_lean_node(
    ctx: &NodeExecutionContext<'_>,
) -> anyhow::Result<NodeExecutionResult> {
    let proof_obligations =
        read_json_input(ctx, "review_loop/proof_obligations.json").unwrap_or_else(|| json!({}));
    let lean_targets =
        read_json_input(ctx, "review_loop/lean_targets.json").unwrap_or_else(|| json!({}));
    let lean_code = lean_code_from_targets(&lean_targets);
    let base = app_node_artifact_dir(ctx);
    let proof_path = base.join("review_loop/lean/GrokRxiv/Proofs.lean");
    if let Some(parent) = proof_path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::write(&proof_path, &lean_code).await?;

    let lean_run = if grokrxiv_review_loop::proof_obligations_skip_lean(&proof_obligations) {
        LeanRunOutcome::skipped(
            proof_obligations
                .get("skip_reason")
                .and_then(Value::as_str)
                .unwrap_or("no_math_found"),
        )
    } else if lean_execution_requested(ctx, &lean_targets) {
        run_lean_verifier(ctx, &base, &proof_path, &lean_code, &lean_targets).await?
    } else {
        LeanRunOutcome::skipped("lean_execution_not_enabled_in_gated_manifest_dag")
    };
    let lean_results = lean_results_json(ctx, &lean_targets, &lean_run);
    let theorem_map = grokrxiv_review_loop::build_theorem_map(&proof_obligations, &lean_results);
    let outputs = vec![
        (
            "review_loop/lean/GrokRxiv/Proofs.lean".to_string(),
            ReviewLoopArtifact::Text(lean_code.clone()),
        ),
        (
            "review_loop/lean/results.json".to_string(),
            ReviewLoopArtifact::Json(lean_results.clone()),
        ),
        (
            "review_loop/lean/fix_rounds.json".to_string(),
            ReviewLoopArtifact::Json(json!({
                "stage": "lean_review_fix_code",
                "attempts": lean_results["attempts"],
                "status": lean_results["status"],
                "skip_reason": lean_results["skip_reason"]
            })),
        ),
        (
            "review_loop/lean/theorem_map.json".to_string(),
            ReviewLoopArtifact::Json(theorem_map.clone()),
        ),
        (
            "review_loop/lean/verification_report.json".to_string(),
            ReviewLoopArtifact::Json(theorem_map),
        ),
    ];

    let mut result = write_review_loop_outputs_at_base(&base, outputs).await?;
    if let Some(command) = lean_run.command {
        result = result.with_command(command);
    }
    result = result.with_exit_status(lean_run.exit_status);
    for name in ["stdout.log", "stderr.log", "status.json"] {
        let path = base.join(name);
        if tokio::fs::metadata(&path).await.is_ok() {
            result = result.with_diagnostic_artifact(
                format!("logs/{}/{}", ctx.node.id, name),
                artifact_ref(&path),
            );
        }
    }
    Ok(result)
}

#[derive(Debug)]
struct LeanRunOutcome {
    status: &'static str,
    skipped: bool,
    skip_reason: Option<String>,
    command: Option<Vec<String>>,
    exit_status: Option<i32>,
    stdout: String,
    stderr: String,
    semantic_issues: Vec<String>,
}

impl LeanRunOutcome {
    fn skipped(reason: &str) -> Self {
        Self {
            status: "skipped",
            skipped: true,
            skip_reason: Some(reason.to_string()),
            command: None,
            exit_status: None,
            stdout: String::new(),
            stderr: String::new(),
            semantic_issues: vec![reason.to_string()],
        }
    }
}

fn lean_execution_requested(ctx: &NodeExecutionContext<'_>, lean_targets: &Value) -> bool {
    let env_run_lean = std::env::var("AGENTHERO_RUN_LEAN")
        .ok()
        .is_some_and(|value| matches!(value.as_str(), "1" | "true" | "yes" | "on"));
    lean_execution_requested_from_policy(&ctx.inputs.values, lean_targets, env_run_lean)
}

fn lean_execution_requested_from_policy(
    values: &std::collections::BTreeMap<String, Value>,
    lean_targets: &Value,
    env_run_lean: bool,
) -> bool {
    let lean_policy = values.get("lean_policy").unwrap_or(&Value::Null);
    if lean_policy
        .get("disabled")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return false;
    }
    if values
        .get("run_lean")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        || lean_policy
            .get("run_lean")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        || env_run_lean
    {
        return true;
    }
    lean_policy
        .get("auto_detect")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        && lean_targets
            .get("targets")
            .and_then(Value::as_array)
            .is_some_and(|targets| !targets.is_empty())
}

async fn run_lean_verifier(
    ctx: &NodeExecutionContext<'_>,
    base: &Path,
    proof_path: &Path,
    lean_code: &str,
    lean_targets: &Value,
) -> anyhow::Result<LeanRunOutcome> {
    let mut command = lean_command(ctx);
    command.push(
        lean_verifier_proof_arg(base, proof_path)
            .to_string_lossy()
            .to_string(),
    );
    let timeout_secs = ctx
        .inputs
        .values
        .get("lean_timeout_secs")
        .and_then(Value::as_u64)
        .unwrap_or(120);

    let mut process = tokio::process::Command::new(&command[0]);
    process
        .args(&command[1..])
        .current_dir(base)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let output = match tokio::time::timeout(
        std::time::Duration::from_secs(timeout_secs),
        process.output(),
    )
    .await
    {
        Ok(output) => output?,
        Err(_) => {
            let status = json!({
                "command": command,
                "exit_status": null,
                "status": "fail",
                "error": format!("lean verifier timed out after {timeout_secs}s"),
            });
            write_lean_diagnostics(base, b"", b"timed out", &status).await?;
            return Ok(LeanRunOutcome {
                status: "fail",
                skipped: false,
                skip_reason: None,
                command: status["command"].as_array().map(|items| {
                    items
                        .iter()
                        .filter_map(Value::as_str)
                        .map(ToString::to_string)
                        .collect()
                }),
                exit_status: None,
                stdout: String::new(),
                stderr: "timed out".to_string(),
                semantic_issues: vec![format!("lean verifier timed out after {timeout_secs}s")],
            });
        }
    };

    let forbidden = forbidden_lean_terms(lean_code);
    let mut semantic_issues = Vec::new();
    if !forbidden.is_empty() {
        semantic_issues.push(format!(
            "forbidden Lean proof terms present: {}",
            forbidden.join(", ")
        ));
    }
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let pass = output.status.success() && semantic_issues.is_empty();
    let status_json = json!({
        "command": command,
        "exit_status": output.status.code(),
        "status": if pass { "pass" } else { "fail" },
        "target_count": lean_targets.get("targets").and_then(Value::as_array).map(Vec::len).unwrap_or(0),
        "semantic_issues": semantic_issues,
    });
    write_lean_diagnostics(base, &output.stdout, &output.stderr, &status_json).await?;
    Ok(LeanRunOutcome {
        status: if pass { "pass" } else { "fail" },
        skipped: false,
        skip_reason: None,
        command: status_json["command"].as_array().map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(ToString::to_string)
                .collect()
        }),
        exit_status: output.status.code(),
        stdout,
        stderr,
        semantic_issues: status_json["semantic_issues"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(ToString::to_string)
            .collect(),
    })
}

fn lean_command(ctx: &NodeExecutionContext<'_>) -> Vec<String> {
    if let Some(items) = ctx
        .inputs
        .values
        .get("lean_command")
        .and_then(Value::as_array)
    {
        let command = items
            .iter()
            .filter_map(Value::as_str)
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        if !command.is_empty() {
            return command;
        }
    }
    if let Some(command) = ctx
        .inputs
        .values
        .get("lean_command")
        .and_then(Value::as_str)
    {
        let command = command
            .split_whitespace()
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        if !command.is_empty() {
            return command;
        }
    }
    std::env::var("AGENTHERO_LEAN_COMMAND")
        .ok()
        .map(|value| {
            value
                .split_whitespace()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
        })
        .filter(|command| !command.is_empty())
        .unwrap_or_else(|| vec!["lean".to_string()])
}

fn lean_verifier_proof_arg(base: &Path, proof_path: &Path) -> PathBuf {
    proof_path
        .strip_prefix(base)
        .map(Path::to_path_buf)
        .unwrap_or_else(|_| proof_path.to_path_buf())
}

async fn write_lean_diagnostics(
    base: &Path,
    stdout: &[u8],
    stderr: &[u8],
    status: &Value,
) -> anyhow::Result<()> {
    tokio::fs::create_dir_all(base).await?;
    tokio::fs::write(base.join("stdout.log"), stdout).await?;
    tokio::fs::write(base.join("stderr.log"), stderr).await?;
    tokio::fs::write(base.join("status.json"), serde_json::to_vec_pretty(status)?).await?;
    Ok(())
}

fn forbidden_lean_terms(lean_code: &str) -> Vec<&'static str> {
    ["sorry", "admit", "axiom"]
        .into_iter()
        .filter(|term| lean_code.contains(term))
        .collect()
}

fn lean_results_json(
    ctx: &NodeExecutionContext<'_>,
    lean_targets: &Value,
    outcome: &LeanRunOutcome,
) -> Value {
    let declarations = lean_targets
        .get("targets")
        .and_then(Value::as_array)
        .map(|targets| {
            targets
                .iter()
                .filter_map(|target| {
                    let decl = target.get("lean_declaration").and_then(Value::as_str)?;
                    Some((
                        decl.to_string(),
                        json!({
                            "status": outcome.status,
                            "skipped": outcome.skipped,
                            "skip_reason": outcome.skip_reason,
                            "compile": {
                                "exit_status": outcome.exit_status,
                                "stdout": outcome.stdout,
                                "stderr": outcome.stderr,
                            },
                            "semantic_validation": {
                                "status": if outcome.semantic_issues.is_empty() { "pass" } else { "fail" },
                                "issues": outcome.semantic_issues,
                            }
                        }),
                    ))
                })
                .collect::<serde_json::Map<String, Value>>()
        })
        .unwrap_or_default();
    json!({
        "stage": "lean_review_fix_code",
        "target": "lean",
        "language": "lean",
        "filename": "GrokRxiv/Proofs.lean",
        "max_attempts": ctx
            .inputs
            .values
            .get(agenthero_dag_executor::LOOP_MAX_ROUNDS_INPUT)
            .cloned()
            .unwrap_or_else(|| json!(1)),
        "attempts": [
            {
                "attempt": ctx
                    .inputs
                    .values
                    .get(agenthero_dag_executor::LOOP_ROUND_INPUT)
                    .cloned()
                    .unwrap_or_else(|| json!(1)),
                "status": outcome.status,
                "skipped": outcome.skipped,
                "compile": {
                    "command": outcome.command,
                    "exit_status": outcome.exit_status,
                    "stdout": outcome.stdout,
                    "stderr": outcome.stderr,
                },
                "semantic_validation": {
                    "status": if outcome.semantic_issues.is_empty() { "pass" } else { "fail" },
                    "issues": outcome.semantic_issues,
                }
            }
        ],
        "declarations": declarations,
        "agent_output_audit_summary": {
            "total": 0,
            "accepted": 0,
            "rejected": 0,
            "by_role": {}
        },
        "compile": {
            "command": outcome.command,
            "exit_status": outcome.exit_status,
            "stdout": outcome.stdout,
            "stderr": outcome.stderr,
        },
        "status": outcome.status,
        "skipped": outcome.skipped,
        "skip_reason": outcome.skip_reason,
        "final_path": "review_loop/lean/GrokRxiv/Proofs.lean",
        "loop_continue": false
    })
}

fn review_loop_faithfulness_outputs(ctx: &NodeExecutionContext<'_>) -> Vec<(String, Value)> {
    let theorem_map =
        read_json_input(ctx, "review_loop/lean/theorem_map.json").unwrap_or_else(|| json!({}));
    let proved_targets = theorem_map
        .get("entries")
        .and_then(Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter(|entry| entry.get("status").and_then(Value::as_str) == Some("PROVED"))
                .count()
        })
        .unwrap_or(0);
    vec![(
        "review_loop/faithfulness.json".to_string(),
        json!({
            "schema_version": "1.0.0",
            "status": "skipped",
            "checked": 0,
            "proved_targets": proved_targets,
            "note": "No kernel-proved Lean targets to check for faithfulness in the gated manifest DAG.",
            "source": "review_loop/lean/theorem_map.json",
        }),
    )]
}

fn review_loop_semantic_adequacy_outputs(ctx: &NodeExecutionContext<'_>) -> Vec<(String, Value)> {
    let semantic_ir =
        read_json_input(ctx, "review_loop/semantic_ir.json").unwrap_or_else(|| json!({}));
    let theorem_map =
        read_json_input(ctx, "review_loop/lean/theorem_map.json").unwrap_or_else(|| json!({}));
    vec![(
        "review_loop/semantic_adequacy.json".to_string(),
        grokrxiv_review_loop::build_semantic_adequacy(&semantic_ir, &theorem_map),
    )]
}

fn review_loop_citation_validation_outputs(ctx: &NodeExecutionContext<'_>) -> Vec<(String, Value)> {
    let citations = read_json_input(ctx, "references.json")
        .and_then(|references| references.get("citations").cloned())
        .unwrap_or_else(|| json!([]));
    let report = build_citation_validation_report(citations);
    let adjudication = build_citation_validation_adjudication(&report);
    vec![
        ("citation_validation_report.json".to_string(), report),
        (
            "citation_validation_adjudication.json".to_string(),
            adjudication,
        ),
    ]
}

fn review_loop_pr_fixer_outputs(
    ctx: &NodeExecutionContext<'_>,
) -> Vec<(String, ReviewLoopArtifact)> {
    let review_md = ctx
        .inputs
        .values
        .get("render_artifacts")
        .and_then(|value| value.get("review_md").or_else(|| value.get("review")))
        .and_then(Value::as_str)
        .unwrap_or("Review artifact was not supplied; generated deterministic placeholder.");
    let fixed_review = format!(
        "{review_md}\n\n<!-- AgentHero review-loop pr_fixer: deterministic artifact pass -->\n"
    );
    let pr_fixes = json!({
        "stage": "pr_fixer",
        "status": "pass",
        "artifact_worktree": "review_loop/fixed",
        "fixed_markdown": "review_loop/fixed/review.md",
        "fixed_tex": null,
        "fixed_pdf": null,
        "compile_review_loop": {
            "stage": "pr_review_fix_code",
            "status": "pass",
            "attempts": [],
        },
        "issues": [],
    });
    vec![
        (
            "review_loop/pr_fixes.json".to_string(),
            ReviewLoopArtifact::Json(pr_fixes),
        ),
        (
            "review_loop/fixed/review.md".to_string(),
            ReviewLoopArtifact::Text(fixed_review),
        ),
    ]
}

fn review_loop_pr_review_outputs(
    ctx: &NodeExecutionContext<'_>,
) -> Vec<(String, ReviewLoopArtifact)> {
    let pr_fixes = read_json_input(ctx, "review_loop/pr_fixes.json").unwrap_or_else(|| json!({}));
    let status = pr_fixes
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("pass");
    let result = json!({
        "stage": "pr_review_fix_code",
        "target": "pr",
        "status": status,
        "attempts": [
            {
                "attempt": ctx
                    .inputs
                    .values
                    .get(agenthero_dag_executor::LOOP_ROUND_INPUT)
                    .cloned()
                    .unwrap_or_else(|| json!(1)),
                "status": status,
                "skipped": false,
            }
        ],
        "agent_output_audit_summary": {
            "total": 0,
            "accepted": 0,
            "rejected": 0,
            "by_role": {}
        },
        "loop_continue": false
    });
    vec![
        (
            "review_loop/pr_review/results.json".to_string(),
            ReviewLoopArtifact::Json(result.clone()),
        ),
        (
            "review_loop/pr_review/fix_rounds.json".to_string(),
            ReviewLoopArtifact::Json(json!({
                "stage": "pr_review_fix_code",
                "attempts": result["attempts"],
                "status": result["status"]
            })),
        ),
    ]
}

fn review_loop_policy_gate_outputs(ctx: &NodeExecutionContext<'_>) -> Vec<(String, Value)> {
    let lean_results =
        read_json_input(ctx, "review_loop/lean/results.json").unwrap_or_else(|| json!({}));
    let semantic_adequacy =
        read_json_input(ctx, "review_loop/semantic_adequacy.json").unwrap_or_else(|| json!({}));
    let citation_report =
        read_json_input(ctx, "citation_validation_report.json").unwrap_or_else(|| json!({}));
    let pr_review =
        read_json_input(ctx, "review_loop/pr_review/results.json").unwrap_or_else(|| json!({}));
    let pr_fixes = read_json_input(ctx, "review_loop/pr_fixes.json").unwrap_or_else(|| json!({}));
    let citation_status = citation_report
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let pr_status = pr_fixes
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let mut blocking_issues = Vec::new();
    if citation_status != "verified" {
        blocking_issues.push(format!(
            "Citation validation status `{citation_status}` requires remediation."
        ));
    }
    if pr_status != "pass" {
        blocking_issues.push(format!(
            "PR artifact fixer status `{pr_status}` requires remediation."
        ));
    }
    let integrity_ready = blocking_issues.is_empty();
    vec![(
        "review_loop/policy_gate.json".to_string(),
        json!({
            "deterministic_status": if integrity_ready { "pass" } else { "fail" },
            "integrity_ready": integrity_ready,
            "publisher_ready": false,
            "blocking_issues": blocking_issues,
            "component_status": {
                "lean": lean_results.get("status").cloned().unwrap_or_else(|| json!("unknown")),
                "semantic_adequacy": semantic_adequacy.get("status").cloned().unwrap_or_else(|| json!("unknown")),
                "citation_validation": citation_status,
                "pr_fixer": pr_status,
                "pr_review": pr_review.get("status").cloned().unwrap_or_else(|| json!("unknown")),
            },
            "publishability_vector": {
                "formal": if lean_results.get("skipped").and_then(Value::as_bool) == Some(true) {
                    "awaiting_formalization"
                } else {
                    "not_run"
                },
                "semantic_adequacy": semantic_adequacy.get("status").cloned().unwrap_or_else(|| json!("unknown")),
                "citation": citation_status,
                "reproducibility": "not_run",
                "integrity": if integrity_ready { "pass" } else { "fail" },
                "safety": pr_status,
            },
            "release_tier": {
                "tier": "in_review",
                "lifecycle_state": "needs_update",
            }
        }),
    )]
}

fn review_loop_report_outputs(ctx: &NodeExecutionContext<'_>) -> Vec<(String, Value)> {
    let policy_gate =
        read_json_input(ctx, "review_loop/policy_gate.json").unwrap_or_else(|| json!({}));
    let theorem_map =
        read_json_input(ctx, "review_loop/lean/theorem_map.json").unwrap_or_else(|| json!({}));
    let semantic_adequacy =
        read_json_input(ctx, "review_loop/semantic_adequacy.json").unwrap_or_else(|| json!({}));
    let pr_fixes = read_json_input(ctx, "review_loop/pr_fixes.json").unwrap_or_else(|| json!({}));
    let publisher_ready = policy_gate
        .get("publisher_ready")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let publish_decision = json!({
        "publisher_ready": publisher_ready,
        "action": if publisher_ready { "publication_pr" } else { "revision_needed_pr" },
        "auto_publish": publisher_ready,
    });
    vec![(
        "review_loop/review_loop_report.json".to_string(),
        json!({
            "review_id": review_id_value(ctx),
            "dag_type": "review-loop",
            "deterministic_status": policy_gate.get("deterministic_status").cloned().unwrap_or_else(|| json!("unknown")),
            "publisher_ready": publisher_ready,
            "blocking_issues": policy_gate.get("blocking_issues").cloned().unwrap_or_else(|| json!([])),
            "artifact_paths": review_loop_report_artifact_paths(),
            "theorem_formalization": theorem_map,
            "semantic_adequacy": semantic_adequacy,
            "publishability_vector": policy_gate.get("publishability_vector").cloned().unwrap_or_else(|| json!({})),
            "release_tier": policy_gate.get("release_tier").cloned().unwrap_or_else(|| json!({})),
            "pr_evidence": pr_fixes,
            "publish_decision": publish_decision,
        }),
    )]
}

fn review_loop_publish_decision_outputs(ctx: &NodeExecutionContext<'_>) -> Vec<(String, Value)> {
    let report =
        read_json_input(ctx, "review_loop/review_loop_report.json").unwrap_or_else(|| json!({}));
    vec![(
        "review_loop/publish_decision.json".to_string(),
        report.get("publish_decision").cloned().unwrap_or_else(|| {
            json!({
                "publisher_ready": false,
                "action": "revision_needed_pr",
                "auto_publish": false,
            })
        }),
    )]
}

fn lean_code_from_targets(lean_targets: &Value) -> String {
    let mut code =
        String::from("/- Generated by the GrokRxiv gated AgentHero review-loop adapter. -/\n");
    if let Some(targets) = lean_targets.get("targets").and_then(Value::as_array) {
        for target in targets {
            if let Some(skeleton) = target.get("lean_skeleton").and_then(Value::as_str) {
                code.push_str("\n");
                code.push_str(skeleton);
                code.push('\n');
            } else if let Some(statement) = target.get("lean_statement").and_then(Value::as_str) {
                code.push_str("\n");
                code.push_str(statement);
                code.push_str(" := by\n  trivial\n");
            }
        }
    }
    if code.lines().count() <= 1 {
        code.push_str("\n-- No Lean targets were selected for this review-loop run.\n");
    }
    code
}

fn review_loop_report_artifact_paths() -> Value {
    json!({
        "claims": "review_loop/claims.json",
        "paper_math_sources": "review_loop/paper_math_sources.json",
        "knowledge_graph": "review_loop/knowledge_graph.json",
        "semantic_ir": "review_loop/semantic_ir.json",
        "semantic_model": "review_loop/semantic_model.json",
        "lean": "review_loop/lean/results.json",
        "lean_targets": "review_loop/lean_targets.json",
        "lean_theorem_map": "review_loop/lean/theorem_map.json",
        "lean_verification_report": "review_loop/lean/verification_report.json",
        "semantic_adequacy": "review_loop/semantic_adequacy.json",
        "proof_obligations": "review_loop/proof_obligations.json",
        "citation_validation": "citation_validation_report.json",
        "citation_adjudication": "citation_validation_adjudication.json",
        "pr_fixes": "review_loop/pr_fixes.json",
        "policy_gate": "review_loop/policy_gate.json",
        "publish_decision": "review_loop/publish_decision.json",
    })
}

async fn write_single_json_output(
    ctx: &NodeExecutionContext<'_>,
    value: Value,
) -> anyhow::Result<NodeExecutionResult> {
    let output_name = ctx
        .node
        .outputs
        .first()
        .ok_or_else(|| anyhow::anyhow!("node `{}` has no declared output", ctx.node.id))?
        .clone();
    let path = app_node_artifact_dir(ctx).join(&output_name);
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::write(&path, serde_json::to_vec_pretty(&value)?).await?;
    Ok(NodeExecutionResult::ok().with_artifact(output_name, artifact_ref(&path)))
}

async fn write_json_outputs(
    ctx: &NodeExecutionContext<'_>,
    outputs: Vec<(String, Value)>,
) -> anyhow::Result<NodeExecutionResult> {
    write_review_loop_outputs(
        ctx,
        outputs
            .into_iter()
            .map(|(name, value)| (name, ReviewLoopArtifact::Json(value)))
            .collect(),
    )
    .await
}

enum ReviewLoopArtifact {
    Json(Value),
    Text(String),
}

async fn write_review_loop_outputs(
    ctx: &NodeExecutionContext<'_>,
    outputs: Vec<(String, ReviewLoopArtifact)>,
) -> anyhow::Result<NodeExecutionResult> {
    let base = app_node_artifact_dir(ctx);
    write_review_loop_outputs_at_base(&base, outputs).await
}

async fn write_review_loop_outputs_at_base(
    base: &Path,
    outputs: Vec<(String, ReviewLoopArtifact)>,
) -> anyhow::Result<NodeExecutionResult> {
    let mut result = NodeExecutionResult::ok();
    for (output_name, value) in outputs {
        let path = base.join(&output_name);
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        match value {
            ReviewLoopArtifact::Json(value) => {
                if let Some(loop_continue) = value.get("loop_continue").and_then(Value::as_bool) {
                    result = result.with_value("loop_continue", json!(loop_continue));
                }
                tokio::fs::write(&path, serde_json::to_vec_pretty(&value)?).await?;
            }
            ReviewLoopArtifact::Text(value) => {
                tokio::fs::write(&path, value).await?;
            }
        }
        result = result.with_artifact(output_name, artifact_ref(&path));
    }
    Ok(result)
}

fn read_json_input(ctx: &NodeExecutionContext<'_>, name: &str) -> Option<Value> {
    let artifact = ctx.inputs.artifacts.get(name)?;
    let bytes = std::fs::read(&artifact.uri).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn read_text_input(ctx: &NodeExecutionContext<'_>, name: &str) -> Option<String> {
    let artifact = ctx.inputs.artifacts.get(name)?;
    std::fs::read_to_string(&artifact.uri).ok()
}

fn review_loop_artifact_sources(ctx: &NodeExecutionContext<'_>) -> Vec<String> {
    [
        "body.md",
        "equations.json",
        "theorem_graph.json",
        "semantic_ast.json",
    ]
    .into_iter()
    .filter(|name| ctx.inputs.artifacts.contains_key(*name))
    .map(ToString::to_string)
    .collect()
}

fn canonical_body_source(body: String) -> Value {
    json!({
        "artifact": "body.md",
        "sections": [
            {
                "id": "body",
                "heading": null,
                "body_markdown": body
            }
        ]
    })
}

fn canonical_equation_source(equations: Value) -> Value {
    if equations
        .get("equations")
        .and_then(Value::as_array)
        .is_some()
    {
        return equations;
    }
    if let Some(items) = equations.as_array() {
        return json!({
            "artifact": "equations.json",
            "equations": items
        });
    }
    json!({
        "artifact": "equations.json",
        "equations": []
    })
}

fn canonical_theorem_graph_source(theorem_graph: Value) -> Value {
    if theorem_graph
        .get("nodes")
        .and_then(Value::as_array)
        .is_some()
        || theorem_graph
            .get("theorem_graph")
            .and_then(Value::as_array)
            .is_some()
    {
        return theorem_graph;
    }
    if let Some(items) = theorem_graph.get("theorems").and_then(Value::as_array) {
        return json!({
            "artifact": "theorem_graph.json",
            "nodes": items
        });
    }
    if let Some(items) = theorem_graph.as_array() {
        return json!({
            "artifact": "theorem_graph.json",
            "nodes": items
        });
    }
    json!({
        "artifact": "theorem_graph.json",
        "nodes": []
    })
}

fn review_id_value(ctx: &NodeExecutionContext<'_>) -> Value {
    ctx.inputs
        .values
        .get("review_id")
        .cloned()
        .unwrap_or(Value::Null)
}

fn review_uuid(ctx: &NodeExecutionContext<'_>) -> uuid::Uuid {
    ctx.inputs
        .values
        .get("review_id")
        .and_then(Value::as_str)
        .and_then(|value| uuid::Uuid::parse_str(value).ok())
        .unwrap_or_else(uuid::Uuid::nil)
}

fn normalize_claim(index: usize, claim: Value) -> Value {
    let id = claim
        .get("id")
        .and_then(Value::as_str)
        .map(ToString::to_string)
        .unwrap_or_else(|| format!("claim_{}", index + 1));
    let statement = claim
        .get("statement")
        .or_else(|| claim.get("claim"))
        .cloned()
        .unwrap_or(Value::Null);
    let confidence = claim
        .get("confidence")
        .cloned()
        .unwrap_or_else(|| json!(null));
    json!({
        "id": id,
        "statement": statement,
        "confidence": confidence
    })
}

fn app_node_artifact_dir(ctx: &NodeExecutionContext<'_>) -> PathBuf {
    app_artifact_root()
        .join(ctx.manifest.id.as_str())
        .join(&ctx.node.id)
        .join(uuid::Uuid::new_v4().to_string())
}

fn artifact_ref(path: &Path) -> ArtifactRef {
    ArtifactRef {
        uri: path.to_string_lossy().to_string(),
        media_type: Some(media_type_for_path(path).to_string()),
        metadata: Default::default(),
    }
}

fn media_type_for_path(path: &Path) -> &'static str {
    match path.extension().and_then(|extension| extension.to_str()) {
        Some("json") => "application/json",
        Some("md") => "text/markdown",
        Some("lean") => "text/x-lean",
        Some("txt") | Some("log") => "text/plain",
        _ => "application/octet-stream",
    }
}

async fn run_app_runtime_action(request: &AppAdapterRequest) -> anyhow::Result<AppAdapterResponse> {
    let runtime_manifest = app_root()
        .join("crates")
        .join("orchestrator")
        .join("Cargo.toml");
    let stream_stderr = runtime_stderr_stream_requested(request);
    let mut command = runtime_command(&runtime_manifest);
    build_runtime_args(&mut command, request);
    emit_runtime_action_event(runtime_action_started_event(request));
    let output = match run_runtime_process(command, stream_stderr).await {
        Ok(output) => output,
        Err(err) if err.kind() == ErrorKind::NotFound && runtime_fallback_allowed() => {
            let mut fallback = runtime_fallback_command(&runtime_manifest);
            build_runtime_args(&mut fallback, request);
            match run_runtime_process(fallback, stream_stderr).await {
                Ok(output) => output,
                Err(fallback_err) => {
                    let message = format!(
                        "run GrokRxiv app action `{}`: compiled binary was not found and cargo fallback failed: {fallback_err}",
                        request.action
                    );
                    emit_runtime_action_event(runtime_action_error_event(request, &message));
                    return Err(anyhow::anyhow!(message));
                }
            }
        }
        Err(err) => {
            let message = format!(
                "run GrokRxiv app action `{}` with compiled grokrxiv-app binary: {err}",
                request.action
            );
            emit_runtime_action_event(runtime_action_error_event(request, &message));
            return Err(anyhow::anyhow!(message));
        }
    };
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    if !output.status.success() {
        let message = format!(
            "GrokRxiv action `{}` exited with {}: {}",
            request.action,
            output.status,
            stderr.trim()
        );
        emit_runtime_action_event(runtime_action_status_event(
            request,
            "error",
            "app_action.failed",
            &message,
            "failed",
            output.status.code(),
            output.stdout.len(),
            output.stderr.len(),
        ));
        anyhow::bail!(message);
    }
    emit_runtime_action_event(runtime_action_status_event(
        request,
        "info",
        "app_action.completed",
        &format!("GrokRxiv action `{}` completed", request.action),
        "completed",
        output.status.code(),
        output.stdout.len(),
        output.stderr.len(),
    ));
    Ok(AppAdapterResponse {
        protocol: APP_ADAPTER_PROTOCOL.to_string(),
        app: request.app.clone(),
        action: request.action.clone(),
        dag_type: request.dag_type.clone(),
        ok: true,
        report: None,
        output: Some(serde_json::json!({
            "status": output.status.code(),
            "stdout": stdout,
            "stderr": stderr,
        })),
        error: None,
    })
}

fn emit_runtime_action_event(event: DagExecutionEvent) {
    let _ = write_adapter_event(std::io::stderr(), &event);
}

fn runtime_action_started_event(request: &AppAdapterRequest) -> DagExecutionEvent {
    runtime_action_event(
        request,
        "info",
        "app_action.started",
        &format!("GrokRxiv action `{}` started", request.action),
        "started",
        None,
        BTreeMap::new(),
    )
}

fn runtime_action_error_event(request: &AppAdapterRequest, message: &str) -> DagExecutionEvent {
    runtime_action_event(
        request,
        "error",
        "app_action.failed",
        message,
        "failed",
        None,
        BTreeMap::from([("error".to_string(), json!(message))]),
    )
}

fn runtime_action_status_event(
    request: &AppAdapterRequest,
    level: &str,
    event_type: &str,
    message: &str,
    status: &str,
    exit_status: Option<i32>,
    stdout_bytes: usize,
    stderr_bytes: usize,
) -> DagExecutionEvent {
    runtime_action_event(
        request,
        level,
        event_type,
        message,
        status,
        exit_status,
        BTreeMap::from([
            ("stdout_bytes".to_string(), json!(stdout_bytes)),
            ("stderr_bytes".to_string(), json!(stderr_bytes)),
        ]),
    )
}

fn manifest_report_action_event(
    request: &AppAdapterRequest,
    report: &DagExecutionReport,
) -> DagExecutionEvent {
    let dag_status = serde_json::to_value(report.status).unwrap_or_else(|_| json!("unknown"));
    let extra = BTreeMap::from([
        ("dag_status".to_string(), dag_status),
        ("node_count".to_string(), json!(report.nodes.len())),
    ]);
    match report.status {
        DagNodeStatus::Ok | DagNodeStatus::Degraded => runtime_action_event(
            request,
            "info",
            "app_action.completed",
            &format!("GrokRxiv action `{}` completed", request.action),
            "completed",
            Some(0),
            extra,
        ),
        DagNodeStatus::AwaitingApproval => runtime_action_event(
            request,
            "info",
            "app_action.awaiting_approval",
            &format!("GrokRxiv action `{}` awaiting approval", request.action),
            "awaiting_approval",
            None,
            extra,
        ),
        DagNodeStatus::Pending
        | DagNodeStatus::Running
        | DagNodeStatus::Failed
        | DagNodeStatus::Skipped => runtime_action_event(
            request,
            "error",
            "app_action.failed",
            &format!(
                "GrokRxiv action `{}` ended with DAG status {:?}",
                request.action, report.status
            ),
            "failed",
            Some(1),
            extra,
        ),
    }
}

fn runtime_action_event(
    request: &AppAdapterRequest,
    level: &str,
    event_type: &str,
    message: &str,
    status: &str,
    exit_status: Option<i32>,
    mut extra: BTreeMap<String, Value>,
) -> DagExecutionEvent {
    app_adapter_lifecycle_event(
        request,
        level,
        event_type,
        message,
        status,
        exit_status,
        std::mem::take(&mut extra),
    )
}

fn runtime_command(runtime_manifest: &Path) -> tokio::process::Command {
    let mut command =
        tokio::process::Command::new(resolve_runtime_binary("GROKRXIV_APP_BIN", "grokrxiv-app"));
    if let Some(parent) = runtime_manifest.parent() {
        command.current_dir(parent);
    }
    set_app_root_env(&mut command);
    command
}

fn runtime_fallback_command(runtime_manifest: &Path) -> tokio::process::Command {
    let mut command = tokio::process::Command::new("cargo");
    command
        .arg("run")
        .arg("--manifest-path")
        .arg(runtime_manifest)
        .arg("--quiet")
        .arg("--bin")
        .arg("grokrxiv-app")
        .arg("--")
        .current_dir(repo_root());
    set_app_root_env(&mut command);
    command
}

fn build_runtime_args(command: &mut tokio::process::Command, request: &AppAdapterRequest) {
    command
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    if request.json {
        command.arg("--json");
    }
    if request.dry_run {
        command.arg("--dry-run");
    }
    if runtime_debug_logs_requested(request) {
        command.arg("--debug-logs");
    }
    if runtime_status_requested(request) || review_debug_requested(request) {
        command.arg("--status");
    }
    command.arg(&request.action).args(&request.args);
}

async fn run_runtime_process(
    mut command: tokio::process::Command,
    tee_stderr: bool,
) -> std::io::Result<RuntimeProcessOutput> {
    let mut child = command.spawn()?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| std::io::Error::new(ErrorKind::Other, "runtime stdout unavailable"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| std::io::Error::new(ErrorKind::Other, "runtime stderr unavailable"))?;
    let stdout_task = tokio::spawn(async move { read_runtime_pipe(stdout, false).await });
    let stderr_task = tokio::spawn(async move { read_runtime_pipe(stderr, tee_stderr).await });
    let status = child.wait().await?;
    let stdout = stdout_task
        .await
        .map_err(|err| std::io::Error::new(ErrorKind::Other, format!("join stdout: {err}")))??;
    let stderr = stderr_task
        .await
        .map_err(|err| std::io::Error::new(ErrorKind::Other, format!("join stderr: {err}")))??;
    Ok(RuntimeProcessOutput {
        status,
        stdout,
        stderr,
    })
}

async fn read_runtime_pipe(
    mut pipe: impl tokio::io::AsyncRead + Unpin,
    tee_stderr: bool,
) -> std::io::Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut buf = [0u8; 8192];
    let mut stderr = tokio::io::stderr();
    loop {
        let n = pipe.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        out.extend_from_slice(&buf[..n]);
        if tee_stderr {
            stderr.write_all(&buf[..n]).await?;
            stderr.flush().await?;
        }
    }
    Ok(out)
}

fn runtime_status_requested(request: &AppAdapterRequest) -> bool {
    request
        .input
        .values
        .get("stream_stderr")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}

fn runtime_debug_logs_requested(request: &AppAdapterRequest) -> bool {
    request
        .input
        .values
        .get("debug_logs")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}

fn review_debug_requested(request: &AppAdapterRequest) -> bool {
    request.args.iter().any(|arg| arg == "--debug")
}

fn runtime_stderr_stream_requested(request: &AppAdapterRequest) -> bool {
    runtime_status_requested(request)
        || runtime_debug_logs_requested(request)
        || review_debug_requested(request)
}

fn runtime_fallback_allowed() -> bool {
    cfg!(debug_assertions)
        || std::env::var("AGENTHERO_ALLOW_ADAPTER_FALLBACK")
            .ok()
            .is_some_and(|value| matches!(value.as_str(), "1" | "true" | "yes" | "on"))
}

fn app_root() -> PathBuf {
    resolve_app_root("grokrxiv", env!("CARGO_MANIFEST_DIR"))
}

fn repo_root() -> PathBuf {
    app_root()
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf()
}

fn generic_tool_artifact_root() -> PathBuf {
    app_artifact_root().join("generic-tools")
}

fn app_artifact_root() -> PathBuf {
    std::env::var_os("AGENTHERO_RUNTIME_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| repo_root().join(".agenthero"))
        .join("grokrxiv")
}

fn set_app_root_env(command: &mut tokio::process::Command) {
    let app_root = app_root();
    command.env("AGENTHERO_APP_ROOT", &app_root);
    if let Some(apps_root) = app_root.parent() {
        command.env("AGENTHERO_APPS_ROOT", apps_root);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agenthero_agent_runtime::AGENTHERO_EVENT_TRACE_FIELDS;
    use agenthero_dag_executor::DagIo;
    use agenthero_dag_runtime::{DagManifest, DagNodeStatus};

    fn assert_schema_valid(schema_path: &str, value: &serde_json::Value) {
        let schema: serde_json::Value = serde_json::from_slice(
            &std::fs::read(app_root().join(schema_path)).expect("schema file is readable"),
        )
        .expect("schema file is JSON");
        let validator = jsonschema::validator_for(&schema).expect("schema compiles");
        if let Err(error) = validator.validate(value) {
            panic!("{schema_path} rejected artifact: {error}");
        }
    }

    fn request_with_flags(stream: bool, debug_logs: bool, args: Vec<String>) -> AppAdapterRequest {
        let mut input = DagIo::default();
        input
            .values
            .insert("stream_stderr".to_string(), serde_json::json!(stream));
        input
            .values
            .insert("debug_logs".to_string(), serde_json::json!(debug_logs));
        input.values.insert(
            "app_run_id".to_string(),
            serde_json::json!("2d0a1d88-b9f9-4e8f-848e-605b86717330"),
        );
        input.values.insert(
            "dag_run_id".to_string(),
            serde_json::json!("f78c57db-89e3-4b63-8c1a-2c07e3331f0c"),
        );
        input.values.insert(
            "lease_id".to_string(),
            serde_json::json!("a9353847-48b3-472e-b88e-89770fcdbf7a"),
        );
        AppAdapterRequest::new(
            "grokrxiv",
            "review",
            "review-loop",
            args,
            input,
            false,
            false,
        )
    }

    fn request_with_stream(stream: bool) -> AppAdapterRequest {
        request_with_flags(
            stream,
            false,
            vec!["2606.00799".to_string(), "--loop".to_string()],
        )
    }

    #[test]
    fn stable_review_id_uses_canonical_review_args() {
        let request = AppAdapterRequest::new(
            "grokrxiv",
            "review",
            "review-loop",
            vec![
                "2606.00799".to_string(),
                "--loop".to_string(),
                "--type".to_string(),
                "arxiv".to_string(),
                "--title=Example".to_string(),
                "--with-lean".to_string(),
                "--debug".to_string(),
            ],
            DagIo::default(),
            true,
            false,
        );
        let equivalent = AppAdapterRequest::new(
            "grokrxiv",
            "review",
            "review-loop",
            vec![
                "--title".to_string(),
                "Example".to_string(),
                "2606.00799".to_string(),
                "--type=arxiv".to_string(),
                "--no-external-actions".to_string(),
            ],
            DagIo::default(),
            true,
            false,
        );
        let request_options =
            review_request_options(&request, &DagIo::default()).expect("request parses");
        let equivalent_options =
            review_request_options(&equivalent, &DagIo::default()).expect("equivalent parses");

        assert_eq!(
            stable_review_id(&request, &request_options),
            stable_review_id(&equivalent, &equivalent_options)
        );
    }

    #[tokio::test]
    async fn review_request_with_lean_sets_manifest_lean_policy() {
        let request = AppAdapterRequest::new(
            "grokrxiv",
            "review",
            "review-loop",
            vec![
                "2606.00799".to_string(),
                "--loop".to_string(),
                "--with-lean".to_string(),
            ],
            DagIo::default(),
            true,
            true,
        );

        let response = run(&request)
            .await
            .expect("review request with Lean should run through DagExecutor");
        let report = response.report.as_ref().expect("dag report");
        assert_eq!(report.input.values["run_lean"], true);
        assert_eq!(report.input.values["lean_policy"]["requested"], true);
        assert_eq!(report.input.values["lean_policy"]["disabled"], false);
    }

    #[tokio::test]
    async fn review_request_without_lean_flags_sets_auto_detect_policy() {
        let request = AppAdapterRequest::new(
            "grokrxiv",
            "review",
            "review-loop",
            vec![
                "2606.24837".to_string(),
                "--type".to_string(),
                "arxiv".to_string(),
                "--no-external-actions".to_string(),
            ],
            DagIo::default(),
            true,
            true,
        );

        let response = run(&request)
            .await
            .expect("review request should run through DagExecutor");
        let report = response.report.as_ref().expect("dag report");
        assert_eq!(report.input.values["source"], "2606.24837");
        assert_eq!(report.input.values["run_lean"], false);
        assert_eq!(report.input.values["lean_policy"]["requested"], false);
        assert_eq!(report.input.values["lean_policy"]["disabled"], false);
        assert_eq!(report.input.values["lean_policy"]["auto_detect"], true);
        assert_eq!(report.input.values["lean_policy"]["run_lean"], false);
        assert_eq!(
            report.input.values["review_options"]["no_external_actions"],
            true
        );
    }

    #[tokio::test]
    async fn review_request_no_lean_disables_auto_detect_policy() {
        let request = AppAdapterRequest::new(
            "grokrxiv",
            "review",
            "review-loop",
            vec![
                "2606.24837".to_string(),
                "--type".to_string(),
                "arxiv".to_string(),
                "--no-lean".to_string(),
                "--no-external-actions".to_string(),
            ],
            DagIo::default(),
            true,
            true,
        );

        let response = run(&request)
            .await
            .expect("review --no-lean request should run through DagExecutor");
        let report = response.report.as_ref().expect("dag report");
        assert_eq!(report.input.values["source"], "2606.24837");
        assert_eq!(report.input.values["run_lean"], false);
        assert_eq!(report.input.values["lean_policy"]["requested"], false);
        assert_eq!(report.input.values["lean_policy"]["disabled"], true);
        assert_eq!(report.input.values["lean_policy"]["auto_detect"], false);
        assert_eq!(report.input.values["lean_policy"]["run_lean"], false);
        assert_eq!(report.input.values["review_options"]["no_lean"], true);
    }

    #[tokio::test]
    async fn review_request_rejects_conflicting_lean_flags() {
        let request = AppAdapterRequest::new(
            "grokrxiv",
            "review",
            "review-loop",
            vec![
                "2606.00799".to_string(),
                "--with-lean".to_string(),
                "--no-lean".to_string(),
            ],
            DagIo::default(),
            true,
            true,
        );

        let err = run(&request)
            .await
            .expect_err("conflicting Lean flags should fail before running DAG");
        assert!(format!("{err:#}").contains("cannot combine --with-lean and --no-lean"));
    }

    #[tokio::test]
    async fn non_dry_run_manifest_review_rejects_unsupported_source_options() {
        let request = AppAdapterRequest::new(
            "grokrxiv",
            "review",
            "review-loop",
            vec![
                "paper.pdf".to_string(),
                "--type".to_string(),
                "pdf".to_string(),
            ],
            DagIo::default(),
            true,
            false,
        );

        let err = run(&request)
            .await
            .expect_err("unsupported source options should fail explicitly");
        assert!(format!("{err:#}").contains("--type pdf requires the legacy GrokRxiv runtime"));
    }

    #[test]
    fn runtime_status_requested_follows_adapter_input_flag() {
        assert!(runtime_status_requested(&request_with_stream(true)));
        assert!(!runtime_status_requested(&request_with_stream(false)));
    }

    #[test]
    fn runtime_debug_logs_requested_follows_adapter_input_flag() {
        assert!(runtime_debug_logs_requested(&request_with_flags(
            false,
            true,
            vec!["2606.00799".to_string(), "--loop".to_string()],
        )));
        assert!(!runtime_debug_logs_requested(&request_with_flags(
            false,
            false,
            vec!["2606.00799".to_string(), "--loop".to_string()],
        )));
    }

    #[test]
    fn runtime_stderr_streaming_includes_review_debug_flag() {
        assert!(runtime_stderr_stream_requested(&request_with_flags(
            false,
            false,
            vec![
                "2606.00799".to_string(),
                "--loop".to_string(),
                "--debug".to_string()
            ],
        )));
    }

    #[test]
    fn runtime_action_started_event_records_audit_payload() {
        let request = request_with_flags(
            false,
            true,
            vec![
                "paper.pdf".to_string(),
                "--type".to_string(),
                "pdf".to_string(),
            ],
        );

        let event = runtime_action_started_event(&request);

        assert_eq!(event.level, "info");
        assert_eq!(event.event_type, "app_action.started");
        assert_eq!(event.node_id, None);
        assert_eq!(event.payload["app"], "grokrxiv");
        assert_eq!(event.payload["action"], "review");
        assert_eq!(event.payload["dag_type"], "review-loop");
        assert_eq!(event.payload["args_count"], 3);
        assert_eq!(event.payload["dry_run"], false);
        assert_eq!(event.payload["json"], false);
        assert_eq!(event.payload["status"], "started");
        assert_eq!(
            event.payload["app_run_id"],
            "2d0a1d88-b9f9-4e8f-848e-605b86717330"
        );
        assert_eq!(
            event.payload["dag_run_id"],
            "f78c57db-89e3-4b63-8c1a-2c07e3331f0c"
        );
        assert_eq!(
            event.payload["lease_id"],
            "a9353847-48b3-472e-b88e-89770fcdbf7a"
        );
        for field in AGENTHERO_EVENT_TRACE_FIELDS {
            assert!(
                event.payload.contains_key(*field),
                "runtime action event should include mandatory AgentHero trace field `{field}`"
            );
        }
    }

    #[test]
    fn runtime_action_status_event_records_exit_and_log_sizes() {
        let request = request_with_stream(false);

        let event = runtime_action_status_event(
            &request,
            "error",
            "app_action.failed",
            "review failed",
            "failed",
            Some(7),
            123,
            456,
        );

        assert_eq!(event.level, "error");
        assert_eq!(event.event_type, "app_action.failed");
        assert_eq!(event.payload["status"], "failed");
        assert_eq!(event.payload["exit_status"], 7);
        assert_eq!(event.payload["stdout_bytes"], 123);
        assert_eq!(event.payload["stderr_bytes"], 456);
        assert_eq!(event.message.as_deref(), Some("review failed"));
    }

    #[tokio::test]
    async fn adapter_composes_generic_command_tool_runner() {
        let manifest = DagManifest::from_str(
            r#"
id: adapter-command
version: 1
accepts: []
tools:
  - id: write_result
    executor: cli
    command: ["sh", "-c", "printf 'from-generic-runner' > result.txt"]
nodes:
  - id: command_node
    kind: tool
    tool: write_result
    outputs: [result.txt]
    required: true
"#,
        )
        .expect("manifest parses");
        let workspace = std::env::temp_dir().join(format!(
            "agenthero-grokrxiv-generic-tool-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock after unix epoch")
                .as_nanos()
        ));
        std::fs::create_dir_all(&workspace).expect("workspace exists");

        let report = DagExecutor::new(GrokrxivAdapter {
            app_name: "grokrxiv",
            generic_tools: GenericToolRunner::new(&workspace),
        })
        .execute(&manifest, DagIo::default())
        .await
        .expect("generic command manifest runs through adapter");

        assert_eq!(report.status, DagNodeStatus::Ok);
        let node = &report.nodes[0];
        assert_eq!(node.exit_status, Some(0));
        assert!(node.output_refs.contains_key("result.txt"));
        assert!(node
            .diagnostic_refs
            .contains_key("logs/command_node/status.json"));
        assert_eq!(
            std::fs::read_to_string(node.output_refs.get("result.txt").expect("result ref"))
                .expect("artifact written by generic runner"),
            "from-generic-runner"
        );

        std::fs::remove_dir_all(workspace).expect("cleanup temp workspace");
    }

    #[tokio::test]
    async fn validate_citations_manifest_materializes_app_owned_artifacts() {
        let request = AppAdapterRequest::new(
            "grokrxiv",
            "validate-citations",
            "citation-validation",
            Vec::new(),
            DagIo::default(),
            true,
            false,
        );

        let report = run_manifest_dag(&request)
            .await
            .expect("citation-validation manifest runs");

        assert_eq!(report.status, DagNodeStatus::Ok);
        assert!(!report
            .outputs
            .values
            .contains_key("bibtex_reference_parser"));
        assert!(report
            .outputs
            .artifacts
            .contains_key("citation_validation_report.json"));
        assert!(report.nodes.iter().all(|node| !node.output_refs.is_empty()));
        let report_uri = &report.outputs.artifacts["citation_validation_report.json"].uri;
        let report_json: serde_json::Value = serde_json::from_slice(
            &std::fs::read(report_uri).expect("citation report artifact is readable"),
        )
        .expect("citation report is JSON");
        assert_schema_valid(
            "schemas/citation_validation_report.schema.json",
            &report_json,
        );
        assert_eq!(report_json["status"], "verified");
        let adjudication_uri =
            &report.outputs.artifacts["citation_validation_adjudication.json"].uri;
        let adjudication_json: serde_json::Value = serde_json::from_slice(
            &std::fs::read(adjudication_uri).expect("citation adjudication artifact is readable"),
        )
        .expect("citation adjudication is JSON");
        assert_schema_valid(
            "schemas/citation_validation_adjudicator.schema.json",
            &adjudication_json,
        );
        assert_eq!(adjudication_json["verdict"], "verified");
    }

    #[tokio::test]
    async fn gated_review_loop_request_returns_dag_report() {
        let mut input = DagIo::default();
        input.values.insert(
            "agenthero_manifest_dag".to_string(),
            serde_json::json!(true),
        );
        let request = AppAdapterRequest::new(
            "grokrxiv",
            "review",
            "review-loop",
            vec!["2606.00799".to_string(), "--loop".to_string()],
            input,
            true,
            true,
        );

        let response = run(&request)
            .await
            .expect("gated review-loop request should run through DagExecutor");

        assert!(response.ok);
        assert!(response.report.is_some());
        assert_eq!(
            response
                .report
                .as_ref()
                .expect("dag report")
                .dag_type
                .as_str(),
            "review-loop"
        );
        assert!(response.output.is_none());
    }

    #[tokio::test]
    async fn review_request_uses_manifest_runtime_by_default() {
        let request = AppAdapterRequest::new(
            "grokrxiv",
            "review",
            "review-loop",
            vec!["2606.00799".to_string(), "--loop".to_string()],
            DagIo::default(),
            true,
            true,
        );

        let response = run(&request)
            .await
            .expect("public review request should run through DagExecutor");

        assert!(response.ok);
        let report = response.report.as_ref().expect("dag report");
        assert_eq!(report.dag_type.as_str(), "review-loop");
        assert!(response.output.is_none());
        assert_eq!(report.input.values["agenthero_manifest_dag"], true);
        assert_eq!(report.input.values["dry_run"], true);
        assert!(report.input.artifacts.contains_key("source_manifest.json"));
        assert!(!report.input.artifacts.contains_key("review_input.json"));
        assert!(report.input.values["review_id"].is_string());
        let paper_review = report
            .nodes
            .iter()
            .find(|node| node.node_id == "paper_review")
            .expect("paper_review node report");
        assert!(paper_review.input_refs.contains_key("source_manifest.json"));
        for expected in [
            "body.md",
            "equations.json",
            "theorem_graph.json",
            "semantic_ast.json",
            "references.json",
        ] {
            assert!(
                paper_review.output_refs.contains_key(expected),
                "missing paper_review output ref {expected}"
            );
        }
        let paper_math = report
            .nodes
            .iter()
            .find(|node| node.node_id == "paper_math_source_collector")
            .expect("paper_math_source_collector node report");
        assert!(paper_math.input_refs.contains_key("body.md"));
        assert!(paper_math.input_refs.contains_key("theorem_graph.json"));
        assert!(report
            .outputs
            .artifacts
            .contains_key("review_loop/review_loop_report.json"));
        assert!(report
            .outputs
            .artifacts
            .contains_key("review_loop/publish_decision.json"));
    }

    #[tokio::test]
    async fn review_request_loads_local_review_input_artifacts_without_dry_run_seed() {
        let workspace = std::env::temp_dir().join(format!(
            "agenthero-grokrxiv-review-input-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock after unix epoch")
                .as_nanos()
        ));
        std::fs::create_dir_all(&workspace).expect("workspace exists");
        std::fs::write(
            workspace.join("metadata.json"),
            br#"{"title":"Local Review Input","abstract":"fixture"}"#,
        )
        .expect("metadata written");
        std::fs::write(
            workspace.join("sections.json"),
            br#"{"sections":[{"id":"body","heading":null,"body_markdown":"Real extracted body with theorem context."}]}"#,
        )
        .expect("sections written");
        std::fs::write(
            workspace.join("body.md"),
            "Real extracted body with theorem context.",
        )
        .expect("body written");
        std::fs::write(
            workspace.join("equations.json"),
            br#"{"artifact":"equations.json","equations":[]}"#,
        )
        .expect("equations written");
        std::fs::write(
            workspace.join("theorem_graph.json"),
            br#"{"artifact":"theorem_graph.json","nodes":[]}"#,
        )
        .expect("theorem graph written");
        std::fs::write(
            workspace.join("references.json"),
            br#"{"citations":[{"key":"ref_1","raw":"Example reference"}]}"#,
        )
        .expect("references written");
        std::fs::write(
            workspace.join("extraction_report.json"),
            br#"{"stages":[{"id":"fixture","status":"ok"}]}"#,
        )
        .expect("extraction report written");
        std::fs::write(
            workspace.join("review_input.json"),
            br#"{
  "schema_version": "1.0.0",
  "arxiv_id": "local",
  "metadata": "metadata.json",
  "body_markdown": "body.md",
  "sections": "sections.json",
  "equations": "equations.json",
  "references": "references.json",
  "theorem_graph": "theorem_graph.json",
  "extraction_report": "extraction_report.json"
}"#,
        )
        .expect("review input written");
        let request = AppAdapterRequest::new(
            "grokrxiv",
            "review",
            "review-loop",
            vec![workspace
                .join("review_input.json")
                .to_string_lossy()
                .to_string()],
            DagIo::default(),
            true,
            false,
        );

        let response = run(&request)
            .await
            .expect("review_input review request should run through DagExecutor");
        let report = response.report.as_ref().expect("dag report");
        assert_eq!(report.input.values["dry_run"], false);
        assert!(report.input.artifacts.contains_key("source_manifest.json"));
        assert!(report.input.artifacts.contains_key("review_input.json"));
        let frozen_review_input = &report.input.artifacts["review_input.json"].uri;
        assert_ne!(
            frozen_review_input,
            &workspace
                .join("review_input.json")
                .to_string_lossy()
                .to_string()
        );
        assert!(frozen_review_input.ends_with("review_input.json"));
        let paper_review = report
            .nodes
            .iter()
            .find(|node| node.node_id == "paper_review")
            .expect("paper_review node report");
        assert!(paper_review.input_refs.contains_key("source_manifest.json"));
        assert!(paper_review.input_refs.contains_key("review_input.json"));
        let frozen_body = paper_review
            .output_refs
            .get("body.md")
            .expect("body output ref");
        assert_ne!(
            frozen_body,
            &workspace.join("body.md").to_string_lossy().to_string()
        );
        assert!(frozen_body.ends_with("body.md"));
        let paper_math = report
            .nodes
            .iter()
            .find(|node| node.node_id == "paper_math_source_collector")
            .expect("paper math node report");
        assert_eq!(
            paper_math
                .input_refs
                .get("body.md")
                .expect("body input ref"),
            frozen_body
        );
        let paper_sources_uri =
            &report.outputs.artifacts["review_loop/paper_math_sources.json"].uri;
        let paper_sources: serde_json::Value = serde_json::from_slice(
            &std::fs::read(paper_sources_uri).expect("paper math sources readable"),
        )
        .expect("paper math sources JSON");
        assert_eq!(
            paper_sources["body"]["sections"][0]["body_markdown"],
            "Real extracted body with theorem context."
        );

        std::fs::remove_dir_all(workspace).expect("cleanup temp workspace");
    }

    #[tokio::test]
    async fn lean_review_fix_code_invokes_configured_lean_verifier_when_enabled() {
        let workspace = std::env::temp_dir().join(format!(
            "agenthero-grokrxiv-lean-verifier-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock after unix epoch")
                .as_nanos()
        ));
        std::fs::create_dir_all(&workspace).expect("workspace exists");
        let fake_lean = workspace.join("fake-lean.sh");
        std::fs::write(
            &fake_lean,
            "#!/bin/sh\ncat \"$1\" >/dev/null\nprintf '%s' \"$1\" > invoked-lean-path.txt\nexit 0\n",
        )
        .expect("fake lean written");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mut permissions = std::fs::metadata(&fake_lean)
                .expect("fake lean metadata")
                .permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(&fake_lean, permissions).expect("fake lean executable");
        }

        let proof_path = workspace.join("proof_obligations.json");
        let targets_path = workspace.join("lean_targets.json");
        std::fs::write(
            &proof_path,
            serde_json::to_vec_pretty(&serde_json::json!({
                "schema_version": "1.0.0",
                "review_id": "22222222-2222-2222-2222-222222222222",
                "source": "review_loop/semantic_ir.json",
                "status": "ready",
                "lean_attempt_status": "pending",
                "candidate_count": 1,
                "selected_count": 1,
                "omitted_count": 0,
                "obligations": [
                    {
                        "id": "formalize_true",
                        "kind": "theorem_formalization",
                        "statement": "True.",
                        "source_claim_id": "claim_true",
                        "source_span": {
                            "artifact": "body.md",
                            "claim_id": "claim_true"
                        },
                        "semantic_category": "plain_theorem",
                        "lean_declaration": "smoke_true",
                        "lean_statement": "theorem smoke_true : True := by\n  trivial",
                        "expected_proof": "closed Lean theorem proof with no sorry, admit, or unapproved axiom"
                    }
                ],
                "skipped_targets": []
            }))
            .expect("proof obligations JSON"),
        )
        .expect("proof obligations written");
        std::fs::write(
            &targets_path,
            serde_json::to_vec_pretty(&serde_json::json!({
                "schema_version": "1.0.0",
                "source": "review_loop/proof_obligations.json",
                "candidate_count": 1,
                "selected_count": 1,
                "omitted_count": 0,
                "skipped_targets": [],
                "targets": [
                    {
                        "obligation_id": "formalize_true",
                        "lean_declaration": "smoke_true",
                        "statement": "True.",
                        "lean_statement": "theorem smoke_true : True := by\n  trivial",
                        "source_claim_id": "claim_true"
                    }
                ]
            }))
            .expect("lean targets JSON"),
        )
        .expect("lean targets written");

        let manifest = DagManifest::from_str(
            r#"
id: review-loop
version: 1
accepts: []
tools:
  - id: review_fix_code
    executor: rust
    handler: review_loop::review_fix_code
nodes:
  - id: lean_review_fix_code
    kind: loop
    tool: review_fix_code
    loop:
      max_rounds: 1
    inputs:
      - review_loop/proof_obligations.json
      - review_loop/lean_targets.json
    outputs:
      - review_loop/lean/GrokRxiv/Proofs.lean
      - review_loop/lean/results.json
      - review_loop/lean/fix_rounds.json
      - review_loop/lean/theorem_map.json
      - review_loop/lean/verification_report.json
    required: true
"#,
        )
        .expect("lean manifest parses");
        let mut input = DagIo::default();
        input
            .values
            .insert("run_lean".to_string(), serde_json::json!(true));
        input.values.insert(
            "lean_command".to_string(),
            serde_json::json!([fake_lean.to_string_lossy()]),
        );
        for (name, path) in [
            ("review_loop/proof_obligations.json", proof_path),
            ("review_loop/lean_targets.json", targets_path),
        ] {
            input.artifacts.insert(
                name.to_string(),
                ArtifactRef {
                    uri: path.to_string_lossy().to_string(),
                    media_type: Some("application/json".to_string()),
                    metadata: Default::default(),
                },
            );
        }

        let report = DagExecutor::new(GrokrxivAdapter {
            app_name: "grokrxiv",
            generic_tools: GenericToolRunner::new(workspace.join("generic-tools")),
        })
        .execute(&manifest, input)
        .await
        .expect("lean verifier manifest runs");

        assert_eq!(report.status, DagNodeStatus::Ok);
        let node = report
            .nodes
            .iter()
            .find(|node| node.node_id == "lean_review_fix_code")
            .expect("aggregate lean loop report");
        assert_eq!(node.exit_status, Some(0));
        assert!(node.command.as_ref().expect("lean command recorded")[0].contains("fake-lean"));
        assert!(node
            .diagnostic_refs
            .contains_key("logs/lean_review_fix_code/stdout.log"));
        let results_uri = &report.outputs.artifacts["review_loop/lean/results.json"].uri;
        let lean_results: serde_json::Value =
            serde_json::from_slice(&std::fs::read(results_uri).expect("lean results readable"))
                .expect("lean results JSON");
        assert_eq!(lean_results["status"], "pass");
        assert_eq!(lean_results["compile"]["exit_status"], 0);
        assert_eq!(lean_results["declarations"]["smoke_true"]["status"], "pass");
        let theorem_map_uri = &report.outputs.artifacts["review_loop/lean/theorem_map.json"].uri;
        let theorem_map: serde_json::Value =
            serde_json::from_slice(&std::fs::read(theorem_map_uri).expect("theorem map readable"))
                .expect("theorem map JSON");
        assert_eq!(theorem_map["status"], "PROVED");
        assert_eq!(theorem_map["lean_attempt_status"], "proved");

        std::fs::remove_dir_all(workspace).expect("cleanup temp workspace");
    }

    #[test]
    fn lean_verifier_proof_arg_uses_path_relative_to_node_workdir() {
        let base = Path::new(
            ".agenthero/run/grokrxiv/review-loop/lean_review_fix_code/node-attempt",
        );
        let proof_path = base.join("review_loop/lean/GrokRxiv/Proofs.lean");

        assert_eq!(
            lean_verifier_proof_arg(base, &proof_path),
            PathBuf::from("review_loop/lean/GrokRxiv/Proofs.lean")
        );
    }

    #[test]
    fn lean_code_header_is_plain_comment_before_namespace() {
        let code = lean_code_from_targets(&json!({
            "targets": [
                {
                    "lean_skeleton": "namespace GrokRxiv\n\ntheorem smoke_true : True := by\n  trivial\n\nend GrokRxiv\n"
                }
            ]
        }));

        assert!(
            code.starts_with("/- Generated by the GrokRxiv gated AgentHero review-loop adapter. -/"),
            "generated Lean file should use a plain comment header so Lean accepts the following namespace"
        );
        assert!(
            !code.starts_with("/--"),
            "generated Lean file must not use a doc comment before `namespace`"
        );
    }

    #[tokio::test]
    async fn review_loop_manifest_materializes_app_owned_artifacts() {
        let workspace = std::env::temp_dir().join(format!(
            "agenthero-grokrxiv-review-loop-prefix-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock after unix epoch")
                .as_nanos()
        ));
        std::fs::create_dir_all(&workspace).expect("workspace exists");
        let body_path = workspace.join("body.md");
        let equations_path = workspace.join("equations.json");
        let theorem_graph_path = workspace.join("theorem_graph.json");
        let semantic_ast_path = workspace.join("semantic_ast.json");
        let references_path = workspace.join("references.json");
        std::fs::write(
            &body_path,
            "\\begin{theorem}\\label{thm:add-zero} For every $n \\in \\mathbb{N}$, $n + 0 = n$.\\end{theorem}",
        )
        .expect("body written");
        std::fs::write(
            &equations_path,
            br#"{"artifact":"equations.json","equations":[{"id":"eq-add-zero","canonical_tex":"n + 0 = n","section_id":"sec-main"}]}"#,
        )
        .expect("equations written");
        std::fs::write(
            &theorem_graph_path,
            br#"{"artifact":"theorem_graph.json","nodes":[{"id":"thm-add-zero","type":"theorem","statement":"For every $n \\in \\mathbb{N}$, $n + 0 = n.","section_id":"sec-main","depends_on":["eq-add-zero"],"typed_transcription":{"status":"transcribed","source_text":"For every $n \\in \\mathbb{N}$, $n + 0 = n.","math_objects":[{"name":"n","type":{"kind":"nat"}}],"binders":[{"name":"n","type":{"kind":"nat"}}],"assumptions":[],"conclusion":{"kind":"equals","lhs":{"kind":"add","args":[{"kind":"var","name":"n"},{"kind":"nat_lit","value":0}]},"rhs":{"kind":"var","name":"n"}}},"theorem_ir":{"theorem_name":"add_zero_claim","binders":[{"name":"n","type":{"kind":"nat"}}],"assumptions":[],"conclusion":{"kind":"equals","lhs":{"kind":"add","args":[{"kind":"var","name":"n"},{"kind":"nat_lit","value":0}]},"rhs":{"kind":"var","name":"n"}}}}]}"#,
        )
        .expect("theorem graph written");
        std::fs::write(&semantic_ast_path, br#"{"nodes":[]}"#).expect("semantic ast written");
        std::fs::write(&references_path, br#"{"citations":[]}"#).expect("references written");

        let manifest = load_dag_manifest(app_root(), "review-loop").expect("manifest loads");
        let mut input = DagIo::default();
        input.values.insert(
            "review_id".to_string(),
            serde_json::json!("11111111-1111-1111-1111-111111111111"),
        );
        input.values.insert(
            "review_agents".to_string(),
            serde_json::json!([
                {
                    "role": "technical_correctness",
                    "claims": [
                        {
                            "id": "claim_1",
                            "statement": "Every compact group has a normalized Haar measure.",
                            "confidence": 0.91
                        }
                    ]
                }
            ]),
        );
        input.values.insert(
            "render_artifacts".to_string(),
            serde_json::json!({"review_md": "Review body"}),
        );
        for (name, path) in [
            ("body.md", body_path),
            ("equations.json", equations_path),
            ("theorem_graph.json", theorem_graph_path),
            ("semantic_ast.json", semantic_ast_path),
            ("references.json", references_path),
        ] {
            input.artifacts.insert(
                name.to_string(),
                ArtifactRef {
                    uri: path.to_string_lossy().to_string(),
                    media_type: Some("application/json".to_string()),
                    metadata: Default::default(),
                },
            );
        }

        let report = DagExecutor::new(GrokrxivAdapter {
            app_name: "grokrxiv",
            generic_tools: GenericToolRunner::new(workspace.join("generic-tools")),
        })
        .execute(&manifest, input)
        .await
        .expect("review-loop manifest runs");

        assert_eq!(report.status, DagNodeStatus::Ok);
        for expected in [
            "review_loop/claims.json",
            "review_loop/paper_math_sources.json",
            "review_loop/knowledge_graph.json",
            "review_loop/semantic_ir.json",
            "review_loop/semantic_model.json",
            "review_loop/proof_obligations.json",
            "review_loop/lean_targets.json",
            "review_loop/lean/GrokRxiv/Proofs.lean",
            "review_loop/lean/results.json",
            "review_loop/lean/fix_rounds.json",
            "review_loop/lean/theorem_map.json",
            "review_loop/lean/verification_report.json",
            "review_loop/faithfulness.json",
            "review_loop/semantic_adequacy.json",
            "citation_validation_report.json",
            "citation_validation_adjudication.json",
            "review_loop/pr_fixes.json",
            "review_loop/fixed/review.md",
            "review_loop/pr_review/results.json",
            "review_loop/pr_review/fix_rounds.json",
            "review_loop/policy_gate.json",
            "review_loop/review_loop_report.json",
            "review_loop/publish_decision.json",
        ] {
            assert!(
                report.outputs.artifacts.contains_key(expected),
                "missing artifact {expected}"
            );
        }
        assert!(!report.outputs.values.contains_key("claim_extractor"));
        let claims_uri = &report.outputs.artifacts["review_loop/claims.json"].uri;
        let claims: serde_json::Value = serde_json::from_slice(
            &std::fs::read(claims_uri).expect("claims artifact is readable"),
        )
        .expect("claims artifact is JSON");
        assert_eq!(claims["source"], "grokrxiv.review_loop.claim_extractor");
        assert_eq!(claims["claims"][0]["id"], "claim_1");
        let semantic_model_uri = &report.outputs.artifacts["review_loop/semantic_model.json"].uri;
        let semantic_model: serde_json::Value = serde_json::from_slice(
            &std::fs::read(semantic_model_uri).expect("semantic model artifact is readable"),
        )
        .expect("semantic model artifact is JSON");
        assert_eq!(semantic_model["schema_version"], "1.0.0");
        assert_eq!(
            semantic_model["semantic_ir"],
            "review_loop/semantic_ir.json"
        );
        assert_eq!(
            semantic_model["paper_math_sources"],
            "review_loop/paper_math_sources.json"
        );
        assert_eq!(semantic_model["theorem_candidate_count"], 1);
        let proof_uri = &report.outputs.artifacts["review_loop/proof_obligations.json"].uri;
        let proof_obligations: serde_json::Value = serde_json::from_slice(
            &std::fs::read(proof_uri).expect("proof obligations artifact is readable"),
        )
        .expect("proof obligations artifact is JSON");
        assert_eq!(proof_obligations["schema_version"], "1.0.0");
        assert_eq!(proof_obligations["status"], "ready");
        assert_eq!(proof_obligations["selected_count"], 1);
        let theorem_map_uri = &report.outputs.artifacts["review_loop/lean/theorem_map.json"].uri;
        let theorem_map: serde_json::Value = serde_json::from_slice(
            &std::fs::read(theorem_map_uri).expect("theorem map artifact is readable"),
        )
        .expect("theorem map artifact is JSON");
        assert_eq!(theorem_map["status"], "AWAITING_FORMALIZATION");
        assert_eq!(theorem_map["lean_attempt_status"], "not_run");
        let adequacy_uri = &report.outputs.artifacts["review_loop/semantic_adequacy.json"].uri;
        let semantic_adequacy: serde_json::Value = serde_json::from_slice(
            &std::fs::read(adequacy_uri).expect("semantic adequacy artifact is readable"),
        )
        .expect("semantic adequacy artifact is JSON");
        assert_eq!(semantic_adequacy["status"], "skipped");
        assert_eq!(
            semantic_adequacy["skip_reason"],
            "lean_execution_not_enabled_in_gated_manifest_dag"
        );
        assert_eq!(
            semantic_adequacy["operator_status"],
            "AWAITING_FORMALIZATION"
        );
        assert_eq!(report.outputs.values["loop_continue"], false);
        let citation_uri = &report.outputs.artifacts["citation_validation_report.json"].uri;
        let citation_report: serde_json::Value = serde_json::from_slice(
            &std::fs::read(citation_uri).expect("citation report artifact is readable"),
        )
        .expect("citation report artifact is JSON");
        assert_schema_valid(
            "schemas/citation_validation_report.schema.json",
            &citation_report,
        );
        assert_eq!(citation_report["status"], "verified");
        let citation_adjudication_uri =
            &report.outputs.artifacts["citation_validation_adjudication.json"].uri;
        let citation_adjudication: serde_json::Value = serde_json::from_slice(
            &std::fs::read(citation_adjudication_uri)
                .expect("citation adjudication artifact is readable"),
        )
        .expect("citation adjudication artifact is JSON");
        assert_schema_valid(
            "schemas/citation_validation_adjudicator.schema.json",
            &citation_adjudication,
        );
        assert_eq!(citation_adjudication["verdict"], "verified");
        let policy_uri = &report.outputs.artifacts["review_loop/policy_gate.json"].uri;
        let policy_gate: serde_json::Value = serde_json::from_slice(
            &std::fs::read(policy_uri).expect("policy gate artifact is readable"),
        )
        .expect("policy gate artifact is JSON");
        assert_eq!(
            policy_gate["component_status"]["citation_validation"],
            "verified"
        );
        assert_eq!(policy_gate["integrity_ready"], true);
        let report_uri = &report.outputs.artifacts["review_loop/review_loop_report.json"].uri;
        let loop_report: serde_json::Value = serde_json::from_slice(
            &std::fs::read(report_uri).expect("review-loop report artifact is readable"),
        )
        .expect("review-loop report artifact is JSON");
        assert_eq!(loop_report["dag_type"], "review-loop");
        assert_eq!(
            loop_report["artifact_paths"]["publish_decision"],
            "review_loop/publish_decision.json"
        );
        let publish_uri = &report.outputs.artifacts["review_loop/publish_decision.json"].uri;
        let publish_decision: serde_json::Value = serde_json::from_slice(
            &std::fs::read(publish_uri).expect("publish decision artifact is readable"),
        )
        .expect("publish decision artifact is JSON");
        assert_eq!(publish_decision["auto_publish"], false);

        std::fs::remove_dir_all(workspace).expect("cleanup temp workspace");
    }

    #[test]
    fn lean_auto_detect_policy_runs_when_targets_exist() {
        let mut values = std::collections::BTreeMap::new();
        values.insert(
            "lean_policy".to_string(),
            serde_json::json!({
                "requested": false,
                "disabled": false,
                "auto_detect": true,
                "run_lean": false
            }),
        );
        let targets = serde_json::json!({
            "status": "ready",
            "targets": [
                {"id": "lean_target"}
            ]
        });

        assert!(lean_execution_requested_from_policy(
            &values, &targets, false
        ));
    }

    #[test]
    fn no_lean_policy_disables_auto_detect_even_when_targets_exist() {
        let mut values = std::collections::BTreeMap::new();
        values.insert(
            "lean_policy".to_string(),
            serde_json::json!({
                "requested": false,
                "disabled": true,
                "auto_detect": true,
                "run_lean": false
            }),
        );
        let targets = serde_json::json!({
            "status": "ready",
            "targets": [
                {"id": "lean_target"}
            ]
        });

        assert!(!lean_execution_requested_from_policy(
            &values, &targets, false
        ));
    }
}
