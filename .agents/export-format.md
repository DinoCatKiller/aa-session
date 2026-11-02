# 导出格式

`show` 与 `export` 都有 `--format json|markdown`，默认 `json`。两种格式不是
互相替代：**JSON 是给机器和导入用的，Markdown 是给人读的**。

（形状与体积代价写在 [README](../README.md)「导出格式」一节，这里只记改代码前
必须遵守的约束。）

## JSON（默认）：原始记录的无损镜像

- `raw_data` 就是各 source 的 `dump()` 结果：opencode 是四张表原样，codebuddy
  是 JSONL 事件流，zed 是 `columns` + 解压后的 `thread_json`。**不要为了好看
  在 dump 层归一**——归一了就搜不到原文。
- 单会话实测可达 33 MB（主要是文件全文和命令输出）。批量导出前先 `--dry-run`。
- `_index.json` 描述**本次运行的结果**，每次重写，别手改。

## Markdown：版式化的对话

````markdown
# 修复导出

- app: opencode
- session: ses_...
- directory: /home/aa/repos/ide_ls/aa-session
- agent: build
- model: gemma4:31b (ollama-cloud)
- created: 2026-06-07 12:33 UTC
- updated: 2026-06-07 12:39 UTC

## User

把目标语言改成 vi

## Assistant

<thinking>
先看配置文件。
</thinking>

**Tool Use**: read (ID: call_5l2195k5)
Status: completed

```json
{ "filePath": "packages/cli/config.json" }
```

**Tool Result**: read (ID: call_5l2195k5)

<path>...</path>
1: { ... }
````

- 版式参照 zed 自己的线程导出（`crates/agent/src/thread.rs` 与
  `crates/acp_thread/src/acp_thread.rs`）：`## User` / `## Assistant`、
  `<thinking>`、`**Tool Use** / **Tool Result**`。
- 工具参数与输出**不截断**，所以单会话仍可能有几十 MB；它适合阅读和全文
  搜索，不适合当聊天记录转发。
- 渲染逻辑在 `core::markdown`：各 source 只负责把 `dump()` 解析成统一的
  `Message`（opencode v2/v1、codebuddy 事件流、zed 的 `thread_json`），
  `##` 标题、代码围栏、元信息头只写一遍。
- opencode 的 841 个 v2 会话**同时**残留 v1 的 `message` / `part` 行；
  渲染时**只读 v2**（`session_message` 非空即视为 v2），否则同一段对话会
  打印两遍。
- 批量模式的扩展名随之变成 `.md`；`_index.json` **始终是 JSON**，并带
  `format` 字段（索引 `index_version: 2`）。同一目录可以同时放 `.json` 和
  `.md`，但索引只描述**最后一次**运行。
