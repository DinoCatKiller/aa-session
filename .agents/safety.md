# 安全边界与禁区

## 🚫 硬约束：不写 opencode 库

**绝不对 `opencode.db` 做写操作。** 该库正被运行中的 opencode 进程持有，
写入会与其内存状态冲突并可能撞 WAL 锁。

具体地：

- **不做 import**。将来若要做，**必须默认先备份目标库**。
- **不在运行中的 opencode 库上测试任何写操作**。
- 要改内容 → 导出 JSON → 用户在编辑器里改。

（曾有一次在本项目里跑 `opencode import` 做往返测试，往真实库里写进了一个
空壳 session。虽然只污染了 v1 旧表的 1 行、不影响 891 个真实会话，但这是
不该发生的——测试写操作前必须先问用户。）

> 现状更新：`rename-project` 已经是唯一会写 opencode 库的子命令，默认只预检，
> 加 `--apply` 才写，且写前强制 `VACUUM INTO` 备份。上面这条「绝不做写操作」
> 的原始语境是**没有备份路径之前**；新增任何写路径仍须遵守「默认预检 + 写前
> 备份 + 不手改运行中进程的状态」。

## 与 AAgent / zed 的关系

**零依赖，零复用。** 已核实：

- `packages/search`（16637 行）依赖 `editor`/`workspace`/`picker`，是 zed 面板不是库。
  唯一干净的是 `packages/project/src/search_history.rs`（128 行，纯 `std::VecDeque`，
  与 zed 逐字节相同）——将来 GUI 要搜索框翻历史时再抄。
- `packages/db` 是 zed 应用数据库框架（sqlez + `gpui::Global` + `inventory` 链接期
  migration），与本项目无关，**不用**。
- `packages/agent/src/db.rs:450` 把整个 thread 压成 zstd blob，没有可检索的行式存储。
  aacode 全仓库 0 处 FTS5/MATCH/LIKE，只在 `thread_search_bar.rs` 做内存线性扫描
  当前打开的 thread。
- `agent-client-protocol`（crates.io `=2.2.0`）是干净的公共 crate（9 个依赖，零 gpui），
  其 `ContentBlock` 的**分块思路**被 `core::ContentBlock` 借鉴，但 v1 不直接依赖它：
  opencode.db 的 JSON 是自己的格式，不是 ACP。
- ACP 已定义 `session/fork`（`unstable_session_fork`），但**只接受 `session_id`，
  没有边界 / seq 参数**——「从某个位置 fork」正是 ACP 的空白区。
  opencode 的 `session_v2` 虽有 `fork_session_id` / `fork_boundary` 字段，
  实测**全库 0 条使用**（652 条 `parent_id` 是子 agent 派生，不是 fork）。
