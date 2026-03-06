use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct WarningSummary {
    pub walk_errors: u64,
    pub metadata_errors: u64,
    pub sample_hash_errors: u64,
    pub full_hash_errors: u64,
    pub link_errors: u64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct LinkReportEntry {
    pub source: String,
    pub target: String,
    pub status: String,
    pub method: Option<String>,
    pub fallback_reason: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RunReport {
    pub mode: String,
    pub dry_run: bool,
    pub strict: bool,
    pub files_considered: usize,
    pub planned_actions: usize,
    pub applied_actions: usize,
    pub total_saved_bytes: u64,
    pub skipped_by_min_age: u64,
    pub warnings: WarningSummary,
    pub entries: Vec<LinkReportEntry>,
}

#[cfg(test)]
impl RunReport {
    pub fn warning_total(&self) -> u64 {
        self.warnings.walk_errors
            + self.warnings.metadata_errors
            + self.warnings.sample_hash_errors
            + self.warnings.full_hash_errors
            + self.warnings.link_errors
    }
}
