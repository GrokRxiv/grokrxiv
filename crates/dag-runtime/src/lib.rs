//! Data-driven DAG manifest loading and validation.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::path::Path;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DagTypeId(String);

impl DagTypeId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for DagTypeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RoleId(String);

impl RoleId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for RoleId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DagRoleKey(String);

impl DagRoleKey {
    pub fn new(dag_type: DagTypeId, role_id: RoleId) -> Self {
        Self(format!("{}.{}", dag_type.as_str(), role_id.as_str()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn dag_type(&self) -> &str {
        self.0.split_once('.').map(|(dag, _)| dag).unwrap_or("")
    }

    pub fn role_id(&self) -> &str {
        self.0
            .split_once('.')
            .map(|(_, role)| role)
            .unwrap_or(self.0.as_str())
    }
}

impl fmt::Display for DagRoleKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentKind {
    Extractor,
    Critic,
    TypeTheoryValidator,
    Synthesizer,
    CodeGenerator,
    Renderer,
    Verifier,
}

impl fmt::Display for AgentKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Extractor => "extractor",
            Self::Critic => "critic",
            Self::TypeTheoryValidator => "type_theory_validator",
            Self::Synthesizer => "synthesizer",
            Self::CodeGenerator => "code_generator",
            Self::Renderer => "renderer",
            Self::Verifier => "verifier",
        };
        f.write_str(s)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DagNodeKind {
    PrepareInputs,
    Agent,
    Synthesizer,
    Verify,
    Gate,
    RenderArtifacts,
    ModerationReady,
    IngestSource,
    Tool,
    Artifact,
    DagCall,
}

impl fmt::Display for DagNodeKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::PrepareInputs => "prepare_inputs",
            Self::Agent => "agent",
            Self::Synthesizer => "synthesizer",
            Self::Verify => "verify",
            Self::Gate => "gate",
            Self::RenderArtifacts => "render_artifacts",
            Self::ModerationReady => "moderation_ready",
            Self::IngestSource => "ingest_source",
            Self::Tool => "tool",
            Self::Artifact => "artifact",
            Self::DagCall => "dag_call",
        };
        f.write_str(s)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DagExecutionMode {
    OneShot,
    ToolLoop,
}

impl Default for DagExecutionMode {
    fn default() -> Self {
        Self::OneShot
    }
}

impl fmt::Display for DagExecutionMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::OneShot => "one_shot",
            Self::ToolLoop => "tool_loop",
        };
        f.write_str(s)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DagRole {
    pub id: RoleId,
    pub kind: AgentKind,
    pub config: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DagGate {
    #[serde(default)]
    pub min_usable: Option<u32>,
    #[serde(default)]
    pub sources: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DagNode {
    pub id: String,
    pub kind: DagNodeKind,
    #[serde(default)]
    pub role: Option<RoleId>,
    #[serde(default)]
    pub tool: Option<String>,
    #[serde(default)]
    pub dag_type: Option<String>,
    #[serde(default)]
    pub inputs: Vec<String>,
    #[serde(default)]
    pub outputs: Vec<String>,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub feeds_meta: bool,
    #[serde(default)]
    pub gate: Option<DagGate>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DagTool {
    pub id: String,
    pub executor: ToolExecutorKind,
    #[serde(default)]
    pub handler: Option<String>,
    #[serde(default)]
    pub command: Option<Vec<String>>,
    #[serde(default)]
    pub timeout_secs: Option<u64>,
    #[serde(default)]
    pub input_schema: Option<serde_yaml::Value>,
    #[serde(default)]
    pub output_schema: Option<serde_yaml::Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolExecutorKind {
    Rust,
    Cli,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DagEdge {
    pub from: OneOrMany,
    pub to: OneOrMany,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum OneOrMany {
    One(String),
    Many(Vec<String>),
}

impl OneOrMany {
    fn values(&self) -> Vec<String> {
        match self {
            Self::One(value) => vec![value.clone()],
            Self::Many(values) => values.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DagManifest {
    pub id: DagTypeId,
    pub version: u32,
    #[serde(default)]
    pub accepts: Vec<AgentKind>,
    #[serde(default)]
    pub concurrency: Option<u32>,
    #[serde(default)]
    pub tools: Vec<DagTool>,
    #[serde(default)]
    pub roles: Vec<DagRole>,
    #[serde(default)]
    pub nodes: Vec<DagNode>,
    #[serde(default)]
    pub edges: Vec<DagEdge>,
}

impl DagManifest {
    pub fn from_path(path: impl AsRef<Path>) -> Result<Self, DagError> {
        let text = std::fs::read_to_string(path).map_err(DagError::Io)?;
        Self::from_str(&text)
    }

    pub fn from_str(text: &str) -> Result<Self, DagError> {
        let manifest: Self = serde_yaml::from_str(text).map_err(DagError::Yaml)?;
        manifest.validate()?;
        Ok(manifest)
    }

    pub fn validate(&self) -> Result<(), DagError> {
        let mut role_ids = HashSet::new();
        for role in &self.roles {
            if !role_ids.insert(role.id.clone()) {
                return Err(DagError::DuplicateRole(role.id.to_string()));
            }
            if !self.accepts.contains(&role.kind) {
                return Err(DagError::KindNotAccepted {
                    dag: self.id.to_string(),
                    role: role.id.to_string(),
                    kind: role.kind.to_string(),
                });
            }
        }

        let mut tool_def_ids = HashSet::new();
        for tool in &self.tools {
            if !tool_def_ids.insert(tool.id.clone()) {
                return Err(DagError::DuplicateTool(tool.id.clone()));
            }
            if tool.executor == ToolExecutorKind::Cli {
                let has_command = tool
                    .command
                    .as_ref()
                    .map(|command| {
                        !command.is_empty() && command.iter().all(|part| !part.is_empty())
                    })
                    .unwrap_or(false);
                if !has_command {
                    return Err(DagError::CliToolMissingCommand(tool.id.clone()));
                }
            }
        }

        let mut node_ids = HashSet::new();
        let tool_ids: HashSet<&str> = self.tools.iter().map(|tool| tool.id.as_str()).collect();
        for node in &self.nodes {
            if !node_ids.insert(node.id.clone()) {
                return Err(DagError::DuplicateNode(node.id.clone()));
            }
            if node.kind == DagNodeKind::Tool && node.tool.is_none() {
                return Err(DagError::ToolNodeMissingTool(node.id.clone()));
            }
            if let Some(role) = &node.role {
                if !role_ids.contains(role) {
                    return Err(DagError::MissingRole {
                        node: node.id.clone(),
                        role: role.to_string(),
                    });
                }
            }
            if let Some(tool) = node.tool.as_deref() {
                if !tool_ids.contains(tool) {
                    return Err(DagError::MissingTool {
                        node: node.id.clone(),
                        tool: tool.to_string(),
                    });
                }
            }
            if node.kind == DagNodeKind::Gate {
                let Some(gate) = &node.gate else {
                    return Err(DagError::GateNodeMissingPolicy(node.id.clone()));
                };
                if let Some(min_usable) = gate.min_usable {
                    if min_usable == 0 {
                        return Err(DagError::GateNodeInvalidMinUsable(node.id.clone()));
                    }
                }
            }
        }

        for edge in &self.edges {
            for id in edge.from.values().into_iter().chain(edge.to.values()) {
                if !node_ids.contains(&id) {
                    return Err(DagError::MissingNode(id));
                }
            }
        }

        for node in &self.nodes {
            if let Some(gate) = &node.gate {
                for source in &gate.sources {
                    if !node_ids.contains(source) {
                        return Err(DagError::MissingNode(source.clone()));
                    }
                }
            }
        }

        self.execution_layers().map(|_| ())
    }

    pub fn execution_layers(&self) -> Result<Vec<Vec<String>>, DagError> {
        let node_order: HashMap<&str, usize> = self
            .nodes
            .iter()
            .enumerate()
            .map(|(idx, node)| (node.id.as_str(), idx))
            .collect();
        let mut remaining: HashMap<String, HashSet<String>> = self
            .nodes
            .iter()
            .map(|node| (node.id.clone(), HashSet::new()))
            .collect();

        for edge in &self.edges {
            let froms = edge.from.values();
            let tos = edge.to.values();
            for to in &tos {
                let deps = remaining
                    .get_mut(to)
                    .ok_or_else(|| DagError::MissingNode(to.clone()))?;
                deps.extend(froms.iter().cloned());
            }
        }

        let mut layers = Vec::new();
        while !remaining.is_empty() {
            let mut ready: Vec<String> = remaining
                .iter()
                .filter_map(|(id, deps)| deps.is_empty().then_some(id.clone()))
                .collect();
            if ready.is_empty() {
                return Err(DagError::Cycle);
            }
            ready.sort_by_key(|id| node_order.get(id.as_str()).copied().unwrap_or(usize::MAX));
            for id in &ready {
                remaining.remove(id);
            }
            for deps in remaining.values_mut() {
                for id in &ready {
                    deps.remove(id);
                }
            }
            layers.push(ready);
        }

        Ok(layers)
    }

    pub fn compatible_dag_ids(manifests: &[DagManifest], kind: AgentKind) -> Vec<String> {
        manifests
            .iter()
            .filter(|manifest| manifest.accepts.contains(&kind))
            .map(|manifest| manifest.id.to_string())
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DagRunReport {
    pub dag_type: DagTypeId,
    pub status: DagNodeStatus,
    #[serde(default)]
    pub nodes: Vec<DagNodeReport>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DagNodeReport {
    pub node_id: String,
    pub kind: String,
    pub status: DagNodeStatus,
    #[serde(default)]
    pub executor: Option<String>,
    #[serde(default)]
    pub inputs: Vec<String>,
    #[serde(default)]
    pub outputs: Vec<String>,
    #[serde(default)]
    pub warning: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub latency_ms: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DagNodeStatus {
    Pending,
    Running,
    Ok,
    Degraded,
    Failed,
    Skipped,
}

#[derive(Debug, thiserror::Error)]
pub enum DagError {
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Yaml(#[from] serde_yaml::Error),
    #[error("duplicate role id `{0}`")]
    DuplicateRole(String),
    #[error("duplicate node id `{0}`")]
    DuplicateNode(String),
    #[error("duplicate tool id `{0}`")]
    DuplicateTool(String),
    #[error("node `{node}` references missing role `{role}`")]
    MissingRole { node: String, role: String },
    #[error("missing node `{0}`")]
    MissingNode(String),
    #[error("node `{node}` references missing tool `{tool}`")]
    MissingTool { node: String, tool: String },
    #[error("tool node `{0}` must reference a registered tool")]
    ToolNodeMissingTool(String),
    #[error("CLI tool `{0}` must declare a non-empty command")]
    CliToolMissingCommand(String),
    #[error("gate node `{0}` must define a gate policy")]
    GateNodeMissingPolicy(String),
    #[error("gate node `{0}` has invalid min_usable; value must be >= 1")]
    GateNodeInvalidMinUsable(String),
    #[error("agent kind `{kind}` for role `{role}` is not accepted by DAG `{dag}`")]
    KindNotAccepted {
        dag: String,
        role: String,
        kind: String,
    },
    #[error("DAG contains a cycle")]
    Cycle,
}
