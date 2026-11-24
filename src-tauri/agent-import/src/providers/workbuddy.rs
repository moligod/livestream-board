// Adapted from CC Switch v4.0.40 (MIT; Copyright (c) 2025 Jason Young).
use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
};
pub(crate) fn projects_dir() -> PathBuf {
    crate::workbuddy_config::data_dir().join("projects")
}
/// 每个项目目录下的顶层 `*.jsonl`（子目录里的子代理记录不是独立会话）。
pub(crate) fn session_files(projects: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let Ok(projects) = fs::read_dir(projects) else {
        return files;
    };
    for project in projects.flatten() {
        let Ok(entries) = fs::read_dir(project.path()) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() && path.extension().and_then(|ext| ext.to_str()) == Some("jsonl") {
                files.push(path);
            }
        }
    }
    files
}

/// 导入用量要读的文件：顶层会话文件，加上 `<sessionId>/subagents/*.jsonl`。
/// 子代理不是独立会话，但它的请求算在父会话头上；返回 `(文件, 父会话 ID)`，
/// 顶层文件的父会话 ID 为 `None`（用记录里的 `sessionId`）。
pub(crate) fn usage_files(projects: &Path) -> Vec<(PathBuf, Option<String>)> {
    let mut files: Vec<_> = session_files(projects)
        .into_iter()
        .map(|path| (path, None))
        .collect();
    let Ok(projects) = fs::read_dir(projects) else {
        return files;
    };
    for project in projects.flatten() {
        let Ok(sessions) = fs::read_dir(project.path()) else {
            continue;
        };
        for session in sessions.flatten() {
            let session_dir = session.path();
            let Some(parent_id) = session_dir
                .file_name()
                .and_then(|name| name.to_str())
                .map(str::to_string)
            else {
                continue;
            };
            let Ok(entries) = fs::read_dir(session_dir.join("subagents")) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_file() && path.extension().and_then(|ext| ext.to_str()) == Some("jsonl")
                {
                    files.push((path, Some(parent_id.clone())));
                }
            }
        }
    }
    files
}
fn kind(record: &Value) -> &str {
    record.get("type").and_then(Value::as_str).unwrap_or("")
}

fn role(record: &Value) -> &str {
    record.get("role").and_then(Value::as_str).unwrap_or("")
}

/// `content` 数组里的文本（`input_text` / `output_text` / `text`）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Usage {
    pub model: Option<String>,
    pub fresh_input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub reasoning: u64,
}

impl Usage {
    pub(crate) fn is_empty(&self) -> bool {
        self.fresh_input == 0 && self.output == 0 && self.cache_read == 0 && self.cache_write == 0
    }
}

fn count(value: Option<&Value>, key: &str) -> u64 {
    value
        .and_then(|value| value.get(key))
        .and_then(Value::as_u64)
        .unwrap_or(0)
}

/// 自定义模型在 `requestModelId` 里记成 `custom-local:<id>`；去掉前缀，
/// 让用量里的模型和价格表、用户配置的模型 ID 对得上。
fn strip_custom_prefix(model: &str) -> &str {
    match model.split_once(':') {
        Some((prefix, id)) if prefix.starts_with("custom-") && !id.trim().is_empty() => id.trim(),
        _ => model,
    }
}

/// 从模型产出的事件里取用量；没有任何用量字段时返回 `None`。
///
/// 一次请求的 `rawUsage` 记在这次请求的最后一个事件上：只回文本时是 assistant
/// `message`，调用工具时是最后一个 `function_call`（实际日志里大部分 token 在这里），
/// 所以只要带 `rawUsage` 就算，不看事件类型。
pub(crate) fn assistant_usage(record: &Value) -> Option<Usage> {
    if role(record) == "user" {
        return None;
    }
    let data = record.get("providerData");
    let model = data
        .and_then(|data| data.get("model").or_else(|| data.get("requestModelId")))
        .and_then(Value::as_str)
        .map(|model| strip_custom_prefix(model.trim()))
        .filter(|model| !model.is_empty())
        .map(str::to_string);
    let raw = data.and_then(|data| data.get("rawUsage"));
    if raw.is_some_and(Value::is_object) {
        let details = raw.and_then(|raw| raw.get("prompt_tokens_details"));
        let cache_read = [
            count(raw, "prompt_cache_hit_tokens"),
            count(raw, "cache_read_input_tokens"),
            count(raw, "cached_tokens"),
            count(details, "cached_tokens"),
        ]
        .into_iter()
        .max()
        .unwrap_or(0);
        let cache_write =
            count(raw, "cache_creation_input_tokens").max(count(raw, "prompt_cache_write_tokens"));
        let reasoning = count(raw, "completion_thinking_tokens").max(count(
            raw.and_then(|raw| raw.get("completion_tokens_details")),
            "reasoning_tokens",
        ));
        return Some(Usage {
            model,
            fresh_input: count(raw, "prompt_tokens")
                .saturating_sub(cache_read)
                .saturating_sub(cache_write),
            output: count(raw, "completion_tokens"),
            cache_read,
            cache_write,
            reasoning,
        });
    }
    // 没有 rawUsage 时退回 agents SDK 的 usage（同样是含缓存的输入），只认 assistant 消息。
    if kind(record) != "message" || role(record) != "assistant" {
        return None;
    }
    let usage = data.and_then(|data| data.get("usage"))?;
    let cache_read = usage
        .get("inputTokensDetails")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|detail| count(Some(detail), "cached_tokens"))
        .sum::<u64>();
    let reasoning = usage
        .get("outputTokensDetails")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|detail| count(Some(detail), "reasoning_tokens"))
        .sum::<u64>();
    Some(Usage {
        model,
        fresh_input: count(Some(usage), "inputTokens").saturating_sub(cache_read),
        output: count(Some(usage), "outputTokens"),
        cache_read,
        cache_write: 0,
        reasoning,
    })
}
