//! Claude Code 事件流 → Markdown 对话模型。
//!
//! **本阶段尚未实现归并逻辑**（见 `lib.rs` 模块注释：`assistant` 事件按
//! `message.id` 拆分，一条消息可能占几十行）。这里先返回空列表，保证
//! `markdown()` 不 panic 且输出带正确的元信息头；归并规则在下一阶段补齐：
//!
//! - 同 `message.id` 的连续 `assistant` 行归并为一条消息；
//! - `user` 事件里只含 `tool_result` 的不新开 `## User`，回填到对应工具调用；
//! - `attachment` / `api-request*` / `mode` 等运行时状态跳过。

use aa_session_core::markdown::Message;
use serde_json::Value;

/// 从 [`crate::ClaudeSource::dump`] 的结果解析出对话。
pub fn from_dump(_dump: &Value) -> Vec<Message> {
    Vec::new()
}
