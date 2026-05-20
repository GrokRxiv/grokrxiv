use std::collections::{HashMap, HashSet};

use grokrxiv_schemas::AgentRole;

pub(crate) const DEFAULT_MIN_SPECIALIST_QUORUM: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum ReviewNodeId {
    PrepareReview,
    Specialist(AgentRole),
    VerifySpecialist(AgentRole),
    SpecialistQuorum,
    MetaReviewer,
    VerifyMetaReviewer,
    RenderArtifacts,
    ModerationReady,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ReviewNodeKind {
    PrepareReview,
    Specialist(AgentRole),
    VerifySpecialist(AgentRole),
    QuorumGate { min_usable: usize },
    MetaReviewer,
    VerifyMetaReviewer,
    RenderArtifacts,
    ModerationReady,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReviewNode {
    pub(crate) id: ReviewNodeId,
    pub(crate) kind: ReviewNodeKind,
    pub(crate) depends_on: Vec<ReviewNodeId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReviewDag {
    nodes: Vec<ReviewNode>,
}

impl ReviewDag {
    pub(crate) fn canonical() -> Self {
        let specialists = canonical_specialist_roles();
        let mut nodes = Vec::with_capacity(16);
        nodes.push(ReviewNode {
            id: ReviewNodeId::PrepareReview,
            kind: ReviewNodeKind::PrepareReview,
            depends_on: Vec::new(),
        });
        for role in specialists {
            nodes.push(ReviewNode {
                id: ReviewNodeId::Specialist(role),
                kind: ReviewNodeKind::Specialist(role),
                depends_on: vec![ReviewNodeId::PrepareReview],
            });
            nodes.push(ReviewNode {
                id: ReviewNodeId::VerifySpecialist(role),
                kind: ReviewNodeKind::VerifySpecialist(role),
                depends_on: vec![ReviewNodeId::Specialist(role)],
            });
        }
        nodes.push(ReviewNode {
            id: ReviewNodeId::SpecialistQuorum,
            kind: ReviewNodeKind::QuorumGate {
                min_usable: DEFAULT_MIN_SPECIALIST_QUORUM,
            },
            depends_on: specialists
                .iter()
                .copied()
                .map(ReviewNodeId::VerifySpecialist)
                .collect(),
        });
        nodes.push(ReviewNode {
            id: ReviewNodeId::MetaReviewer,
            kind: ReviewNodeKind::MetaReviewer,
            depends_on: vec![ReviewNodeId::SpecialistQuorum],
        });
        nodes.push(ReviewNode {
            id: ReviewNodeId::VerifyMetaReviewer,
            kind: ReviewNodeKind::VerifyMetaReviewer,
            depends_on: vec![ReviewNodeId::MetaReviewer],
        });
        nodes.push(ReviewNode {
            id: ReviewNodeId::RenderArtifacts,
            kind: ReviewNodeKind::RenderArtifacts,
            depends_on: vec![ReviewNodeId::VerifyMetaReviewer],
        });
        nodes.push(ReviewNode {
            id: ReviewNodeId::ModerationReady,
            kind: ReviewNodeKind::ModerationReady,
            depends_on: vec![ReviewNodeId::RenderArtifacts],
        });
        Self { nodes }
    }

    pub(crate) fn nodes(&self) -> &[ReviewNode] {
        &self.nodes
    }

    pub(crate) fn specialist_roles(&self) -> Vec<AgentRole> {
        self.nodes
            .iter()
            .filter_map(|node| match node.kind {
                ReviewNodeKind::Specialist(role) => Some(role),
                _ => None,
            })
            .collect()
    }

    pub(crate) fn specialist_count(&self) -> usize {
        self.specialist_roles().len()
    }

    pub(crate) fn min_specialist_quorum(&self) -> usize {
        self.nodes
            .iter()
            .find_map(|node| match node.kind {
                ReviewNodeKind::QuorumGate { min_usable } => Some(min_usable),
                _ => None,
            })
            .unwrap_or(DEFAULT_MIN_SPECIALIST_QUORUM)
    }

    pub(crate) fn validate(&self) -> Result<(), String> {
        let mut ids = HashSet::new();
        for node in &self.nodes {
            if !ids.insert(node.id) {
                return Err(format!("duplicate review DAG node: {:?}", node.id));
            }
        }
        for node in &self.nodes {
            for dep in &node.depends_on {
                if !ids.contains(dep) {
                    return Err(format!(
                        "review DAG node {:?} depends on missing node {:?}",
                        node.id, dep
                    ));
                }
            }
        }
        if self.has_cycle() {
            return Err("review DAG contains a cycle".to_string());
        }
        Ok(())
    }

    pub(crate) fn execution_layers(&self) -> Result<Vec<Vec<ReviewNodeId>>, String> {
        self.validate()?;
        let mut remaining: HashMap<ReviewNodeId, HashSet<ReviewNodeId>> = self
            .nodes
            .iter()
            .map(|node| (node.id, node.depends_on.iter().copied().collect()))
            .collect();
        let mut layers = Vec::new();
        while !remaining.is_empty() {
            let mut ready: Vec<ReviewNodeId> = remaining
                .iter()
                .filter_map(|(id, deps)| deps.is_empty().then_some(*id))
                .collect();
            if ready.is_empty() {
                return Err("review DAG contains a cycle".to_string());
            }
            ready.sort_by_key(|id| node_sort_key(*id));
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

    fn has_cycle(&self) -> bool {
        self.execution_layers_without_validation().is_err()
    }

    fn execution_layers_without_validation(&self) -> Result<Vec<Vec<ReviewNodeId>>, ()> {
        let mut remaining: HashMap<ReviewNodeId, HashSet<ReviewNodeId>> = self
            .nodes
            .iter()
            .map(|node| (node.id, node.depends_on.iter().copied().collect()))
            .collect();
        let known: HashSet<ReviewNodeId> = remaining.keys().copied().collect();
        for deps in remaining.values() {
            if deps.iter().any(|dep| !known.contains(dep)) {
                return Err(());
            }
        }
        let mut layers = Vec::new();
        while !remaining.is_empty() {
            let ready: Vec<ReviewNodeId> = remaining
                .iter()
                .filter_map(|(id, deps)| deps.is_empty().then_some(*id))
                .collect();
            if ready.is_empty() {
                return Err(());
            }
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
}

pub(crate) fn canonical_specialist_roles() -> [AgentRole; 5] {
    [
        AgentRole::Summary,
        AgentRole::TechnicalCorrectness,
        AgentRole::Novelty,
        AgentRole::Reproducibility,
        AgentRole::Citation,
    ]
}

pub(crate) fn role_slug(role: AgentRole) -> &'static str {
    match role {
        AgentRole::Summary => "summary",
        AgentRole::TechnicalCorrectness => "technical_correctness",
        AgentRole::Novelty => "novelty",
        AgentRole::Reproducibility => "reproducibility",
        AgentRole::Citation => "citation",
        AgentRole::MetaReviewer => "meta_reviewer",
    }
}

fn node_sort_key(id: ReviewNodeId) -> (u8, u8) {
    match id {
        ReviewNodeId::PrepareReview => (0, 0),
        ReviewNodeId::Specialist(role) => (1, role_sort_key(role)),
        ReviewNodeId::VerifySpecialist(role) => (2, role_sort_key(role)),
        ReviewNodeId::SpecialistQuorum => (3, 0),
        ReviewNodeId::MetaReviewer => (4, 0),
        ReviewNodeId::VerifyMetaReviewer => (5, 0),
        ReviewNodeId::RenderArtifacts => (6, 0),
        ReviewNodeId::ModerationReady => (7, 0),
    }
}

fn role_sort_key(role: AgentRole) -> u8 {
    match role {
        AgentRole::Summary => 0,
        AgentRole::TechnicalCorrectness => 1,
        AgentRole::Novelty => 2,
        AgentRole::Reproducibility => 3,
        AgentRole::Citation => 4,
        AgentRole::MetaReviewer => 5,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_review_dag_validates() {
        ReviewDag::canonical().validate().expect("canonical DAG");
    }

    #[test]
    fn canonical_review_dag_has_five_specialists_and_quorum_three() {
        let dag = ReviewDag::canonical();
        assert_eq!(dag.specialist_roles(), canonical_specialist_roles());
        assert_eq!(dag.specialist_count(), 5);
        assert_eq!(dag.min_specialist_quorum(), 3);
    }

    #[test]
    fn canonical_review_dag_fans_out_specialists_after_prepare() {
        let layers = ReviewDag::canonical()
            .execution_layers()
            .expect("valid layers");
        assert_eq!(layers[0], vec![ReviewNodeId::PrepareReview]);
        assert_eq!(
            layers[1],
            canonical_specialist_roles()
                .iter()
                .copied()
                .map(ReviewNodeId::Specialist)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn canonical_review_dag_runs_meta_after_quorum_and_render_after_meta_verification() {
        let layers = ReviewDag::canonical()
            .execution_layers()
            .expect("valid layers");
        assert_eq!(layers[3], vec![ReviewNodeId::SpecialistQuorum]);
        assert_eq!(layers[4], vec![ReviewNodeId::MetaReviewer]);
        assert_eq!(layers[5], vec![ReviewNodeId::VerifyMetaReviewer]);
        assert_eq!(layers[6], vec![ReviewNodeId::RenderArtifacts]);
        assert_eq!(layers[7], vec![ReviewNodeId::ModerationReady]);
    }

    #[test]
    fn validation_rejects_missing_dependencies() {
        let dag = ReviewDag {
            nodes: vec![ReviewNode {
                id: ReviewNodeId::MetaReviewer,
                kind: ReviewNodeKind::MetaReviewer,
                depends_on: vec![ReviewNodeId::PrepareReview],
            }],
        };
        let err = dag.validate().expect_err("missing dependency");
        assert!(err.contains("missing node"));
    }

    #[test]
    fn validation_rejects_cycles() {
        let dag = ReviewDag {
            nodes: vec![
                ReviewNode {
                    id: ReviewNodeId::PrepareReview,
                    kind: ReviewNodeKind::PrepareReview,
                    depends_on: vec![ReviewNodeId::MetaReviewer],
                },
                ReviewNode {
                    id: ReviewNodeId::MetaReviewer,
                    kind: ReviewNodeKind::MetaReviewer,
                    depends_on: vec![ReviewNodeId::PrepareReview],
                },
            ],
        };
        let err = dag.validate().expect_err("cycle");
        assert!(err.contains("cycle"));
    }
}
