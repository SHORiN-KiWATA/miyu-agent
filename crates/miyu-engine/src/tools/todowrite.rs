use super::{ToolRegistry, ToolSpec};
use anyhow::Result;
use miyu_base::config::AppConfig;
use miyu_base::paths::MiyuPaths;
use miyu_core::state::{SessionValueKind, StateStore};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Todo {
    pub content: String,
    pub status: String,
    pub priority: String,
}

pub type TodoList = Arc<Mutex<Vec<Todo>>>;

// 存储绑定(任务#13):按会话存。旧实现是注册时的单例 Arc<Mutex<Vec>>,daemon
// 按 config 缓存复用 registry 后所有会话共享同一份(串味实锤);现在每次调用
// 按当前回合的会话加载/回存,纯函数 todo_write/todo_update 与其测试原样保留。
// 09-24 起住在会话库里(`session_values`,kind=todos),删会话时级联带走;以前是
// `state_dir/todos/<会话>.json`,删会话没人清(实测 11/11 是孤儿)。

/// 某个会话当前的待办清单。
///
/// WebUI 的常驻面板要在刷新之后还能显示当前状态，而工具事件只在工具跑的
/// 那一刻发生一次。读取收口在这里，损坏容错的规则只该有一份。
pub fn session_todos(store: &StateStore, session: &str) -> Vec<Todo> {
    load_todos(store, session)
}

/// 清掉某个会话的待办。
///
/// 「重置对话」那条路上一串清理动作都走 `clear_session_content`，唯独待办
/// 不在它清的那几张表里——对话重来了，上一轮的待办还挂在侧边面板上，模型
/// 下一次读 todo 也还是旧的。
pub fn clear_session_todos(store: &StateStore, session: &str) -> Result<()> {
    store.clear_session_value(session, SessionValueKind::Todos)
}

fn load_todos(store: &StateStore, session: &str) -> Vec<Todo> {
    let raw = match store.session_value(session, SessionValueKind::Todos) {
        Ok(Some(raw)) => raw,
        Ok(None) => return Vec::new(),
        Err(error) => {
            tracing::warn!(session, error = %error, "todo list could not be read; starting with an empty list");
            return Vec::new();
        }
    };
    match serde_json::from_str(&raw) {
        Ok(todos) => todos,
        Err(error) => {
            // 容错语义保留(损坏即重新开始),但必须留痕:任务清单无声消失
            // 会让模型以为自己从没建过清单。
            tracing::warn!(
                session,
                error = %error,
                "todo list is corrupt; starting with an empty list"
            );
            Vec::new()
        }
    }
}

fn save_todos(store: &StateStore, session: &str, todos: &[Todo]) -> Result<()> {
    store.set_session_value(
        session,
        SessionValueKind::Todos,
        &serde_json::to_string_pretty(todos)?,
    )
}

fn session_for_call() -> Result<String> {
    miyu_base::workspace::try_session()
        .map(|session| session.to_string())
        .ok_or_else(|| anyhow::anyhow!("todo tools require a session turn"))
}

fn run_scoped(
    config: &AppConfig,
    paths: &MiyuPaths,
    args: Value,
    apply: fn(Value, TodoList) -> Result<String>,
) -> Result<(String, Vec<Todo>)> {
    let session = session_for_call()?;
    let store = super::session_store::store_for(config, paths)?;
    let todos: TodoList = Arc::new(Mutex::new(load_todos(&store, &session)));
    let output = apply(args, Arc::clone(&todos))?;
    let list = todos.lock().expect("todo state lock").clone();
    save_todos(&store, &session, &list)?;
    Ok((output, list))
}

/// todowrite + todoupdate 合并(08-17):同一份清单的整表替换与增量修改。
/// 给了 updates 就走增量,给了 todos 就整表替换。
pub fn register(registry: &mut ToolRegistry, config: AppConfig, paths: MiyuPaths) {
    registry.register(ToolSpec::new_with_progress(
        "todowrite",
        "Maintain the structured task list for the current session. Pass todos to create or replace the whole list; pass updates to apply small atomic changes (add, update, remove, clear) without resending everything. Exactly one of the two.",
        json!({
            "type": "object",
            "properties": {
                "todos": {
                    "type": "array",
                    "description": "The full todo list. This replaces the entire list.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "content": { "type": "string", "description": "Brief description of the task." },
                            "status": {
                                "type": "string",
                                "enum": ["pending", "in_progress", "completed", "cancelled"],
                                "description": "Current status of the task."
                            },
                            "priority": {
                                "type": "string",
                                "enum": ["high", "medium", "low"],
                                "description": "Priority level of the task."
                            }
                        },
                        "required": ["content", "status", "priority"]
                    }
                },
                "updates": {
                    "type": "array",
                    "description": "Sequential todo mutations to apply atomically.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "action": {
                                "type": "string",
                                "enum": ["add", "update", "remove", "clear"],
                                "description": "Mutation type."
                            },
                            "index": {
                                "type": "integer",
                                "description": "1-based target item index. For add, inserts at this position; omitted means append."
                            },
                            "match_content": {
                                "type": "string",
                                "description": "Exact content used to find the target when index is omitted."
                            },
                            "content": { "type": "string", "description": "New task content for add or update." },
                            "status": {
                                "type": "string",
                                "enum": ["pending", "in_progress", "completed", "cancelled"],
                                "description": "Updated task status."
                            },
                            "priority": {
                                "type": "string",
                                "enum": ["high", "medium", "low"],
                                "description": "Updated task priority."
                            }
                        },
                        "required": ["action"],
                        "additionalProperties": false
                    }
                }
            },
            "additionalProperties": false
        }),
        move |args, progress| {
            let paths = paths.clone();
            let config = config.clone();
            async move {
                let apply = if args.get("updates").is_some() {
                    todo_update
                } else {
                    todo_write
                };
                let (output, list) = run_scoped(&config, &paths, args, apply)?;
                // 表格数据走 progress 侧信道给 REPL/WebUI 画,不再进模型字节
                // (08-21 token-diet:整表回显是 65% 结构开销的纯浪费)。
                progress.report(format!("__todo_table__{}", todo_table_payload(&list)));
                Ok(output)
            }
        },
    ).writes());
}

/// REPL `write_todo_table` 与 WebUI 面板共用的表格载荷,形状与旧版工具输出
/// 的 `todos` 字段一致,渲染端无需区分新旧。
fn todo_table_payload(list: &[Todo]) -> String {
    json!({ "todos": list }).to_string()
}

fn todo_write(args: Value, todos: TodoList) -> Result<String> {
    let items = args
        .get("todos")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("todos array is required"))?;

    let mut list = Vec::with_capacity(items.len());
    for item in items {
        let content = item
            .get("content")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_string();
        if content.is_empty() {
            anyhow::bail!("todo content must not be empty");
        }
        // 与 todoupdate 同一套校验:enum 外的值静默入库会让 pending 计数
        // 和状态展示错乱。
        let status = item
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("pending");
        validate_status(status)?;
        let priority = item
            .get("priority")
            .and_then(Value::as_str)
            .unwrap_or("medium");
        validate_priority(priority)?;
        list.push(Todo {
            content,
            status: status.to_string(),
            priority: priority.to_string(),
        });
    }

    let mut state = todos.lock().expect("todo state lock");
    *state = list.clone();
    drop(state);

    todo_output("write", &list)
}

fn todo_update(args: Value, todos: TodoList) -> Result<String> {
    let updates = args
        .get("updates")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("updates array is required"))?;
    if updates.is_empty() {
        anyhow::bail!("updates must not be empty");
    }

    let mut state = todos.lock().expect("todo state lock");
    let mut list = state.clone();
    for update in updates {
        let action = update
            .get("action")
            .and_then(Value::as_str)
            .unwrap_or_default();
        match action {
            "add" => {
                let todo = todo_from_update(update)?;
                let insert_at = match update.get("index").and_then(Value::as_u64) {
                    Some(index) if index == 0 => anyhow::bail!("index must be 1-based"),
                    Some(index) => (index as usize - 1).min(list.len()),
                    None => list.len(),
                };
                list.insert(insert_at, todo);
            }
            "update" => {
                let idx = target_index(update, &list)?;
                if let Some(content) = update.get("content").and_then(Value::as_str) {
                    let content = content.trim();
                    if content.is_empty() {
                        anyhow::bail!("todo content must not be empty");
                    }
                    list[idx].content = content.to_string();
                }
                if let Some(status) = update.get("status").and_then(Value::as_str) {
                    validate_status(status)?;
                    list[idx].status = status.to_string();
                }
                if let Some(priority) = update.get("priority").and_then(Value::as_str) {
                    validate_priority(priority)?;
                    list[idx].priority = priority.to_string();
                }
            }
            "remove" => {
                let idx = target_index(update, &list)?;
                list.remove(idx);
            }
            "clear" => list.clear(),
            _ => anyhow::bail!("action must be add, update, remove, or clear"),
        }
    }
    *state = list.clone();
    drop(state);
    todo_output("update", &list)
}

fn todo_from_update(update: &Value) -> Result<Todo> {
    let content = update
        .get("content")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    if content.is_empty() {
        anyhow::bail!("content is required for add");
    }
    let status = update
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("pending");
    validate_status(status)?;
    let priority = update
        .get("priority")
        .and_then(Value::as_str)
        .unwrap_or("medium");
    validate_priority(priority)?;
    Ok(Todo {
        content,
        status: status.to_string(),
        priority: priority.to_string(),
    })
}

fn target_index(update: &Value, list: &[Todo]) -> Result<usize> {
    if let Some(index) = update.get("index").and_then(Value::as_u64) {
        if index == 0 || index as usize > list.len() {
            anyhow::bail!("index out of range");
        }
        return Ok(index as usize - 1);
    }
    let Some(content) = update.get("match_content").and_then(Value::as_str) else {
        anyhow::bail!("index or match_content is required");
    };
    let matches = list
        .iter()
        .enumerate()
        .filter(|(_, todo)| todo.content == content)
        .map(|(idx, _)| idx)
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [idx] => Ok(*idx),
        [] => anyhow::bail!("no todo matches match_content"),
        _ => anyhow::bail!("match_content matches multiple todos; use index instead"),
    }
}

fn validate_status(status: &str) -> Result<()> {
    if matches!(
        status,
        "pending" | "in_progress" | "completed" | "cancelled"
    ) {
        Ok(())
    } else {
        anyhow::bail!("status must be pending, in_progress, completed, or cancelled")
    }
}

fn validate_priority(priority: &str) -> Result<()> {
    if matches!(priority, "high" | "medium" | "low") {
        Ok(())
    } else {
        anyhow::bail!("priority must be high, medium, or low")
    }
}

/// 给模型的返回体:一行文本确认。整表不回显——模型刚提交的清单它自己知道,
/// 渲染端的表格另走 `__todo_table__` progress 与 `GET /api/sessions/{id}/todos`。
/// 旧 JSON 形态仍被 REPL/WebUI 解析器兼容(历史回合逐字节回放)。
fn todo_output(operation: &str, list: &[Todo]) -> Result<String> {
    let verb = if operation == "write" {
        "replaced"
    } else {
        "updated"
    };
    let mut parts = Vec::new();
    for status in ["in_progress", "pending", "completed", "cancelled"] {
        let count = list.iter().filter(|t| t.status == status).count();
        if count > 0 {
            parts.push(format!("{count} {status}"));
        }
    }
    let breakdown = if parts.is_empty() {
        String::new()
    } else {
        format!(" — {}", parts.join(", "))
    };
    Ok(format!(
        "todo list {verb}: {} item(s){breakdown}",
        list.len()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replace_full_list() {
        let todos: TodoList = Arc::new(Mutex::new(Vec::new()));
        let result = todo_write(
            json!({
                "todos": [
                    {"content": "task A", "status": "completed", "priority": "high"},
                    {"content": "task B", "status": "in_progress", "priority": "medium"},
                    {"content": "task C", "status": "pending", "priority": "low"},
                ]
            }),
            Arc::clone(&todos),
        )
        .unwrap();
        assert_eq!(
            result,
            "todo list replaced: 3 item(s) — 1 in_progress, 1 pending, 1 completed"
        );
        assert_eq!(todos.lock().unwrap().len(), 3);
    }

    #[test]
    fn empty_list_clears_all() {
        let todos: TodoList = Arc::new(Mutex::new(vec![Todo {
            content: "old".into(),
            status: "pending".into(),
            priority: "low".into(),
        }]));
        let result = todo_write(json!({"todos": []}), Arc::clone(&todos)).unwrap();
        assert_eq!(result, "todo list replaced: 0 item(s)");
        assert!(todos.lock().unwrap().is_empty());
    }

    #[test]
    fn empty_content_rejected() {
        let todos: TodoList = Arc::new(Mutex::new(Vec::new()));
        let result = todo_write(
            json!({"todos": [{"content": "  ", "status": "pending", "priority": "low"}]}),
            Arc::clone(&todos),
        );
        assert!(result.is_err());
    }

    #[test]
    fn update_status_by_index_is_atomic() {
        let todos: TodoList = Arc::new(Mutex::new(vec![
            Todo {
                content: "task A".into(),
                status: "pending".into(),
                priority: "high".into(),
            },
            Todo {
                content: "task B".into(),
                status: "in_progress".into(),
                priority: "medium".into(),
            },
        ]));
        let result = todo_update(
            json!({"updates": [{"action": "update", "index": 1, "status": "completed"}]}),
            Arc::clone(&todos),
        )
        .unwrap();
        assert!(
            result.starts_with("todo list updated: 2 item(s)"),
            "{result}"
        );
        let list = todos.lock().unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].content, "task A");
        assert_eq!(list[0].status, "completed");
        assert_eq!(list[1].content, "task B");
        assert_eq!(list[1].status, "in_progress");
    }

    #[test]
    fn update_adds_and_removes_without_resending_full_list() {
        let todos: TodoList = Arc::new(Mutex::new(vec![Todo {
            content: "keep".into(),
            status: "pending".into(),
            priority: "low".into(),
        }]));
        todo_update(
            json!({"updates": [
                {"action": "add", "content": "new task", "status": "in_progress", "priority": "high"},
                {"action": "remove", "match_content": "keep"}
            ]}),
            Arc::clone(&todos),
        )
        .unwrap();
        let list = todos.lock().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].content, "new task");
        assert_eq!(list[0].status, "in_progress");
        assert_eq!(list[0].priority, "high");
    }

    #[test]
    fn update_rejects_ambiguous_content_match() {
        let todos: TodoList = Arc::new(Mutex::new(vec![
            Todo {
                content: "same".into(),
                status: "pending".into(),
                priority: "low".into(),
            },
            Todo {
                content: "same".into(),
                status: "pending".into(),
                priority: "low".into(),
            },
        ]));
        let result = todo_update(
            json!({"updates": [{"action": "update", "match_content": "same", "status": "completed"}]}),
            Arc::clone(&todos),
        );
        assert!(result.is_err());
    }
}
