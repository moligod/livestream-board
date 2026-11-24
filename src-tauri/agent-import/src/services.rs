pub(crate) mod session_usage;
pub(crate) mod session_usage_agy;
pub(crate) mod session_usage_copilot;
pub(crate) mod session_usage_dsh;
pub(crate) mod session_usage_gemini;
pub(crate) mod session_usage_grokbuild;
pub(crate) mod session_usage_kimi;
pub(crate) mod session_usage_mcode;
pub(crate) mod session_usage_omp;
pub(crate) mod session_usage_pi;
pub(crate) mod session_usage_workbuddy;

pub(crate) mod sql_helpers {
    pub const INPUT_TOKEN_SEMANTICS_FRESH: &str = "fresh";
    pub const INPUT_TOKEN_SEMANTICS_TOTAL: &str = "total";
}

// There are no routing rows or CC Switch price tables in the import staging DB.
pub(crate) mod usage_stats {
    use crate::{error::AppError, proxy::usage::calculator::ModelPricing};
    use rusqlite::Connection;
    pub const SESSION_PROXY_DEDUP_WINDOW_SECONDS: i64 = 600;
    #[allow(dead_code)]
    pub struct DedupKey<'a> {
        pub data_source: &'a str,
        pub app_type: &'a str,
        pub model: &'a str,
        pub input_tokens: u32,
        pub output_tokens: u32,
        pub cache_read_tokens: u32,
        pub cache_creation_tokens: u32,
        pub created_at: i64,
    }
    pub fn find_model_pricing(_: &Connection, _: &str) -> Option<ModelPricing> {
        None
    }
    pub fn has_matching_proxy_usage_log(
        _: &Connection,
        _: &DedupKey<'_>,
    ) -> Result<bool, AppError> {
        Ok(false)
    }
    pub fn has_recent_grokbuild_proxy_activity(_: &Connection, _: i64) -> Result<bool, AppError> {
        Ok(false)
    }
}
