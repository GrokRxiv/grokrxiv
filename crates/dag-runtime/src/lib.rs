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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DagRole {
    pub id: RoleId,
    pub kind: AgentKind,
    pub config: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DagNode {
    pub id: String,
    pub kind: String,
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
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DagTool {
    pub id: String,
    pub executor: ToolExecutorKind,
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

        let mut node_ids = HashSet::new();
        let tool_ids: HashSet<&str> = self.tools.iter().map(|tool| tool.id.as_str()).collect();
        for node in &self.nodes {
            if !node_ids.insert(node.id.clone()) {
                return Err(DagError::DuplicateNode(node.id.clone()));
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
        }

        for edge in &self.edges {
            for id in edge.from.values().into_iter().chain(edge.to.values()) {
                if !node_ids.contains(&id) {
                    return Err(DagError::MissingNode(id));
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
    #[error("node `{node}` references missing role `{role}`")]
    MissingRole { node: String, role: String },
    #[error("missing node `{0}`")]
    MissingNode(String),
    #[error("node `{node}` references missing tool `{tool}`")]
    MissingTool { node: String, tool: String },
    #[error("agent kind `{kind}` for role `{role}` is not accepted by DAG `{dag}`")]
    KindNotAccepted {
        dag: String,
        role: String,
        kind: String,
    },
    #[error("DAG contains a cycle")]
    Cycle,
}
