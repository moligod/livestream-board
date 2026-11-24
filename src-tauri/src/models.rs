use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UsageRecord {
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub record_id: Option<String>,
    pub session_id: String,
    pub timestamp: i64,
    pub project: String,
    pub model: String,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub reasoning_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_write_tokens: i64,
    pub total_tokens: i64,
    pub cost: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DashboardData {
    pub records: Vec<UsageRecord>,
    pub scanned_at: i64,
    pub source_status: SourceStatus,
    pub source_paths: SourcePaths,
    pub source_errors: SourceErrors,
}

impl DashboardData {
    pub fn add_agent_sources(&mut self, wanted: impl Fn(&str) -> bool) {
        for source in tokenusage_agent_import::SOURCES {
            if !wanted(source) { continue; }
            let path = tokenusage_agent_import::display_path(source);
            self.source_paths.agents.insert(source.into(), path);
            let (status, warning) = match tokenusage_agent_import::collect(source) {
                Ok(result) => {
                    self.records.extend(result.records.into_iter().map(|r| UsageRecord {
                        source: r.source, record_id: Some(r.record_id), session_id: r.session_id,
                        timestamp: r.timestamp, project: r.project, model: r.model,
                        input_tokens: r.input_tokens, output_tokens: r.output_tokens,
                        reasoning_tokens: r.reasoning_tokens, cache_read_tokens: r.cache_read_tokens,
                        cache_write_tokens: r.cache_write_tokens, total_tokens: r.total_tokens,
                        cost: r.cost,
                    }));
                    (result.status, result.warning)
                }
                Err(error) => ("error", Some(error)),
            };
            self.source_status.agents.insert(source.into(), status.into());
            self.source_errors.agents.insert(source.into(), warning);
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceStatus {
    #[serde(flatten)]
    pub agents: std::collections::BTreeMap<String, String>,
    pub codex: String,
    #[serde(default)]
    pub claude: String,
    #[serde(rename = "trae-cn")]
    pub trae_cn: String,
    pub trae: String,
    pub opencode: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourcePaths {
    #[serde(flatten)]
    pub agents: std::collections::BTreeMap<String, String>,
    pub codex: String,
    #[serde(default)]
    pub claude: String,
    #[serde(rename = "trae-cn")]
    pub trae_cn: String,
    pub trae: String,
    pub opencode: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceErrors {
    #[serde(flatten)]
    pub agents: std::collections::BTreeMap<String, Option<String>>,
    pub codex: Option<String>,
    #[serde(default)]
    pub claude: Option<String>,
    #[serde(rename = "trae-cn")]
    pub trae_cn: Option<String>,
    pub trae: Option<String>,
    pub opencode: Option<String>,
}
