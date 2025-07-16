use std::fmt::Display;

use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckSuiteAction {
    /// A check suite was requested (when code is pushed)
    Requested,
    /// A check suite was re-requested (when re-running on code that was previously pushed)
    Rerequested,
    /// A check suite has finished running
    Completed,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckRunAction {
    Created,
    RequestedAction,
    Rerequested,
    Completed,
}

#[derive(Debug, Deserialize, Clone, Copy)]
#[serde(transparent)]
pub struct CheckRunId(u64);

impl Display for CheckRunId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct CheckRun {
    pub id: CheckRunId,
    pub name: String,
}

#[derive(Debug, Serialize)]
pub struct CheckRunOutput<'a> {
    pub title: &'a str,
    pub summary: &'a str,
    pub annotations: &'a [Annotation],
}

#[derive(Debug, Serialize)]
pub struct Annotation {
    pub path: String,
    pub start_line: usize,
    pub end_line: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_column: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_column: Option<usize>,
    pub annotation_level: AnnotationLevel,
    pub message: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AnnotationLevel {
    Warning,
    Failure,
}
