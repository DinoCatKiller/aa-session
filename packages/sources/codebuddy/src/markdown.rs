//! codebuddy 的 JSONL 事件流 → Markdown 对话模型。
//!
//! codebuddy 不存「消息」，而是一个扁平的**事件流**：一条用户消息、若干
//! 推理、若干工具调用 / 结果、若干助手文本，各占一行。渲染时按事件顺序
//! 归并成「一条消息 + 若干片段」：
//!
//! - `message`（user）→ 新开一条 `## User`；
//! - `message` / `reasoning` / `function_call` / `function_call_result`
//!   （assistant）→ 追加到当前这条 `## Assistant`，直到下一条用户消息；
//! - `summary` / `session-meta` / `turn-metrics` 等元数据事件直接跳过
//!   （它们在 `dump()` 里仍保留，供原始 JSON 导出用）。
//!
//! 工具结果单独一条事件，靠 `callId` 回填到对应的调用上 —— 这样 Markdown 里
//! 调用和结果紧挨着，不用在几十行之后找「到底是哪个命令的输出」。

use std::collections::HashMap;

use aa_session_core::markdown::{Message, Part, Role, ToolCall};
use serde_json::Value;

/// 从 [`crate::CodebuddySource::dump`] 的结果解析出对话。
pub fn from_dump(dump: &Value) -> Vec<Message> {
    let events = dump
        .get("events")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();

    let mut out: Vec<Message> = Vec::new();
    // callId → (消息下标, 片段下标)
    let mut calls: HashMap<String, (usize, usize)> = HashMap::new();

    for event in events {
        match event
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default()
        {
            "message" => {
                let parts = message_parts(event);
                if parts.is_empty() {
                    continue;
                }
                match event.get("role").and_then(Value::as_str) {
                    Some("user") => out.push(Message::user(parts)),
                    Some("assistant") => {
                        let ix = assistant_index(&mut out);
                        out[ix].parts.extend(parts);
                    }
                    _ => {}
                }
            }
            "reasoning" => {
                let parts = reasoning_parts(event);
                if !parts.is_empty() {
                    let ix = assistant_index(&mut out);
                    out[ix].parts.extend(parts);
                }
            }
            "function_call" => {
                if let Some(tool) = function_call(event) {
                    let call_id = tool.id.clone();
                    let ix = assistant_index(&mut out);
                    let part_ix = out[ix].parts.len();
                    out[ix].parts.push(Part::Tool(tool));
                    calls.insert(call_id, (ix, part_ix));
                }
            }
            "function_call_result" => {
                let call_id = event
                    .get("callId")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let Some((message_ix, part_ix)) = calls.get(&call_id).copied() else {
                    // 没有对应调用的孤立结果（截断的 JSONL）：仍保留正文，
                    // 别让用户看到的报告凭空少一段。
                    let mut tool = ToolCall::new(call_id, "tool", Value::Null);
                    fill_result(&mut tool, event);
                    let ix = assistant_index(&mut out);
                    out[ix].parts.push(Part::Tool(tool));
                    continue;
                };
                if let Some(Part::Tool(tool)) = out
                    .get_mut(message_ix)
                    .and_then(|m| m.parts.get_mut(part_ix))
                {
                    fill_result(tool, event);
                }
            }
            // 其余都是元数据 / 统计事件，不进对话正文。
            _ => {}
        }
    }

    out.retain(|message| !message.is_empty());
    out
}

/// 当前助手消息的下标；最后一条不是助手就新开一条。
fn assistant_index(out: &mut Vec<Message>) -> usize {
    if out.last().map(|m| m.role) != Some(Role::Assistant) {
        out.push(Message::assistant(Vec::new()));
    }
    out.len() - 1
}

/// `message` 事件的 content 数组里只取文本块。
fn message_parts(event: &Value) -> Vec<Part> {
    let text = event
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|block| block.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("");
    if text.trim().is_empty() {
        Vec::new()
    } else {
        vec![Part::Text(text)]
    }
}

/// `reasoning` 事件：正文在 `rawContent`（`content` 常为空数组）。
fn reasoning_parts(event: &Value) -> Vec<Part> {
    let text = ["rawContent", "content"]
        .iter()
        .find_map(|key| {
            let joined = event
                .get(key)
                .and_then(Value::as_array)?
                .iter()
                .filter_map(|block| block.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n");
            (!joined.trim().is_empty()).then_some(joined)
        })
        .unwrap_or_default();

    if text.trim().is_empty() {
        Vec::new()
    } else {
        vec![Part::Thinking(text)]
    }
}

fn function_call(event: &Value) -> Option<ToolCall> {
    let id = event.get("callId").and_then(Value::as_str)?;
    let name = event.get("name").and_then(Value::as_str).unwrap_or("tool");
    let input = event
        .get("arguments")
        .and_then(Value::as_str)
        .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
        .unwrap_or(Value::Null);
    Some(ToolCall::new(id, name, input))
}

/// 把 `function_call_result` 的输出 / 状态回填到调用上。
fn fill_result(tool: &mut ToolCall, event: &Value) {
    tool.status = event
        .get("status")
        .and_then(Value::as_str)
        .map(str::to_string);

    let text = result_text(event);
    if tool.status.as_deref() == Some("error") {
        tool.error = text;
    } else {
        tool.output = text;
    }
}

fn result_text(event: &Value) -> Option<String> {
    // 最常见：`output: {type: "text", text: "..."}`。
    if let Some(text) = event.pointer("/output/text").and_then(Value::as_str)
        && !text.trim().is_empty()
    {
        return Some(text.to_string());
    }
    // 兜底：providerData 里同一份内容。
    if let Some(text) = event
        .pointer("/providerData/toolResult/content")
        .and_then(Value::as_str)
        && !text.trim().is_empty()
    {
        return Some(text.to_string());
    }
    // 非文本 output（图片 / 结构化结果）留一个 JSON 快照，别静默丢掉。
    let output = event.get("output")?;
    if output.is_null() {
        return None;
    }
    serde_json::to_string_pretty(output).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn event(value: Value) -> Value {
        value
    }

    #[test]
    fn user_and_assistant_turns_are_grouped() {
        let dump = json!({
            "file": "x.jsonl",
            "events": [
                event(json!({"type": "session-meta"})),
                event(json!({"type": "message", "role": "user",
                             "content": [{"type": "input_text", "text": "看看状态"}]})),
                event(json!({"type": "reasoning",
                             "rawContent": [{"type": "reasoning_text", "text": "先跑 git"}]})),
                event(json!({"type": "function_call", "callId": "c1", "name": "Bash",
                             "arguments": "{\"command\":\"git status\"}"})),
                event(json!({"type": "function_call_result", "callId": "c1", "name": "Bash",
                             "status": "completed", "output": {"type": "text", "text": "clean"}})),
                event(json!({"type": "message", "role": "assistant",
                             "content": [{"type": "output_text", "text": "没有改动"}]})),
            ],
        });

        let messages = from_dump(&dump);
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, Role::User);
        assert_eq!(messages[1].role, Role::Assistant);
        // 推理 + 工具（含结果）+ 正文都在同一条助手消息里，顺序保持事件顺序
        assert_eq!(messages[1].parts.len(), 3);
        assert!(matches!(messages[1].parts[0], Part::Thinking(_)));
        let tool = match &messages[1].parts[1] {
            Part::Tool(tool) => tool,
            other => panic!("expected tool, got {other:?}"),
        };
        assert_eq!(tool.name, "Bash");
        assert_eq!(tool.input["command"], "git status");
        assert_eq!(tool.output.as_deref(), Some("clean"));
        assert_eq!(messages[1].parts[2], Part::Text("没有改动".into()));
    }

    #[test]
    fn a_new_user_message_starts_a_new_assistant_message() {
        let dump = json!({
            "events": [
                {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "一"}]},
                {"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "答一"}]},
                {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "二"}]},
                {"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "答二"}]},
            ],
        });
        let messages = from_dump(&dump);
        let roles: Vec<Role> = messages.iter().map(|m| m.role).collect();
        assert_eq!(
            roles,
            vec![Role::User, Role::Assistant, Role::User, Role::Assistant]
        );
    }

    #[test]
    fn orphan_result_is_kept() {
        // 截断的 JSONL：结果事件还在，调用事件丢了。
        let dump = json!({
            "events": [
                {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "x"}]},
                {"type": "function_call_result", "callId": "gone", "name": "Bash",
                 "status": "completed", "output": {"type": "text", "text": "残留输出"}},
            ],
        });
        let messages = from_dump(&dump);
        let tool = match &messages[1].parts[0] {
            Part::Tool(tool) => tool,
            other => panic!("expected tool, got {other:?}"),
        };
        assert_eq!(tool.output.as_deref(), Some("残留输出"));
    }

    #[test]
    fn metadata_events_do_not_create_messages() {
        let dump = json!({
            "events": [
                {"type": "ai-title"},
                {"type": "summary"},
                {"type": "turn-metrics"},
                {"type": "credit-usage"},
                {"type": "model-usage"},
            ],
        });
        assert!(from_dump(&dump).is_empty());
    }
}
