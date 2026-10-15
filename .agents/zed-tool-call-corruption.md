# zed agent 工具调用损坏（tool_call id/name/arguments 丢失）

记录使用 **zed 原版**（非本 fork）时遇到的 agent 会话被非法流式 `tool_calls`
污染的问题，以及用 `aa-session` 导出→修改→导入的恢复流程。

> 本文件只做**案例登记与线索**。详细分析、复现实验放在本仓库（`aa_session`）做，
> 因为它能直读/导出各 app 的原始会话、并且已有 zed 的 `import` 写回路径。

## 背景：用的是 zed 官方原版，非 fork

本文件记录的 bug 均来自 **zed 官方原版**（1.23.x / 1.24.x），非任何 fork。

本地 fork 尚未完备，其 `open_ai` 仍是旧实现（`ToolCallChunk.index: u32` 必填、
`ResponseStreamResult` untagged 解析），症状与官方版不同类，等 fork 对齐后再单独看。
本文件只针对官方版。

## 上游 issue

- **[zed#65655](https://github.com/zed-industries/zed/issues/65655)**
  `agent: empty tool_call id (and name) from streamed responses is persisted,
  breaking every subsequent request`（我们提交的）

## 案例登记

### 案例 A —— id 与 name **都为空**（已记录，已提 issue）

- 现象：provider 流式返回的 `tool_calls` delta 从不带可用 `id` / `function.name`
  （每片 `index: -1`、无 id、无 name，只有 `arguments`）。
- zed drain 出 `ToolUse { id: "", name: "" }` → agent 按空名查找失败 →
  `"No tool named  exists"` 落库 → 下一轮把空 id/name/tool_call_id 发给 provider → 400。
- 重试只是重发同一份 messages，永久卡死。
- 证据：issue #65655 里含完整 payload + provider 400 原文。

### 案例 B —— 仅 id 为空（name 有效）

- provider 报「没有 id」。工具本身可能已跑通，坏的是重放的 `tool_call_id`。
- **恢复方式**：导出会话 → 手动伪造一个非空且前后一致的 `id`（assistant tool_call
  与 tool result 用同一个）→ 导入。会话恢复正常。
- 观察：`id` 像是个**关联标识**，只需「存在且一致」，值本身似无格式要求。

### 案例 C —— **参数丢失**（arguments lost，未详细记录）

- 现象：工具调用的 `arguments` 丢失。
- **恢复方式**：导出会话 → 把该会话**最后一条消息裁掉** → 导入。会话恢复正常。
- 具体情况当时**没记录**。疑似与 `arguments` 的分片累加逻辑有关，根因可能独立于 A/B。
- **下次遇到要做**：
  1. 损坏发生时**先导出原始会话**存档（`aa-session --app zed export <id> -o ...`），
     再动任何东西；
  2. 记录：zed 版本 + commit（`bun tools/gh-releases.ts zed-industries/zed`）、
     provider 名称、完整流式返回、`aa-session show` 的原始记录；
  3. 做复现实验（可改造/伪造 SQLite 里被 zstd 压缩的 thread blob）。

## 恢复流程（通用）

1. `aa-session --app zed sessions --project-like <片段>` 定位会话；
2. `aa-session --app zed export <session_id> -o /tmp/zed_export.json` 导出（无损 raw）；
3. 编辑 JSON（伪造 id / 裁剪末条消息 / 改 arguments）；
4. `aa-session --app zed --db <threads.db> import /tmp/zed_export.json`（预检）→
   加 `--apply` 写回（写前自动 `VACUUM INTO` 备份）。

> 注意：目前 **只有 zed** 实现了 `import`；opencode / codebuddy 会报「暂不支持」。
> 若 Z 类问题出现在 opencode 上，需要先补 `SessionImporter`。

## 相关上游代码位置（1.23.3 / 1.24.2 相同）

- `crates/language_model_core/src/chat_completion.rs:541`（drain 未过滤空 id/name；
  对比 `:517` 的流式发射有守卫）
- `crates/agent/src/thread.rs:3597`（按名查工具）、`:3610`（`No tool named` 结果）
- `crates/open_ai/src/completion.rs:193-196`（回放时 `tool_call_id: ""`）

## 下一步

- [ ] 案例 C（参数丢失）：等复现，按上面「下次遇到要做」取证后详析。
- [ ] 关注 #65655 上游进展；若合并，核对是否覆盖案例 B。
- [ ] 评估是否给 opencode / codebuddy 也实现 `import`（便于在它们上面做同类修复）。
