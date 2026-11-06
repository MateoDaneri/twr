//! The existing overview's JSON-lines and read requests. This module never starts a server.
use crate::agents::Agent;
use serde_json::{Value, json};
use std::collections::BTreeMap;

#[derive(Default)]
pub struct JsonLines {
    pending: Vec<u8>,
}
impl JsonLines {
    /// Handles split UTF-8 and multiple records per read, bounded per record.
    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<Value>, String> {
        if bytes.len() > 4 * 1024 * 1024 {
            self.pending.clear();
            return Err("app-server read exceeds 4 MiB".into());
        }
        let mut records = Vec::new();
        for part in bytes.split_inclusive(|byte| *byte == b'\n') {
            if self.pending.len() + part.len() > 1024 * 1024 {
                self.pending.clear();
                return Err("app-server JSON line exceeds 1 MiB".into());
            }
            self.pending.extend_from_slice(part);
            if part.last() == Some(&b'\n') {
                if let Ok(value) = serde_json::from_slice::<Value>(&self.pending)
                    && value.is_object()
                {
                    records.push(value);
                    if records.len() > 4096 {
                        self.pending.clear();
                        return Err("too many app-server records in one read".into());
                    }
                }
                self.pending.clear();
            }
        }
        Ok(records)
    }
}

pub fn initialize(id: u64) -> Value {
    json!({"id":id,"method":"initialize","params":{"clientInfo":{"name":"terminal_workspace","title":"Terminal Workspace","version":env!("CARGO_PKG_VERSION")},"capabilities":{"experimentalApi":true}}})
}
pub fn initialized() -> Value {
    json!({"method":"initialized","params":{}})
}
pub fn thread_list(id: u64) -> Value {
    json!({"id":id,"method":"thread/list","params":{"cursor":null,"limit":50,"sortKey":"recency_at","sortDirection":"desc","archived":false,"useStateDbOnly":true,"sourceKinds":["cli","vscode","exec","appServer","subAgentThreadSpawn","unknown"]}})
}
pub fn project_list(id: u64) -> Value {
    json!({"id":id,"method":"project/list","params":{"cursor":null,"limit":100}})
}
pub fn latest_turn(id: u64, thread: &str) -> Value {
    json!({"id":id,"method":"thread/turns/list","params":{"threadId":thread,"limit":1,"sortDirection":"desc","itemsView":"notLoaded"}})
}

fn text(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_owned)
}
fn source_kind(thread: &Value) -> Option<String> {
    let source = &thread["source"];
    if let Some(kind) = source.as_str() {
        return Some(kind.into());
    }
    let sub = source.get("subAgent").or_else(|| source.get("subagent"));
    match sub {
        Some(Value::String(kind)) => Some(
            match kind.as_str() {
                "review" => "subAgentReview",
                "compact" => "subAgentCompact",
                _ => "subAgentOther",
            }
            .into(),
        ),
        Some(Value::Object(fields)) if fields.contains_key("thread_spawn") => {
            Some("subAgentThreadSpawn".into())
        }
        Some(Value::Object(fields)) if fields.contains_key("other") => Some("subAgentOther".into()),
        _ => text(thread, "sourceKind"),
    }
}
pub fn visible_thread(thread: &Value) -> bool {
    thread["ephemeral"] != true
        && !matches!(
            thread["threadSource"].as_str(),
            Some("chatgpt_hidden" | "ambient_suggestions")
        )
        && !matches!(
            source_kind(thread).as_deref(),
            Some("subAgentReview" | "subAgentCompact" | "subAgentOther")
        )
}

pub fn parent_id(thread: &Value) -> Option<String> {
    text(thread, "parentThreadId")
        .filter(|v| !v.is_empty())
        .or_else(|| {
            let source = &thread["source"];
            let sub = source.get("subAgent").or_else(|| source.get("subagent"))?;
            text(&sub["thread_spawn"], "parent_thread_id").filter(|v| !v.is_empty())
        })
}

/// First normalization layer only. Rollout, remote and work-context enrichment remain separate adapters.
pub fn normalize_thread(
    thread: &Value,
    override_status: Option<&Value>,
    host: Option<&str>,
) -> Option<Agent> {
    if !visible_thread(thread) {
        return None;
    }
    let id = thread["id"].as_str()?;
    if id.is_empty() || id.len() > 256 || id.chars().any(char::is_control) {
        return None;
    }
    let status = override_status.unwrap_or(&thread["status"]);
    let status_name = status
        .as_str()
        .or_else(|| status["type"].as_str())
        .unwrap_or("unknown");
    let status = match status_name {
        "active" => "running",
        "idle" => "idle",
        "systemError" => "error",
        _ => "stored",
    };
    let mut flags: Vec<_> = override_status.unwrap_or(&thread["status"])["activeFlags"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect();
    flags.sort();
    let prefix = host
        .map(|host| format!("codex-host:{host}:thread:"))
        .unwrap_or_else(|| "codex-thread:".into());
    Some(Agent {
        id: format!("{prefix}{id}"),
        source: "codex_app_server".into(),
        label: text(thread, "name")
            .or_else(|| text(thread, "preview"))
            .unwrap_or_else(|| "Untitled Codex task".into()),
        status: status.into(),
        status_detail: (!flags.is_empty()).then(|| flags.join(", ")),
        task_id: Some(id.into()),
        host_id: host.map(str::to_owned),
        parent_id: parent_id(thread).map(|id| format!("{prefix}{id}")),
        updated_at: thread["updatedAt"]
            .as_i64()
            .or_else(|| thread["recencyAt"].as_i64()),
        app_bundle_id: Some("com.openai.codex".into()),
        ..Default::default()
    })
}

#[derive(Default)]
pub struct StatusOverrides(BTreeMap<String, Value>);
impl StatusOverrides {
    pub fn observe(&mut self, message: &Value) -> bool {
        if message["method"] != "thread/status/changed" {
            return false;
        }
        let params = &message["params"];
        let Some(id) = params["threadId"]
            .as_str()
            .filter(|id| !id.is_empty() && id.len() <= 256)
        else {
            return false;
        };
        let Some(status) = params.get("status") else {
            return false;
        };
        if !status.is_object() && !status.is_string() {
            return false;
        }
        if self.0.len() >= 2048 && !self.0.contains_key(id) {
            return false;
        }
        self.0.insert(id.into(), status.clone());
        true
    }
    pub fn get(&self, id: &str) -> Option<&Value> {
        self.0.get(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fragmented_utf8_and_concatenated_records_are_decoded_without_loss() {
        let bytes = "{\"method\":\"x\",\"params\":\"ñ\"}\n{\"id\":2,\"result\":{}}\n".as_bytes();
        let mut decoder = JsonLines::default();
        let mut output = Vec::new();
        for byte in bytes {
            output.extend(decoder.push(&[*byte]).unwrap());
        }
        assert_eq!(output.len(), 2);
        assert_eq!(output[0]["params"], "ñ");
    }
    #[test]
    fn hidden_review_threads_are_filtered_and_real_spawn_parent_is_preserved() {
        let review = json!({"id":"internal","source":{"subAgent":"review"}});
        assert!(!visible_thread(&review));
        let child = json!({"id":"child","source":{"subAgent":{"thread_spawn":{"parent_thread_id":"parent"}}},"status":{"type":"active","activeFlags":["waitingOnUserInput"]}});
        let agent = normalize_thread(&child, None, None).unwrap();
        assert_eq!(agent.parent_id.as_deref(), Some("codex-thread:parent"));
        assert_eq!(agent.status, "running");
        assert_eq!(agent.status_detail.as_deref(), Some("waitingOnUserInput"));
    }
    #[test]
    fn status_notifications_use_original_wire_names_and_keep_waiting_distinct() {
        let mut overrides = StatusOverrides::default();
        let message = json!({"method":"thread/status/changed","params":{"threadId":"demo","status":{"type":"active","activeFlags":["waitingOnApproval"]}}});
        assert!(overrides.observe(&message));
        let agent = normalize_thread(
            &json!({"id":"demo","status":"idle"}),
            overrides.get("demo"),
            None,
        )
        .unwrap();
        assert_eq!(
            agent.status_kind(1000),
            crate::agents::StatusKind::Attention
        );
        assert_eq!(thread_list(2)["params"]["limit"], 50);
        assert_eq!(latest_turn(3, "demo")["params"]["itemsView"], "notLoaded");
    }
}
