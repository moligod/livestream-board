CREATE TABLE proxy_request_logs (
    request_id TEXT PRIMARY KEY NOT NULL, provider_id TEXT, app_type TEXT NOT NULL,
    model TEXT NOT NULL, request_model TEXT, pricing_model TEXT,
    input_tokens INTEGER DEFAULT 0, output_tokens INTEGER DEFAULT 0,
    cache_read_tokens INTEGER DEFAULT 0, cache_creation_tokens INTEGER DEFAULT 0,
    input_cost_usd TEXT DEFAULT '0', output_cost_usd TEXT DEFAULT '0',
    cache_read_cost_usd TEXT DEFAULT '0', cache_creation_cost_usd TEXT DEFAULT '0',
    total_cost_usd TEXT DEFAULT '0', latency_ms INTEGER DEFAULT 0,
    first_token_ms INTEGER, duration_ms INTEGER, status_code INTEGER,
    error_message TEXT, session_id TEXT NOT NULL, provider_type TEXT,
    is_streaming INTEGER, cost_multiplier TEXT DEFAULT '1', created_at INTEGER NOT NULL,
    data_source TEXT, input_token_semantics TEXT
);
CREATE TABLE session_log_sync (
    file_path TEXT PRIMARY KEY, last_modified INTEGER NOT NULL,
    last_line_offset INTEGER NOT NULL, last_synced_at INTEGER NOT NULL,
    last_byte_offset INTEGER, last_tail_fingerprint INTEGER, parser_state TEXT
);
CREATE TABLE session_usage_dedup (
    data_source TEXT, request_id TEXT, semantic_id TEXT, has_entry_id INTEGER,
    PRIMARY KEY (data_source, request_id)
);
CREATE INDEX session_usage_dedup_semantic ON session_usage_dedup (data_source, semantic_id);
