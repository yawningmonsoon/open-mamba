pub mod schema;
pub mod store;
pub mod analytics;
pub mod triggers;
pub mod workflows;

pub use store::{Lake, SolverOutcomeRow, SolverSkipRuleRow, TaskRow};
pub use triggers::{Schedule, Webhook};
pub use workflows::{Workflow, WorkflowRun, WorkflowStep};

/// Shared timestamp parser. DuckDB renders TIMESTAMPTZ via `CAST AS VARCHAR`
/// as `2026-04-27 20:40:32.123456+00` — not strict RFC3339. This helper
/// accepts both that form and proper RFC3339 strings.
pub fn triggers_parse_ts(s: String) -> duckdb::Result<chrono::DateTime<chrono::Utc>> {
    use chrono::{DateTime, Utc};
    if let Ok(dt) = DateTime::parse_from_rfc3339(&s) {
        return Ok(dt.with_timezone(&Utc));
    }
    let normalized = s.replace(' ', "T");
    let normalized = if normalized.ends_with("+00") || normalized.ends_with("-00") {
        format!("{normalized}:00")
    } else if normalized.len() >= 3 {
        let (head, tail) = normalized.split_at(normalized.len() - 3);
        if (tail.starts_with('+') || tail.starts_with('-')) && !tail.contains(':') {
            format!("{head}{tail}:00")
        } else {
            normalized.clone()
        }
    } else {
        normalized.clone()
    };
    DateTime::parse_from_rfc3339(&normalized)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|e| {
            duckdb::Error::FromSqlConversionFailure(
                0,
                duckdb::types::Type::Any,
                Box::new(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("bad timestamp {s:?} -> {normalized:?}: {e}"),
                )),
            )
        })
}
