//! opencode 的 `dump()` → Markdown 对话模型。
//!
//! opencode 库里同一个会话可能有两套记录（见 `dump()`）：
//!
//! | schema | 消息 | 片段 |
//! |---|---|---|
//! | v2（当前） | `session_message.data` 一个 JSON 对象 | 助手消息里的 `content[]` |
//! | v1（旧会话） | `message.data`（角色） | `part.data` 按 `message_id` 归属 |
//!
//! 实测 841 个 v2 会话**同时**还留着 v1 的 `message` / `part` 行，两套都渲染
//! 会把整段对话打印两遍。所以固定「`session_message` 非空就只读 v2，否则
//! 回落 v1」—— 与列表/空壳判定用的是同一套优先级。

use std::collections::HashMap;

use aa_session_core::markdown::{Message, Part, ToolCall};
use serde_json::Value;

/// 从 [`crate::OpencodeSource::dump`] 的结果解析出对话。
pub fn from_dump(dump: &Value) -> Vec<Message> {
    let v2 = dump
        .get("session_message")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();

    if v2.is_empty() {
        from_v1(dump)
    } else {
        from_v2(v2)
    }
}

// ----------------------------------------------------------------- v2 schema

fn from_v2(rows: &[Value]) -> Vec<Message> {
    let mut out = Vec::new();

    for row in rows {
        let Some(data) = parse_data(row) else {
            continue;
        };
        let kind = row.get("type").and_then(Value::as_str).unwrap_or_default();

        let message = match kind {
            "user" => Message::user(v2_user_parts(&data)),
            "system" => Message::system(text_part(data.get("text"))),
            "assistant" => Message::assistant(v2_assistant_parts(&data)),
            // 未知类型（将来新增的 role）不猜，跳过。
            _ => continue,
        };

        if !message.is_empty() {
            out.push(message);
        }
    }

    out
}

fn v2_user_parts(data: &Value) -> Vec<Part> {
    let mut parts = text_part(data.get("text"));

    // 附件（实测全是内联图片）：正文里已有 `[Image 1]` 占位，这里补一行
    // 可辨认的来源信息，但**不内联 base64**（那会把文件撑到几 MB 且不可读）。
    for file in data
        .get("files")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let name = file.get("name").and_then(Value::as_str).unwrap_or("image");
        let mime = file
            .get("mime")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        parts.push(Part::Text(format!(
            "<image name=\"{name}\" mime=\"{mime}\" />"
        )));
    }

    parts
}

fn v2_assistant_parts(data: &Value) -> Vec<Part> {
    let mut parts = Vec::new();

    for block in data
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => parts.extend(text_part(block.get("text"))),
            Some("reasoning") => parts.extend(as_thinking(text_part(block.get("text")))),
            Some("tool") => {
                if let Some(tool) = v2_tool(block) {
                    parts.push(Part::Tool(tool));
                }
            }
            _ => {}
        }
    }

    parts
}

fn v2_tool(block: &Value) -> Option<ToolCall> {
    let state = block.get("state")?;
    let mut tool = ToolCall::new(
        block.get("id").and_then(Value::as_str).unwrap_or_default(),
        block.get("name").and_then(Value::as_str).unwrap_or("tool"),
        state.get("input").cloned().unwrap_or(Value::Null),
    );
    tool.status = state
        .get("status")
        .and_then(Value::as_str)
        .map(str::to_string);
    tool.output = state_output(state);
    tool.error = state_error(state);
    Some(tool)
}

// ----------------------------------------------------------------- v1 schema

fn from_v1(dump: &Value) -> Vec<Message> {
    let messages = dump
        .get("message")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let parts = dump
        .get("part")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();

    // 一条消息的片段可能很多（891 个会话里 part 总量是消息的数倍），
    // 先按 message_id 归组，避免 O(消息 × 片段)。
    let mut by_message: HashMap<&str, Vec<&Value>> = HashMap::new();
    for part in parts {
        if let Some(message_id) = part.get("message_id").and_then(Value::as_str) {
            by_message.entry(message_id).or_default().push(part);
        }
    }

    let mut out = Vec::new();
    for row in messages {
        let Some(data) = parse_data(row) else {
            continue;
        };
        let role = data.get("role").and_then(Value::as_str).unwrap_or_default();
        let empty = Vec::new();
        let rows = row
            .get("id")
            .and_then(Value::as_str)
            .and_then(|id| by_message.get(id))
            .unwrap_or(&empty);

        let mut parts = Vec::new();
        for part in rows {
            let Some(part_data) = parse_data(part) else {
                continue;
            };
            match part_data.get("type").and_then(Value::as_str) {
                // step-start / step-finish 是编排元数据，不是对话内容。
                Some("text") => parts.extend(text_part(part_data.get("text"))),
                Some("reasoning") => {
                    parts.extend(as_thinking(text_part(part_data.get("text"))));
                }
                Some("tool") => {
                    if let Some(tool) = v1_tool(&part_data) {
                        parts.push(Part::Tool(tool));
                    }
                }
                _ => {}
            }
        }

        let message = match role {
            "user" => Message::user(parts),
            "assistant" => Message::assistant(parts),
            _ => continue,
        };
        if !message.is_empty() {
            out.push(message);
        }
    }

    out
}

fn v1_tool(data: &Value) -> Option<ToolCall> {
    let state = data.get("state")?;
    let mut tool = ToolCall::new(
        data.get("callID")
            .and_then(Value::as_str)
            .unwrap_or_default(),
        // v1 的字段叫 `tool`，v2 叫 `name` —— 这里不是笔误。
        data.get("tool").and_then(Value::as_str).unwrap_or("tool"),
        state.get("input").cloned().unwrap_or(Value::Null),
    );
    tool.status = state
        .get("status")
        .and_then(Value::as_str)
        .map(str::to_string);
    tool.output = state_output(state);
    tool.error = state_error(state);
    Some(tool)
}

// ------------------------------------------------------------------- 公共小工具

/// `dump()` 里 `data` / `message.data` / `part.data` 都是 JSON **字符串**。
fn parse_data(row: &Value) -> Option<Value> {
    serde_json::from_str(row.get("data")?.as_str()?).ok()
}

fn text_part(text: Option<&Value>) -> Vec<Part> {
    match text.and_then(Value::as_str) {
        Some(text) if !text.trim().is_empty() => vec![Part::Text(text.to_string())],
        _ => Vec::new(),
    }
}

fn as_thinking(parts: Vec<Part>) -> Vec<Part> {
    parts
        .into_iter()
        .map(|p| match p {
            Part::Text(text) => Part::Thinking(text),
            other => other,
        })
        .collect()
}

/// 工具的返回正文：优先 `state.output`（字符串），否则拼接 `state.content`
/// 里的文本块。两种形态在实测数据里都存在（read → content，bash → output）。
fn state_output(state: &Value) -> Option<String> {
    if let Some(text) = state.get("output").and_then(Value::as_str)
        && !text.trim().is_empty()
    {
        return Some(text.to_string());
    }

    let texts: Vec<&str> = state
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|block| block.get("text").and_then(Value::as_str))
        .collect();

    (!texts.is_empty()).then(|| texts.join("\n"))
}

/// 工具错误：v2 是 `{type, message}` 对象，v1 也可能是字符串。
fn state_error(state: &Value) -> Option<String> {
    match state.get("error")? {
        Value::String(text) if !text.trim().is_empty() => Some(text.clone()),
        Value::Object(map) => map
            .get("message")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| serde_json::to_string_pretty(state.get("error")?).ok()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn row(kind: &str, data: Value) -> Value {
        json!({
            "id": format!("msg_{kind}"),
            "session_id": "ses_x",
            "type": kind,
            "seq": 0,
            "data": data.to_string(),
        })
    }

    #[test]
    fn renders_v2_user_and_assistant_blocks_in_order() {
        let dump = json!({
            "session_message": [
                row("user", json!({"text": "看看这个 bug"})),
                row("assistant", json!({
                    "agent": "build",
                    "content": [
                        {"type": "reasoning", "text": "先读文件"},
                        {"type": "text", "text": "我看看"},
                        {"type": "tool", "id": "call_1", "name": "read",
                         "state": {"status": "completed", "input": {"filePath": "a.rs"},
                                   "output": "fn main() {}"}},
                    ],
                })),
            ],
        });

        let messages = from_dump(&dump);
        assert_eq!(messages.len(), 2);
        assert_eq!(
            messages[0],
            Message::user(vec![Part::Text("看看这个 bug".into())])
        );

        let ToolCall {
            id,
            name,
            status,
            output,
            ..
        } = match &messages[1].parts[2] {
            Part::Tool(tool) => tool,
            other => panic!("expected tool, got {other:?}"),
        };
        assert_eq!(id, "call_1");
        assert_eq!(name, "read");
        assert_eq!(status.as_deref(), Some("completed"));
        assert_eq!(output.as_deref(), Some("fn main() {}"));
        assert!(matches!(messages[1].parts[0], Part::Thinking(_)));
    }

    #[test]
    fn v2_tool_prefers_content_blocks_when_output_is_absent() {
        let dump = json!({
            "session_message": [row("assistant", json!({
                "content": [{"type": "tool", "id": "c", "name": "read",
                             "state": {"status": "completed", "input": {},
                                       "content": [
                                           {"type": "text", "text": "第一段"},
                                           {"type": "text", "text": "第二段"},
                                       ]}}],
            }))],
        });
        let messages = from_dump(&dump);
        let tool = match &messages[0].parts[0] {
            Part::Tool(tool) => tool,
            other => panic!("expected tool, got {other:?}"),
        };
        assert_eq!(tool.output.as_deref(), Some("第一段\n第二段"));
    }

    #[test]
    fn v2_error_object_uses_its_message() {
        let dump = json!({
            "session_message": [row("assistant", json!({
                "content": [{"type": "tool", "id": "c", "name": "grep",
                             "state": {"status": "error", "input": {},
                                       "error": {"type": "tool.execution",
                                                 "message": "regex parse error"}}}],
            }))],
        });
        let messages = from_dump(&dump);
        let tool = match &messages[0].parts[0] {
            Part::Tool(tool) => tool,
            other => panic!("expected tool, got {other:?}"),
        };
        assert_eq!(tool.error.as_deref(), Some("regex parse error"));
    }

    #[test]
    fn v2_image_files_become_placeholders_without_base64() {
        let dump = json!({
            "session_message": [row("user", json!({
                "text": "看这张图",
                "files": [{"name": "shot.png", "mime": "image/png",
                           "data": "iVBORw0KGgo="}],
            }))],
        });
        let messages = from_dump(&dump);
        let Part::Text(image) = &messages[0].parts[1] else {
            panic!("expected image placeholder");
        };
        assert!(image.contains("shot.png"));
        assert!(!image.contains("iVBORw0KGgo="), "base64 不应进 Markdown");
    }

    #[test]
    fn v1_fallback_reads_message_and_part() {
        let dump = json!({
            "session_message": [],
            "message": [
                {"id": "m1", "data": json!({"role": "user"}).to_string()},
                {"id": "m2", "data": json!({"role": "assistant"}).to_string()},
            ],
            "part": [
                {"id": "p1", "message_id": "m1", "data": json!({"type": "text", "text": "帮我提交"}).to_string()},
                {"id": "p2", "message_id": "m2", "data": json!({"type": "step-start"}).to_string()},
                {"id": "p3", "message_id": "m2", "data": json!({"type": "tool", "tool": "bash",
                    "callID": "call_x",
                    "state": {"status": "completed", "input": {"command": "git status"},
                              "output": "clean"}}).to_string()},
            ],
        });

        let messages = from_dump(&dump);
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, aa_session_core::markdown::Role::User);
        assert_eq!(messages[0].parts.len(), 1, "step-start 不该产生片段");

        let tool = match &messages[1].parts[0] {
            Part::Tool(tool) => tool,
            other => panic!("expected tool, got {other:?}"),
        };
        // v1 工具名在 `tool` 字段，不是 `name`
        assert_eq!(tool.name, "bash");
        assert_eq!(tool.id, "call_x");
        assert_eq!(tool.output.as_deref(), Some("clean"));
    }

    #[test]
    fn v2_wins_when_both_schemas_are_present() {
        // 841 个 v2 会话同时残留 v1 记录；两套都读会打印两遍对话。
        let dump = json!({
            "session_message": [row("user", json!({"text": "v2 正文"}))],
            "message": [{"id": "m1", "data": json!({"role": "user"}).to_string()}],
            "part": [{"id": "p1", "message_id": "m1",
                      "data": json!({"type": "text", "text": "v1 正文"}).to_string()}],
        });
        let messages = from_dump(&dump);
        assert_eq!(messages.len(), 1);
        assert_eq!(
            messages[0],
            Message::user(vec![Part::Text("v2 正文".into())])
        );
    }
}
