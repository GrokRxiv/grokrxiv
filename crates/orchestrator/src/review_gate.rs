use grokrxiv_schemas::{AgentRole, VerifierStatus};

/// Final automated gate verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GateVerdict {
    /// Clean pass.
    Pass,
    /// Meta-review accepted, but verifier warnings remain.
    Warn,
    /// Automated publication gate failed.
    Fail,
}

/// Specialist verifier aggregate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SpecialistGate {
    /// Whether enough specialist outputs are usable to run the meta-reviewer.
    pub(crate) meta_can_run: bool,
    /// Whether the review can publish without force.
    pub(crate) publishable_without_force: bool,
    /// Specialist roles with pass or warn verifier output.
    pub(crate) usable_roles: Vec<&'static str>,
    /// Specialist roles with warn verifier output.
    pub(crate) warning_roles: Vec<&'static str>,
    /// Specialist roles with fail or missing verifier output.
    pub(crate) blocked_roles: Vec<&'static str>,
    /// Minimum usable specialist outputs needed for meta-review.
    pub(crate) min_usable: usize,
    /// Expected total specialist outputs.
    pub(crate) expected_total: usize,
}

impl SpecialistGate {
    /// Evaluate specialist verifier statuses.
    pub(crate) fn evaluate(
        statuses: &[(AgentRole, Option<VerifierStatus>)],
        min_usable: usize,
        expected_total: usize,
    ) -> Self {
        let mut usable_roles = Vec::new();
        let mut warning_roles = Vec::new();
        let mut blocked_roles = Vec::new();
        for (role, status) in statuses {
            let slug = role_slug(*role);
            match status {
                Some(VerifierStatus::Pass) => usable_roles.push(slug),
                Some(VerifierStatus::Warn) => {
                    usable_roles.push(slug);
                    warning_roles.push(slug);
                }
                Some(VerifierStatus::Fail) | None => blocked_roles.push(slug),
            }
        }
        let meta_can_run = usable_roles.len() >= min_usable;
        let publishable_without_force = usable_roles.len() == expected_total
            && warning_roles.is_empty()
            && blocked_roles.is_empty();
        Self {
            meta_can_run,
            publishable_without_force,
            usable_roles,
            warning_roles,
            blocked_roles,
            min_usable,
            expected_total,
        }
    }

    #[cfg(test)]
    pub(crate) fn all_pass_for_test() -> Self {
        Self {
            meta_can_run: true,
            publishable_without_force: true,
            usable_roles: vec![
                "summary",
                "technical_correctness",
                "novelty",
                "reproducibility",
                "citation",
            ],
            warning_roles: vec![],
            blocked_roles: vec![],
            min_usable: 3,
            expected_total: 5,
        }
    }
}

/// Inputs to the publication gate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PublicationGateInput<'a> {
    /// Meta-review recommendation string.
    pub(crate) recommendation: Option<&'a str>,
    /// Specialist verifier aggregate.
    pub(crate) specialist_gate: SpecialistGate,
}

/// Publication gate decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PublicationGate {
    /// Pass/warn/fail result.
    pub(crate) verdict: GateVerdict,
    /// Human-readable reason.
    pub(crate) reason: String,
    /// Normalized recommendation used by the decision.
    pub(crate) recommendation: String,
}

impl PublicationGate {
    /// Evaluate whether a review is publishable without force.
    pub(crate) fn evaluate(input: PublicationGateInput<'_>) -> Self {
        let recommendation = input.recommendation.unwrap_or("missing").to_string();
        if !input.specialist_gate.meta_can_run {
            return Self {
                verdict: GateVerdict::Fail,
                reason: format!(
                    "Only {} of {} specialist outputs were usable; need at least {}.",
                    input.specialist_gate.usable_roles.len(),
                    input.specialist_gate.expected_total,
                    input.specialist_gate.min_usable,
                ),
                recommendation,
            };
        }
        if recommendation != "accept" {
            return Self {
                verdict: GateVerdict::Fail,
                reason: format!("Meta-review recommendation is `{recommendation}`, not `accept`."),
                recommendation,
            };
        }
        if !input.specialist_gate.publishable_without_force {
            return Self {
                verdict: GateVerdict::Warn,
                reason: format!(
                    "Meta-review accepted, but verifier warnings or blocked roles remain. warnings={:?}; blocked={:?}",
                    input.specialist_gate.warning_roles,
                    input.specialist_gate.blocked_roles,
                ),
                recommendation,
            };
        }
        Self {
            verdict: GateVerdict::Pass,
            reason: "Meta-review accepted and all specialist verifier statuses passed.".to_string(),
            recommendation,
        }
    }
}

fn role_slug(role: AgentRole) -> &'static str {
    crate::review_dag::role_slug(role)
}

#[cfg(test)]
mod tests {
    use super::*;
    use grokrxiv_schemas::{AgentRole, VerifierStatus};

    #[test]
    fn specialist_gate_distinguishes_usable_from_publishable() {
        let statuses = vec![
            (AgentRole::Summary, Some(VerifierStatus::Pass)),
            (AgentRole::TechnicalCorrectness, Some(VerifierStatus::Warn)),
            (AgentRole::Novelty, Some(VerifierStatus::Pass)),
            (AgentRole::Reproducibility, Some(VerifierStatus::Fail)),
            (AgentRole::Citation, None),
        ];
        let gate = SpecialistGate::evaluate(&statuses, 3, 5);
        assert!(gate.meta_can_run);
        assert!(!gate.publishable_without_force);
        assert_eq!(
            gate.usable_roles,
            vec!["summary", "technical_correctness", "novelty"]
        );
        assert_eq!(gate.warning_roles, vec!["technical_correctness"]);
        assert_eq!(gate.blocked_roles, vec!["reproducibility", "citation"]);
    }

    #[test]
    fn publication_gate_only_passes_clean_accept() {
        let clean = PublicationGateInput {
            recommendation: Some("accept"),
            specialist_gate: SpecialistGate {
                meta_can_run: true,
                publishable_without_force: true,
                usable_roles: vec![
                    "summary",
                    "technical_correctness",
                    "novelty",
                    "reproducibility",
                    "citation",
                ],
                warning_roles: vec![],
                blocked_roles: vec![],
                min_usable: 3,
                expected_total: 5,
            },
        };
        assert_eq!(PublicationGate::evaluate(clean).verdict, GateVerdict::Pass);

        let minor = PublicationGateInput {
            recommendation: Some("minor_revision"),
            specialist_gate: SpecialistGate::all_pass_for_test(),
        };
        assert_eq!(PublicationGate::evaluate(minor).verdict, GateVerdict::Fail);

        let warned = PublicationGateInput {
            recommendation: Some("accept"),
            specialist_gate: SpecialistGate {
                warning_roles: vec!["citation"],
                publishable_without_force: false,
                ..SpecialistGate::all_pass_for_test()
            },
        };
        assert_eq!(PublicationGate::evaluate(warned).verdict, GateVerdict::Warn);
    }
}
