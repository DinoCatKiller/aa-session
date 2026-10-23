//! zed 的 `thread_json.messages` → Markdown 对话模型。
//!
//! zed 的线程是一整包 JSON（`threads.data` 解压后），`messages` 数组的元素
//! 是外部标记的枚举：
//!
//! ```text
//! {"User": {"content": [{"Text": "..."} | {"Mention": {...}} | {"Image": ...}]}}
//! {"Agent": {"content": [{"Text": ...} | {"Thinking": {...}} |
//!                        {"ToolUse": {...}} | {"RedactedThinking": ...}],
//!            "tool_results": {"<id>": {...}}}}
//! "Resume"
//! {"Compaction": {"Summary": "..."} | {"ProviderNative": {...}}}
//! ```
//!
//! `tool_results` 是一个**按 id 索引的映射**，不在 `content` 的顺序里；
//! 这里按 id 回填到对应的 `ToolUse` 上，让「调用 → 结果」在 Markdown 里
//! 紧挨着出现。

use std::collections::HashSet;

use aa_session_core::markdown::{Message, Part, ToolCall};
use serde_json::Value;

/// 从 [`crate::ZedSource::dump`] 的结果解析出对话。
pub fn from_dump(dump: &Value) -> Vec<Message> {
    let messages = dump
        .pointer("/thread_json/messages")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();

    let mut out = Vec::new();
    for message in messages {
        // `"Resume"` 只是「继续」标记，zed 自己的导出也不渲染它。
        let Some(object) = message.as_object() else {
            continue;
        };

        if let Some(user) = object.get("User") {
            let parts = user_parts(user);
            if !parts.is_empty() {
                out.push(Message::user(parts));
            }
        } else if let Some(agent) = object.get("Agent") {
            let parts = agent_parts(agent);
            if !parts.is_empty() {
                out.push(Message::assistant(parts));
            }
        } else if let Some(compaction) = object.get("Compaction")
            && let Some(part) = compaction_part(compaction)
        {
            out.push(Message::system(vec![part]));
        }
    }

    out
}

fn user_parts(user: &Value) -> Vec<Part> {
    let mut parts = Vec::new();
    for content in user
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if let Some(text) = content.get("Text").and_then(Value::as_str) {
            if !text.trim().is_empty() {
                parts.push(Part::Text(text.to_string()));
            }
        } else if let Some(mention) = content.get("Mention") {
            parts.push(Part::Text(render_mention(mention)));
        } else if content.get("Image").is_some() {
            parts.push(Part::Text("<image />".into()));
        }
    }
    parts
}

fn agent_parts(agent: &Value) -> Vec<Part> {
    let mut parts = Vec::new();

    // 先按 content 顺序列出调用，再按 id 回填 result；没有对应调用的
    // 孤立结果留到后面补，避免静默丢内容。
    let mut matched: HashSet<String> = HashSet::new();
    for content in agent
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if let Some(text) = content.get("Text").and_then(Value::as_str) {
            if !text.trim().is_empty() {
                parts.push(Part::Text(text.to_string()));
            }
        } else if let Some(thinking) = content.get("Thinking") {
            if let Some(text) = thinking.get("text").and_then(Value::as_str)
                && !text.trim().is_empty()
            {
                parts.push(Part::Thinking(text.to_string()));
            }
        } else if content.get("RedactedThinking").is_some() {
            parts.push(Part::Text("<redacted_thinking />".into()));
        } else if let Some(tool_use) = content.get("ToolUse") {
            let mut tool = tool_use_parts(tool_use);
            if let Some(result) = tool_result_for(agent, &tool.id) {
                fill_result(&mut tool, result);
                matched.insert(tool.id.clone());
            }
            parts.push(Part::Tool(tool));
        }
    }

    // 结果映射里有、但 content 里没有对应调用的条目。
    if let Some(results) = agent.get("tool_results").and_then(Value::as_object) {
        for (id, result) in results {
            if matched.contains(id) {
                continue;
            }
            let mut tool = ToolCall::new(id.clone(), result_name(result), Value::Null);
            fill_result(&mut tool, result);
            parts.push(Part::Tool(tool));
        }
    }

    parts
}

fn tool_use_parts(tool_use: &Value) -> ToolCall {
    let id = tool_use
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let name = tool_use
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("tool");
    let input = tool_input(tool_use);
    ToolCall::new(id, name, input)
}

/// zed 的 `input` 是 `LanguageModelToolInput` 包装：
/// `{"type": "json", "value": {...}}` / `{"type": "text", "value": "..."}`。
/// 拆出内层 `value`；没有包装时原样返回，最后才退回 `raw_input` 字符串。
fn tool_input(tool_use: &Value) -> Value {
    let raw = tool_use.get("input");
    match raw {
        Some(Value::Object(map))
            if map.contains_key("value")
                && matches!(
                    map.get("type").and_then(Value::as_str),
                    Some("json" | "text")
                ) =>
        {
            map.get("value").cloned().unwrap_or(Value::Null)
        }
        Some(value) if !value.is_null() => value.clone(),
        _ => tool_use
            .get("raw_input")
            .and_then(Value::as_str)
            .and_then(|raw| serde_json::from_str(raw).ok())
            .unwrap_or(Value::Null),
    }
}

fn tool_result_for<'a>(agent: &'a Value, id: &str) -> Option<&'a Value> {
    agent.get("tool_results")?.get(id)
}

fn result_name(result: &Value) -> &str {
    result
        .get("tool_name")
        .and_then(Value::as_str)
        .unwrap_or("tool")
}

fn fill_result(tool: &mut ToolCall, result: &Value) {
    tool.status = Some(
        if result
            .get("is_error")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            "error".to_string()
        } else {
            "completed".to_string()
        },
    );

    let text = result_text(result);
    if tool.status.as_deref() == Some("error") {
        tool.error = text;
    } else {
        tool.output = text;
    }
}

fn result_text(result: &Value) -> Option<String> {
    let texts: Vec<&str> = result
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|block| block.get("Text").and_then(Value::as_str))
        .collect();
    if !texts.is_empty() {
        return Some(texts.join("\n"));
    }

    // `output` 是 zed 的 debug 输出：实测常与 content 重复，只在 content
    // 为空时兜底，避免同一段结果打印两遍。
    match result.get("output")? {
        Value::String(text) if !text.trim().is_empty() => Some(text.clone()),
        value if !value.is_null() => serde_json::to_string_pretty(value).ok(),
        _ => None,
    }
}

/// 用户消息里的 `@` 引用：名字 + 被引用的原文。
///
/// zed 导出的是 `[@name](zed://…)` 链接；这里不构造 URI（路径里的空格 /
/// 中文需要转义，转错了反而更糟），只保留人能读、能搜索的文本。
fn render_mention(mention: &Value) -> String {
    let label = mention
        .get("uri")
        .map(mention_label)
        .unwrap_or_else(|| "mention".to_string());
    let content = mention
        .get("content")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim_end();

    if content.is_empty() {
        format!("@{label}")
    } else {
        format!("@{label}\n\n{content}")
    }
}

fn mention_label(uri: &Value) -> String {
    let Some(object) = uri.as_object() else {
        return uri.as_str().unwrap_or("mention").to_string();
    };
    let Some((kind, body)) = object.iter().next() else {
        return "mention".to_string();
    };

    let path = |key: &str| {
        body.get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let with_range = |base: String| -> String {
        match body.get("line_range") {
            Some(range) => {
                let start = range.get("start").and_then(Value::as_u64).unwrap_or(0) + 1;
                let end = range.get("end").and_then(Value::as_u64).unwrap_or(0) + 1;
                format!("{base}:{start}-{end}")
            }
            None => base,
        }
    };

    match kind.as_str() {
        "File" => path("abs_path"),
        "Directory" => format!("{}/", path("abs_path").trim_end_matches('/')),
        "Selection" => with_range(path("abs_path")),
        "Symbol" => format!("{} ({})", path("name"), with_range(path("abs_path"))),
        "Thread" => path("name"),
        "PastedImage" => path("name"),
        "Skill" => path("name"),
        "Fetch" => path("url"),
        other => other.to_string(),
    }
}

fn compaction_part(compaction: &Value) -> Option<Part> {
    if let Some(summary) = compaction.get("Summary").and_then(Value::as_str) {
        return Some(Part::Text(format!(
            "**Context Compaction (Completed)**\n\n{}",
            summary.trim_end()
        )));
    }
    if compaction.get("ProviderNative").is_some() {
        return Some(Part::Text("**Context Compaction (Completed)**".to_string()));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use aa_session_core::markdown::Role;
    use serde_json::json;

    #[test]
    fn user_text_and_mention_are_rendered() {
        let dump = json!({
            "thread_json": {"messages": [
                {"User": {"id": "u1", "content": [
                    {"Text": "看这段"},
                    {"Mention": {
                        "uri": {"Selection": {
                            "abs_path": "/tmp/a.rs",
                            "line_range": {"start": 2, "end": 4}
                        }},
                        "content": "fn main() {}"
                    }},
                    {"Image": {"data": "..."}},
                ]}},
            ]},
        });

        let messages = from_dump(&dump);
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].role, Role::User);
        assert_eq!(messages[0].parts[0], Part::Text("看这段".into()));

        let Part::Text(mention) = &messages[0].parts[1] else {
            panic!("expected mention text");
        };
        // 行号从 0 基转 1 基，与 zed 的导出一致
        assert!(mention.contains("@/tmp/a.rs:3-5"), "{mention}");
        assert!(mention.contains("fn main() {}"), "{mention}");
        assert_eq!(messages[0].parts[2], Part::Text("<image />".into()));
    }

    #[test]
    fn agent_content_keeps_order_and_attaches_results() {
        let dump = json!({
            "thread_json": {"messages": [
                {"Agent": {
                    "content": [
                        {"Thinking": {"text": "先看 git", "signature": null}},
                        {"Text": "我来看看"},
                        {"ToolUse": {
                            "id": "call_1", "name": "terminal",
                            "raw_input": "{\"command\":\"git status\"}",
                            "input": {"type": "json", "value": {"command": "git status"}}
                        }},
                    ],
                    "tool_results": {
                        "call_1": {
                            "tool_use_id": "call_1", "tool_name": "terminal",
                            "is_error": false,
                            "content": [{"Text": "clean"}],
                        }
                    }
                }},
            ]},
        });

        let messages = from_dump(&dump);
        assert_eq!(messages[0].role, Role::Assistant);
        assert_eq!(messages[0].parts.len(), 3);
        assert_eq!(messages[0].parts[0], Part::Thinking("先看 git".into()));
        let tool = match &messages[0].parts[2] {
            Part::Tool(tool) => tool,
            other => panic!("expected tool, got {other:?}"),
        };
        assert_eq!(tool.name, "terminal");
        // 包装层被拆掉，只留真正的参数
        assert_eq!(tool.input["command"], "git status");
        assert_eq!(tool.output.as_deref(), Some("clean"));
        assert_eq!(tool.status.as_deref(), Some("completed"));
    }

    #[test]
    fn error_result_is_marked_as_error() {
        let dump = json!({
            "thread_json": {"messages": [
                {"Agent": {
                    "content": [{"ToolUse": {
                        "id": "c", "name": "read",
                        "input": {"type": "json", "value": {"path": "missing"}}
                    }}],
                    "tool_results": {"c": {
                        "tool_use_id": "c", "tool_name": "read", "is_error": true,
                        "content": [{"Text": "No such file"}],
                    }}
                }},
            ]},
        });
        let messages = from_dump(&dump);
        let tool = match &messages[0].parts[0] {
            Part::Tool(tool) => tool,
            other => panic!("expected tool, got {other:?}"),
        };
        assert_eq!(tool.error.as_deref(), Some("No such file"));
        assert!(tool.output.is_none());
    }

    #[test]
    fn resume_is_skipped_and_compaction_kept() {
        let dump = json!({
            "thread_json": {"messages": [
                "Resume",
                {"Compaction": {"Summary": "前面聊了导出格式"}},
            ]},
        });
        let messages = from_dump(&dump);
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].role, Role::System);
        let Part::Text(text) = &messages[0].parts[0] else {
            panic!("expected text");
        };
        assert!(text.contains("Context Compaction"), "{text}");
        assert!(text.contains("前面聊了导出格式"), "{text}");
    }

    #[test]
    fn tool_input_falls_back_to_raw_input_string() {
        let dump = json!({
            "thread_json": {"messages": [
                {"Agent": {
                    "content": [{"ToolUse": {
                        "id": "c", "name": "terminal",
                        "raw_input": "{\"command\":\"ls\"}"
                    }}],
                    "tool_results": {}
                }},
            ]},
        });
        let messages = from_dump(&dump);
        let tool = match &messages[0].parts[0] {
            Part::Tool(tool) => tool,
            other => panic!("expected tool, got {other:?}"),
        };
        assert_eq!(tool.input["command"], "ls");
    }

    #[test]
    fn orphan_tool_result_is_not_dropped() {
        let dump = json!({
            "thread_json": {"messages": [
                {"Agent": {
                    "content": [],
                    "tool_results": {"lost": {
                        "tool_use_id": "lost", "tool_name": "terminal",
                        "is_error": false, "content": [{"Text": "残留输出"}]
                    }}
                }},
            ]},
        });
        let messages = from_dump(&dump);
        let tool = match &messages[0].parts[0] {
            Part::Tool(tool) => tool,
            other => panic!("expected tool, got {other:?}"),
        };
        assert_eq!(tool.output.as_deref(), Some("残留输出"));
    }
}
