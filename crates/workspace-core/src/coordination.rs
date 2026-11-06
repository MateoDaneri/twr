//! Native collaboration metadata only. Never parses tool arguments, output or rollout text.
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Default, PartialEq, Eq)]
struct Thread {
    turn: Option<String>,
    finished: bool,
    created: BTreeSet<String>,
    waits: BTreeMap<String, BTreeSet<String>>,
}
impl Thread {
    fn footprint(&self) -> (usize, usize) {
        let ids = self
            .created
            .iter()
            .chain(self.waits.keys())
            .chain(self.waits.values().flatten());
        ids.fold(
            (0, self.turn.as_ref().map_or(0, String::len)),
            |(n, b), id| (n + 1, b + id.len()),
        )
    }
}
#[derive(Default)]
pub struct Tracker(BTreeMap<String, Thread>);
#[derive(Default, Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub created: usize,
    pub waiting: bool,
    pub targets: usize,
}
fn id(value: &Value) -> Option<&str> {
    value
        .as_str()
        .filter(|s| !s.is_empty() && s.len() <= 256 && !s.chars().any(char::is_control))
}
impl Tracker {
    pub fn observe(&mut self, message: &Value) -> Result<bool, String> {
        if message.get("id").is_some() {
            return Ok(false);
        }
        let Some(method) = message["method"].as_str() else {
            return Ok(false);
        };
        if !matches!(
            method,
            "item/started"
                | "item/completed"
                | "turn/started"
                | "turn/completed"
                | "thread/status/changed"
        ) {
            return Ok(false);
        }
        let params = &message["params"];
        let Some(thread) = id(&params["threadId"]) else {
            return Ok(false);
        };
        let previous = self.0.get(thread).cloned().unwrap_or_default();
        let mut next = previous.clone();
        if method == "thread/status/changed" {
            let status = params["status"]
                .as_str()
                .or_else(|| params["status"]["type"].as_str());
            if matches!(status, Some("idle" | "notLoaded" | "systemError")) {
                next.waits.clear();
                next.finished = true;
            } else {
                return Ok(false);
            }
        } else if method.starts_with("turn/") {
            let Some(turn) = id(&params["turn"]["id"]) else {
                return Ok(false);
            };
            if method == "turn/started" {
                if next.turn.as_deref() != Some(turn) {
                    next.turn = Some(turn.into());
                    next.waits.clear();
                    next.finished = false;
                }
            } else if next.turn.as_deref().is_none_or(|active| active == turn) {
                next.turn = Some(turn.into());
                next.waits.clear();
                next.finished = true;
            }
        } else {
            let item = &params["item"];
            if item["type"] != "collabAgentToolCall" {
                return Ok(false);
            }
            let Some(call) = id(&item["id"]) else {
                return Ok(false);
            };
            let Some(turn) = id(&params["turnId"]) else {
                return Ok(false);
            };
            if next.finished || next.turn.as_deref().is_some_and(|active| active != turn) {
                return Ok(false);
            }
            next.turn = Some(turn.into());
            let targets = || {
                item["receiverThreadIds"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(id)
                    .take(128)
                    .map(str::to_owned)
                    .collect::<BTreeSet<_>>()
            };
            if item["tool"] == "wait" {
                next.waits.remove(call);
                if item["status"] == "inProgress" {
                    next.waits.insert(call.into(), targets());
                }
            } else if item["tool"] == "spawnAgent" && item["status"] == "completed" {
                next.created.extend(targets());
            } else {
                return Ok(false);
            }
        }
        if next == previous {
            return Ok(false);
        }
        let (old_tokens, old_bytes) = previous.footprint();
        let (new_tokens, new_bytes) = next.footprint();
        let total = self
            .0
            .values()
            .map(Thread::footprint)
            .fold((0, 0), |(n, b), (nn, bb)| (n + nn, b + bb));
        if (self.0.len() >= 2048 && !self.0.contains_key(thread))
            || next.waits.len() > 64
            || next.created.len() > 128
            || total.0 - old_tokens + new_tokens > 8192
            || total.1 - old_bytes + new_bytes > 2 * 1024 * 1024
        {
            return Err(
                "Coordinación fuera de límites; se conserva el último estado observado".into(),
            );
        }
        self.0.insert(thread.into(), next);
        Ok(true)
    }
    pub fn snapshot(&self, thread: &str) -> Snapshot {
        let Some(state) = self.0.get(thread) else {
            return Snapshot::default();
        };
        Snapshot {
            created: state.created.len(),
            waiting: !state.waits.is_empty(),
            targets: state
                .waits
                .values()
                .flatten()
                .collect::<BTreeSet<_>>()
                .len(),
        }
    }
    pub fn creator(&self, child: &str) -> Option<&str> {
        let mut creators = self
            .0
            .iter()
            .filter(|(parent, state)| parent.as_str() != child && state.created.contains(child));
        let (first, _) = creators.next()?;
        creators.next().is_none().then_some(first.as_str())
    }
    pub fn retain(&mut self, threads: &BTreeSet<String>) {
        self.0.retain(|id, _| threads.contains(id));
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn item(
        thread: &str,
        turn: &str,
        call: &str,
        tool: &str,
        status: &str,
        targets: Value,
    ) -> Value {
        json!({"method":"item/started","params":{"threadId":thread,"turnId":turn,"item":{"id":call,"type":"collabAgentToolCall","tool":tool,"status":status,"receiverThreadIds":targets}}})
    }
    fn turn(method: &str, turn: &str) -> Value {
        json!({"method":method,"params":{"threadId":"parent","turn":{"id":turn}}})
    }
    #[test]
    fn creation_is_not_waiting_and_waits_end_independently() {
        let mut t = Tracker::default();
        t.observe(&item(
            "parent",
            "turn1",
            "spawn",
            "spawnAgent",
            "completed",
            json!(["child"]),
        ))
        .unwrap();
        assert_eq!(t.creator("child"), Some("parent"));
        assert_eq!(
            t.snapshot("parent"),
            Snapshot {
                created: 1,
                ..Default::default()
            }
        );
        t.observe(&item(
            "parent",
            "turn1",
            "wait1",
            "wait",
            "inProgress",
            json!(["child", "child"]),
        ))
        .unwrap();
        t.observe(&item(
            "parent",
            "turn1",
            "wait2",
            "wait",
            "inProgress",
            json!([]),
        ))
        .unwrap();
        t.observe(&item(
            "parent",
            "turn1",
            "wait1",
            "wait",
            "completed",
            json!(["child"]),
        ))
        .unwrap();
        assert_eq!(
            t.snapshot("parent"),
            Snapshot {
                created: 1,
                waiting: true,
                targets: 0
            }
        );
        t.observe(&item(
            "parent",
            "turn1",
            "wait2",
            "wait",
            "failed",
            json!([]),
        ))
        .unwrap();
        assert!(!t.snapshot("parent").waiting);
    }
    #[test]
    fn new_turn_rejects_late_events_and_status_clears_pending_without_erasing_origin() {
        let mut t = Tracker::default();
        t.observe(&item(
            "parent",
            "old",
            "spawn",
            "spawnAgent",
            "completed",
            json!(["child"]),
        ))
        .unwrap();
        t.observe(&turn("turn/started", "new")).unwrap();
        t.observe(&item(
            "parent",
            "new",
            "wait",
            "wait",
            "inProgress",
            json!(["child"]),
        ))
        .unwrap();
        t.observe(&turn("turn/completed", "old")).unwrap();
        assert!(t.snapshot("parent").waiting);
        t.observe(&turn("turn/completed", "new")).unwrap();
        t.observe(&item(
            "parent",
            "new",
            "late",
            "wait",
            "inProgress",
            json!(["child"]),
        ))
        .unwrap();
        assert!(!t.snapshot("parent").waiting);
        assert_eq!(t.creator("child"), Some("parent"));
        t.observe(&turn("turn/started", "third")).unwrap();
        t.observe(&item(
            "parent",
            "third",
            "wait",
            "wait",
            "inProgress",
            json!([]),
        ))
        .unwrap();
        t.observe(&json!({"method":"thread/status/changed","params":{"threadId":"parent","status":{"type":"idle"}}})).unwrap();
        assert!(!t.snapshot("parent").waiting);
    }
    #[test]
    fn unknown_data_and_requests_do_not_create_links_or_exceed_budget() {
        let mut t = Tracker::default();
        let mut request = item(
            "parent",
            "turn",
            "call",
            "wait",
            "inProgress",
            json!(["child"]),
        );
        request["id"] = 1.into();
        assert!(!t.observe(&request).unwrap());
        assert!(!t.observe(&json!({"method":"item/started","params":{"threadId":"parent","item":{"type":"agentMessage","text":"spawnAgent child"}}})).unwrap());
        for n in 0..64 {
            t.observe(&item(
                "parent",
                "turn",
                &n.to_string(),
                "wait",
                "inProgress",
                json!([]),
            ))
            .unwrap();
        }
        assert!(
            t.observe(&item(
                "parent",
                "turn",
                "overflow",
                "wait",
                "inProgress",
                json!([])
            ))
            .is_err()
        );
        t.observe(&item(
            "other",
            "turn",
            "spawn",
            "spawnAgent",
            "completed",
            json!(["child"]),
        ))
        .unwrap();
        t.observe(&item(
            "parent",
            "turn",
            "spawn",
            "spawnAgent",
            "completed",
            json!(["child"]),
        ))
        .unwrap();
        assert_eq!(t.creator("child"), None);
        t.retain(&BTreeSet::from(["other".into()]));
        assert_eq!(t.creator("child"), Some("other"));
    }
}
