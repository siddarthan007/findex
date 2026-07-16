//! Persistent, bounded MCP task state.

use crate::cancellation::CancellationToken;
use crate::storage::{Storage, StorageError};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const TASK_INDEX_KEY: &str = "mcp:tasks:index";

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Working,
    Completed,
    Failed,
    Cancelled,
    InputRequired,
}

impl TaskStatus {
    pub fn terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskRecord {
    pub task_id: String,
    pub status: TaskStatus,
    pub status_message: String,
    pub created_at: String,
    pub last_updated_at: String,
    pub ttl: u64,
    pub poll_interval: u64,
    pub tool: String,
    pub result: Option<Value>,
    expires_at_ms: u128,
}

impl TaskRecord {
    pub fn protocol_value(&self) -> Value {
        json!({
            "taskId": self.task_id,
            "status": self.status,
            "statusMessage": self.status_message,
            "createdAt": self.created_at,
            "lastUpdatedAt": self.last_updated_at,
            "ttl": self.ttl,
            "pollInterval": self.poll_interval
        })
    }
}

struct TaskState {
    tasks: HashMap<String, TaskRecord>,
    tokens: HashMap<String, CancellationToken>,
}

pub struct TaskManager {
    storage: Arc<Storage>,
    state: Mutex<TaskState>,
    changed: Condvar,
    max_concurrent: usize,
    max_ttl_ms: u64,
}

impl TaskManager {
    pub fn new(storage: Arc<Storage>) -> Self {
        let ids = storage
            .get_metadata::<Vec<String>>(TASK_INDEX_KEY)
            .ok()
            .flatten()
            .unwrap_or_default();
        let tasks = ids
            .into_iter()
            .filter_map(|id| {
                storage
                    .get_metadata::<TaskRecord>(&format!("mcp:task:{id}"))
                    .ok()
                    .flatten()
                    .map(|task| (id, task))
            })
            .collect();
        Self {
            storage,
            state: Mutex::new(TaskState {
                tasks,
                tokens: HashMap::new(),
            }),
            changed: Condvar::new(),
            max_concurrent: env_usize("FINDEX_MAX_CONCURRENT_TASKS", 4).clamp(1, 64),
            max_ttl_ms: env_u64("FINDEX_MAX_TASK_TTL_MS", 3_600_000).clamp(10_000, 86_400_000),
        }
    }

    pub fn create(&self, tool: &str, requested_ttl: Option<u64>) -> Result<TaskRecord, String> {
        let mut state = self.state.lock().expect("task state poisoned");
        self.cleanup_locked(&mut state);
        let working = state
            .tasks
            .values()
            .filter(|task| task.status == TaskStatus::Working)
            .count();
        if working >= self.max_concurrent {
            return Err(format!(
                "concurrent task limit reached ({})",
                self.max_concurrent
            ));
        }
        let ttl = requested_ttl
            .unwrap_or(300_000)
            .clamp(10_000, self.max_ttl_ms);
        let task_id = uuid::Uuid::new_v4().to_string();
        let timestamp = now_rfc3339();
        let record = TaskRecord {
            task_id: task_id.clone(),
            status: TaskStatus::Working,
            status_message: format!("{tool} queued"),
            created_at: timestamp.clone(),
            last_updated_at: timestamp,
            ttl,
            poll_interval: 250,
            tool: tool.to_string(),
            result: None,
            expires_at_ms: now_ms().saturating_add(ttl as u128),
        };
        state.tasks.insert(task_id.clone(), record.clone());
        state.tokens.insert(task_id, CancellationToken::default());
        self.persist_locked(&state)
            .map_err(|error| error.to_string())?;
        Ok(record)
    }

    pub fn complete(&self, task_id: &str, result: Value, failed: bool) {
        let mut state = self.state.lock().expect("task state poisoned");
        if let Some(task) = state.tasks.get_mut(task_id) {
            if task.status == TaskStatus::Cancelled {
                return;
            }
            task.status = if failed {
                TaskStatus::Failed
            } else {
                TaskStatus::Completed
            };
            task.status_message = if failed {
                format!("{} failed", task.tool)
            } else {
                format!("{} completed", task.tool)
            };
            task.last_updated_at = now_rfc3339();
            task.result = Some(result);
            state.tokens.remove(task_id);
            let _ = self.persist_locked(&state);
        }
        self.changed.notify_all();
    }

    pub fn get(&self, task_id: &str) -> Option<TaskRecord> {
        let mut state = self.state.lock().expect("task state poisoned");
        self.cleanup_locked(&mut state);
        state.tasks.get(task_id).cloned()
    }

    pub fn list(&self) -> Vec<TaskRecord> {
        let mut state = self.state.lock().expect("task state poisoned");
        self.cleanup_locked(&mut state);
        let mut tasks: Vec<_> = state.tasks.values().cloned().collect();
        tasks.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        tasks
    }

    pub fn cancel(&self, task_id: &str) -> Result<TaskRecord, String> {
        let mut state = self.state.lock().expect("task state poisoned");
        self.cleanup_locked(&mut state);
        if let Some(token) = state.tokens.get(task_id) {
            token.cancel();
        }
        let record = {
            let task = state
                .tasks
                .get_mut(task_id)
                .ok_or_else(|| "task not found".to_string())?;
            if task.status.terminal() {
                return Err("only active tasks can be cancelled".to_string());
            }
            task.status = TaskStatus::Cancelled;
            task.status_message = format!("{} cancelled", task.tool);
            task.last_updated_at = now_rfc3339();
            task.clone()
        };
        self.persist_locked(&state)
            .map_err(|error| error.to_string())?;
        self.changed.notify_all();
        Ok(record)
    }

    pub fn token(&self, task_id: &str) -> Option<CancellationToken> {
        self.state
            .lock()
            .expect("task state poisoned")
            .tokens
            .get(task_id)
            .cloned()
    }

    pub fn wait_terminal(&self, task_id: &str) -> Option<TaskRecord> {
        let mut state = self.state.lock().expect("task state poisoned");
        loop {
            self.cleanup_locked(&mut state);
            let task = state.tasks.get(task_id)?.clone();
            if task.status.terminal() {
                return Some(task);
            }
            let (next, _) = self
                .changed
                .wait_timeout(state, Duration::from_millis(task.poll_interval.max(50)))
                .expect("task state poisoned");
            state = next;
        }
    }

    fn cleanup_locked(&self, state: &mut TaskState) {
        let now = now_ms();
        let expired: Vec<_> = state
            .tasks
            .iter()
            .filter(|(_, task)| task.expires_at_ms <= now)
            .map(|(id, _)| id.clone())
            .collect();
        for id in expired {
            state.tasks.remove(&id);
            if let Some(token) = state.tokens.remove(&id) {
                token.cancel();
            }
            let _ = self.storage.remove_metadata(&format!("mcp:task:{id}"));
        }
        let _ = self.persist_locked(state);
    }

    fn persist_locked(&self, state: &TaskState) -> Result<(), StorageError> {
        let ids: Vec<_> = state.tasks.keys().cloned().collect();
        self.storage.set_metadata(TASK_INDEX_KEY, &ids)?;
        for (id, task) in &state.tasks {
            self.storage.set_metadata(&format!("mcp:task:{id}"), task)?;
        }
        Ok(())
    }
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn now_rfc3339() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}

fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}
