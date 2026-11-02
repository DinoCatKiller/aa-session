# TODO（待办，优先级低）

> 2026-11-02：`cleanupPeriodDays` 已设为 90（`~/.codebuddy/settings.json`），
> 止损完成。以下两项是 codebuddy 数据源还能挖的增量，等有空再做。

## 1. 从 `~/.codebuddy/traces/` 抢救 09-28 ~ 10-03 的已删会话（片段级）

背景：transcript 被 30 天清理物理删除，但 `traces/` 里的 span 活得更久。

- 现状：`~/.codebuddy/traces/` 共 956 个 trace 文件，span 时间从 **2026-09-28**
  开始；其中 **187 个文件起始日期早于 2026-10-04**（即已删会话时段）。
- 可用的富矿：`generation` span 的 `toolInput` 里塞的是压缩/摘要请求，
  **内嵌了之前的对话原文**（形如 `You are a helpful AI assistant tasked with
  summarizing conversations...` 后跟历史消息）。命中这种 span 的有 **37 个**，
  单个最大约 20 KB。
- 局限（先评估再投入）：
  - span 上**没有** `cwd` / `session.id`（只有 `agent` span 带 `session.id`，
    且只出现在 2026-11-01 的 7 个会话里），无法自动归属到具体会话；
  - `toolInput` 被 `tracePayloadTruncated` 标记截断过，只能拿到片段；
  - 输出只能按时间线导出成文本，由人工认领。
- 做法：写个独立脚本（不必进 aa-session 主仓，或放 `tools/`），把含历史
  原文的 generation span 按 `startedAt` 排序导出到
  `traces-recovered/<日期>/<spanId>.md`，文件头带 `startedAt` / `workerPid` /
  traceId，正文是 `toolInput` + `toolOutput`。先看一眼导出物质量再决定要不要做正式视图。

## 2. codebuddy 子 agent transcript 枚举

背景：codebuddy 把子 agent 存成
`~/.codebuddy/projects/<编码路径>/<session_id>/subagents/agent-<uuid>.jsonl`，
实测 **22 个文件全部含消息**。

- 现状：`sources/codebuddy` 的 `list_session_files` 只扫
  `<project>/*.jsonl` 一层，`subagents/` 目录下的文件永远列不出来；
  `--subagents include|only` 对 codebuddy 不生效（没有 `parent_id` 可解析，
  见 `packages/sources/codebuddy/src/lib.rs:351` 的 `subagent_count: 0`）。
- 修法方向：
  1. 枚举 `<project>/*/subagents/*.jsonl`；
  2. 由文件路径推父会话 id（`<session_id>/subagents/` 那段），映射到
     `SessionMeta` 的父子关系，让 `--subagents` 对 codebuddy 真正生效；
  3. 父会话已被清理时，子 agent 文件应仍能列出并标记父缺失；
  4. 补 `DirectoryMeta::subagent_count` 的 codebuddy 实现（现在是硬编码 0）。
- 验收：`sessions --subagents include` 多列出 22 条，`↳` 挂在父会话下；
  `--subagents only` 只列这 22 条。
