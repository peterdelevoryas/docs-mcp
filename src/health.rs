//! `GET /health`: an unauthenticated summary for the external monitor. 200
//! when the database answers and holds at least one document, 503 otherwise.
//! It reveals no document content.

use axum::{Json, extract::State, http::StatusCode};
use serde::Serialize;

use crate::store::Store;

#[derive(Serialize, Debug)]
pub struct Report {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub documents: Option<u64>,
    pub agent: bool,
    pub problems: Vec<String>,
}

#[derive(Clone)]
pub struct Health {
    pub store: Store,
    pub agent: bool,
}

pub async fn handler(State(h): State<Health>) -> (StatusCode, Json<Report>) {
    let mut problems = Vec::new();
    let documents = match h.store.count_documents().await {
        Ok(0) => {
            problems.push("no documents ingested".to_string());
            Some(0)
        }
        Ok(n) => Some(n),
        Err(e) => {
            problems.push(format!("database: {e:#}"));
            None
        }
    };
    let report = Report {
        ok: problems.is_empty(),
        documents,
        agent: h.agent,
        problems,
    };
    let status = if report.ok {
        StatusCode::OK
    } else {
        tracing::warn!(problems = ?report.problems, "health check failing");
        StatusCode::SERVICE_UNAVAILABLE
    };
    (status, Json(report))
}
