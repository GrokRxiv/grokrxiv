//! Public display helpers for paper source identifiers.

/// Return the human-facing source reference for a paper.
///
/// The database still carries `papers.arxiv_id` as the compatibility key for
/// all sources. Only true arXiv papers should be displayed with an `arXiv:`
/// prefix; local and git sources already encode their kind in `source_id`.
pub fn source_display_ref(
    source_kind: &str,
    source_id: Option<&str>,
    compatibility_id: &str,
) -> String {
    let normalized_kind = source_kind.trim().to_ascii_lowercase();
    let id = source_id
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| compatibility_id.trim());
    if normalized_kind == "arxiv" {
        format!("arXiv:{id}")
    } else {
        id.to_string()
    }
}

/// Return the stable source identifier to use in paths or branch names.
pub fn source_artifact_id(source_id: Option<&str>, compatibility_id: &str) -> String {
    source_id
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| compatibility_id.trim())
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_display_ref_only_prefixes_true_arxiv_sources() {
        assert_eq!(
            source_display_ref("arxiv", Some("2605.16051"), "2605.16051"),
            "arXiv:2605.16051"
        );
        assert_eq!(
            source_display_ref(
                "local_file",
                Some("local-pdf-d96363843fd8"),
                "local-pdf-d96363843fd8",
            ),
            "local-pdf-d96363843fd8"
        );
        assert_eq!(
            source_display_ref(
                "git_repo",
                Some("git-tex-3a2e680b410f"),
                "git-tex-3a2e680b410f",
            ),
            "git-tex-3a2e680b410f"
        );
    }

    #[test]
    fn source_artifact_id_falls_back_to_compatibility_id() {
        assert_eq!(source_artifact_id(None, "2605.16051"), "2605.16051");
        assert_eq!(
            source_artifact_id(Some(" local-tex-c5cddbce17a4 "), "ignored"),
            "local-tex-c5cddbce17a4"
        );
    }
}
