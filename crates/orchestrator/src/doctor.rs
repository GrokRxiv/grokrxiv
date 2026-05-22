//! Platform health checks.

use serde::Serialize;

/// Result for one platform doctor check.
#[derive(Debug, Serialize)]
pub struct DoctorCheck {
    /// Stable check id.
    pub id: &'static str,
    /// Whether the check passed.
    pub ok: bool,
    /// Human-readable status detail.
    pub detail: String,
}

/// Run generic AgentHero checks.
pub async fn doctor(json: bool) -> anyhow::Result<()> {
    let checks = vec![
        DoctorCheck {
            id: "apps_root",
            ok: crate::dag_apps::apps_root().is_dir(),
            detail: crate::dag_apps::apps_root().display().to_string(),
        },
        DoctorCheck {
            id: "database_url",
            ok: std::env::var("DATABASE_URL").is_ok(),
            detail: if std::env::var("DATABASE_URL").is_ok() {
                "set".to_string()
            } else {
                "unset".to_string()
            },
        },
    ];
    if json {
        println!("{}", serde_json::to_string_pretty(&serde_json::json!({ "checks": checks }))?);
    } else {
        for check in checks {
            println!(
                "{} {} {}",
                if check.ok { "ok" } else { "warn" },
                check.id,
                check.detail
            );
        }
    }
    Ok(())
}
