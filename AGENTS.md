# aa-session

列出并导出本地 agent 会话。**v1 只做只读列举 + JSON 导出。**

## 定位：为什么不做搜索

核心场景已经定了——**全文检索交给用户自己的编辑器**。我们负责把会话导出成
可读 JSON，用户在 Zed 里搜。理由：

- 不必维护索引（曾建过 SQLite FTS5 侧车，470 MB 重复数据、永远可能过期、
  还要管 schema 版本；删掉后 891 个会话的列举是一次 SQL 查询的事）；
- 不必猜用户想要什么样的检索——Zed / IDE 的搜索比我们的 FTS 强得多；
- 导出物是**给人读的**：每条消息一个对象，`text` 字段直接是裸文本，
  Zed 搜「音频解码」能直接命中字符串本身。

代价：找东西要先知道大致在哪个会话（或先导出再搜）。这是明确的取舍。

## 命令

`--app` **必填**（放在子命令前后都行），不猜目标 app。

```bash
aa-session --app opencode projects   [--min-sessions N] [--json]
aa-session --app opencode directories [--directory SUB] [--min-sessions N] [--json]
aa-session --app opencode sessions   [--project ABS_PATH | --project-like SUB]
                                    [--directory SUB] [--agent NAME]...
                                    [--title SUB] [--since MINUTES]
                                    [--subagents exclude|include|only]
                                    [--archived exclude|only|include]
                                    [--sort updated|created|title|tokens] [--limit N]
                                    [--format table|json|toon]
aa-session --app opencode show   <SESSION_ID> [--format json|markdown]
aa-session --app opencode export [SESSION_ID] [-o PATH] [--format json|markdown]
                                    [--force] [--dry-run] [--all]
                                    [过滤选项同 sessions] [--sort ...] [--limit N]
```

- 表格里**不显示来源列**：`--app` 参数已经说明了是哪个 app。
- `--format` 默认 `table`；`--json` 是 `--format json` 的简写（保留旧用法）。

### `export`：一条命令，单个或批量

「导一个」和「导一批」是同一种操作的两种规模，所以是**同一个子命令**，
区别只在于给不给 `SESSION_ID`：

```bash
# 单个 → -o 是文件
aa-session --app opencode export ses_xxx -o chat.json

# 批量 → -o 是目录（不存在则创建）
aa-session --app opencode export --directory LocalDub -o exports/LocalDub/opencode
aa-session --app opencode export --project /home/aa/repos/ai_ls/AAgent -o exports/AAgent/opencode
```

单会话模式下给筛选参数会**直接报错**（`--title` 之类在单会话里没有意义，
静默忽略会让人以为筛过）。

批量模式：

- **筛选条件与 `sessions` 完全一致**（`--project` / `--project-like` /
  `--directory` / `--agent` / `--title` / `--since` + 三个 include 模式），
  两者共用 `query_sessions()`，不会一边修了边界、另一边忘了。
- **必须至少给一个筛选条件**，或者显式 `--all`；`--all` 与筛选条件互斥。
  这条护栏挡的是「一条手滑命令把整个库（891 个会话、几十 GB）写进当前目录」。
- 默认仍是**顶层 + 未归档 + 非空壳**；子 agent 的探查会话要显式
  `--subagents include` 才会导出。
- `--limit` **缺省 = 不限制**；`--limit 0` 是字面意义的「一条都不要」
  （用 `Option<usize>` 区分「没给」和「给了 0」，不再需要「0 表示全部」这种
  反直觉约定）。底层用 `core::NO_LIMIT`：`usize::MAX` 转成 `i64` 是 `-1`，
  SQLite 的负数 `LIMIT` 正是不限，`Vec::truncate` 也是无操作。
- `--format json|markdown`（默认 `json`）：决定文件扩展名（`.json` / `.md`）
  与正文格式。Markdown 见下方「导出格式」一节。
- 文件名 `<创建日期>_<标题 slug>_<id 前 12 位>.<ext>`：日期在前可按时间自然
  排序，标题让人在文件树里认出对话，id 段保证不互相覆盖。标题里的空格 /
  斜杠 / emoji 折叠成 `-`；**id 段单独消毒**，不能靠「数据库里的 id 应该
  没怪字符」——`/` 会写到目录之外。
- 附一个 `_index.json`：筛选条件快照 + 每条会话的 id / 标题 / 路径 /
  token / 字节数 / `pre_existing`。它是**本次运行的结果**，每次重写，别手改。
- 默认**跳过**已存在的同名文件，所以能反复重跑补增量；`--force` 才覆盖。
  文件名里的日期取自**创建时间**，会话后续更新不改名，刷新内容必须 `--force`。
- 单条会话失败**不中断整批**，其余照常写入，最后以非零退出码报告失败数。
- `--dry-run` 只打印清单（文件名 + 目标目录），不创建目录也不写盘；
  单会话模式下同样可用，且不会 dump 内容（先校验 id 存在）。

不合并成单个大文件，是因为：编辑器每次命中都落在具体会话上；增量重跑只写
新增；每份导出物本身就是单会话导出格式，将来要 `import` 不用再拆。

### 项目寻址：`--project` 精确，`--project-like` 模糊

| 参数             | 语义                            | 输入                 | 多命中时                            |
| ---------------- | ------------------------------- | -------------------- | ----------------------------------- |
| `--project`      | **全等**匹配 `project.worktree` | 只接受**绝对路径**   | 永不歧义（自动聚合同路径的多个 id） |
| `--project-like` | **子串**匹配路径                | 任意子串（含目录名） | 正常全列，不报错                    |

两者互斥，同时给会报错。三条实现约束：

1. **不做 `canonicalize()`**。实测 12 个项目的目录已从磁盘删除，其下仍有
   **244 条会话**；`canonicalize` 会因路径不存在而失败，让这批会话永远查不到。
   只做去尾部 `/` 和展开 `~` 这类**字符串层面**的规范化。
2. **同路径多 id 必须聚合**。46 个项目行只有 45 个不同 worktree ——
   `Nahida-aa.github.io` 有**两个独立 id**（7 + 11 条会话）。
3. **`/` 是占位符不是真实路径**，显示为「（未知）」。opencode 为「无法确定所属
   仓库」的会话建了名为 `global` 的项目、worktree 填 `/`，那 10 条顶层会话散落
   在多个目录。用 `is_unknown_path()` 判断，别直接和 `"/"` 字符串比较。

### 三种输出格式，各司其职

| 格式 | 解决什么 | 谁看 |
|---|---|---|
| `table`（默认） | 视觉对齐，纵向扫一列对比数字最省力 | 人眼 |
| `toon` | 省 token：表头写一次键名，行内只写值 | AI / token 敏感 |
| `json` | 通用管道（`jq`）、生态兼容 | 脚本、既有工作流 |

**TOON 不是表格的替代品**（Token-Oriented Object Notation，给 LLM 用的紧凑
序列化）。它输出人也能读，但**列不对齐**，扫一列比分隔符列表费劲；反过来表格
对 AI 极不友好——数字带千分位和前后空格。两者解的是不同问题。

实测 30 条会话：`json 4366` → `toon 2267`，**省 48%**（省的是重复的键名；
字段越长越散省得越少）。`table 1189` 看着更小，但它是**有损的**——路径和 id
被截断过，不参与这个比较。

⚠️ `toon-format` 的 `default = ["cli"]` 会拖进 ratatui / crosstrm /
tui-textarea / arboard / syntect（那是它自己交互式 TUI 编辑器的依赖）。
已用 `default-features = false` 只取编解码库。
⚠️ 直接加它会和 comfy-table 抢 `unicode-width`：ratatui 钉 `=0.2.0`、
comfy-table 要 `0.2`。我们的直接依赖必须写 `"0.2"` 而不是 `"0.2.2"`。

### 表格输出：用 comfy-table，一条记录一行

**每条会话只占一行**，路径各自独立成列：

```
 会话 ID         时间               tokens  agent    成本  标题                    会话目录                   项目目录
 ses_f04a8780…  2026-10-02 06:40       0  build     -  New session - 2026-…   /home/aa/repos/ai_ls/…    /home/aa/repos/ai_ls/ocr-lab
↳ses_fc6bb114…  2026-08-25 14:14    4.2万  explore   -  Investigate drag a…   /home/aa/repos/ide_ls/…
```

- 用 [`comfy_table`]（8.x，`tty` feature）而非手写 `{:>6}`：**Rust 的
  `str::len()` 按字节算**，中文标题必然导致列错位；comfy-table 用
  `unicode-width` 按显示列宽算，并处理终端宽度自适应。手写的 `{:>N}` 在这里
  是错的，不是「不够优雅」。
- 样式用 `presets::NOTHING`：只留表头下横线。891 行画 891 个框，噪声压过信息。
- **路径从左侧截断**（`…/env_ls/LocalDub`）：这些路径共享很长的前缀，尾部才是
  区分信息；砍尾部会让所有行看起来一样。标题相反，从**右侧**截断。
- 会话 id 只显示前 12 位（`table::short_id`），全量打印会撑宽 28 列。
- `↳` 前缀标记子会话，`⊘` 标记归档。

### `sessions` 里两个目录都要显示

- `会话目录` = `session_v2.directory`（运行目录），**总是显示**；
- `项目目录` = `project.worktree`（git 仓库根），相同时写「**同左**」。
  默认口径下有 **28 条**不一致（跑在子目录 / 跑在另一个仓库 / `global` 兜底桶）。

  ⚠️ 之前这里**留空**，是错的：空单元格看起来像「数据缺失」，而不是
  「和左边一样」。已改为显式的「同左」。

`/` 兜底桶在 `projects` 和 `sessions` 两个视图里都显示成「（未知）」，判断逻辑
收敛到 `ProjectMeta::is_unknown_path` / `SessionMeta::is_unknown_project`，共用
`UNKNOWN_PROJECT_PATH` 常量（有单测盯住，防止两个视图对同一兜底桶显示不一致）。

### `projects` 与 `directories` 是两个视图，不是重复

**会话的运行目录（`session_v2.directory`）和它归属的项目（`project.worktree`）
是两件独立记录的事**，实测三类不一致：

| 现象                               | 实测                                                     |
| ---------------------------------- | -------------------------------------------------------- |
| 会话跑在项目仓库根的**子目录**     | 如 `/home/aa/repos/mc_ls/mcc/apps/web`                   |
| 会话跑在**另一个仓库**却归到原项目 | aaBot 有 5 条跑在 AAgent 目录                            |
| 同路径两个 project id              | `Nahida-aa.github.io` 在 `directories` 里显示 `项目数=2` |

所以「这个 agent 在哪工作过」只能用 `directories` 回答。`项目数` 列大于 1
即表示该目录混入了不属于本项目的会话。

### opencode 的三个路径字段（极易混淆，改代码前先看）

| 字段                   | 层级   | 含义             | 数据质量                                                                 |
| ---------------------- | ------ | ---------------- | ------------------------------------------------------------------------ |
| `project.worktree`     | 项目级 | git 仓库根       | 45/45 是绝对路径。`vcs` 列是**建项目时的快照**——目录删了也照样显示 `git` |
| `session_v2.directory` | 会话级 | 会话实际运行目录 | 891 行全绝对路径                                                         |
| `session_v2.path`      | 会话级 | 残留字段         | 仅 37 条有值且形态不一致（有缺首斜杠的），**直接忽略**                   |

`project.id` 是 40 字符哈希，**不等于路径**（实测 `id = sha1(path)` 不成立，
全库 0 行满足 `id = worktree`）。对外只用路径寻址，id 仅作内部 join 键。

### CLI 输出必须容忍 broken pipe

`println!` 遇写错误会 `panic!`，于是 `aa-session ... | head -5` 会炸出
`failed printing to stdout: Broken pipe`。已统一改用 `output::out()`：忽略写错误，
并**手动补换行**（`format_args!` 不像 `println!` 那样自动加换行）。

> `--app` 在 clap 里是「全局但可选」，靠代码校验必填。原因是 clap 的
> `global = true` + `required = true` 组合在子命令前后**都解析不到**，只会报一句
> 没有信息量的 "required arguments were not provided"；手动校验才能提示可用值。

### 默认只看「顶层 + 未归档」

两个维度都默认隐藏，都用显式开关放出来：

| 维度          | 现象                                                                                                                                                                                                                        | 默认      | 开关               |
| ------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | --------- | ------------------ |
| `--subagents` | opencode 把主 agent 派出的探查任务记成独立会话（`parent_id` 非空）。本库 891 个会话里 **652 个是子会话**（607 个是 `explore`），只有 239 个顶层；它们标题雷同、目录相同、各自几万 token，混在列表里会淹没真正想找的顶层会话 | `exclude` | `include` / `only` |
| `--archived`  | 用户主动收起的会话（`time_archived` 非空）。本库 17 条，其中 15 条在 YouDub-webui、1 条在 mcc                                                                                                                               | `exclude` | `include` / `only` |

`projects` 的三个计数列各自对齐口径（顶层 / 子会话 / 归档），三者互不重叠且都排除了
另一维度的过滤——否则 `projects` 显示 274 而同一项目 `sessions` 只列出 44 条，
用户会以为哪里坏了。已用一致性校验过（LocalDub: 44=44、YouDub-webui: 21=21）。

### 会话列表显示的字段

时间、tokens、agent、成本、标题/目录，附注行给子会话父 id、归档时间、改动文件数、
推理 token。

- **成本**：opencode 只对少数会话维护 `cost`（实测 891 条里仅 5 条非零），
  所以 `None`（显示 `-`）表示「上游没记录」，与「确实是 0 美元」严格区分。
- **`SessionMeta` 没有 `Eq`**：`cost` 是 `f64`，浮点不满足 `Eq`。为了显示真实成本
  放弃了整个类型的 `Eq` 绑定（本项目不需要把它当 `HashSet` key，可接受）。

## 日常操作用 justfile

`just`（https://github.com/casey/just）封装了常用命令，`just` 可列出全部配方：

| 配方                       | 用途                                                        |
| -------------------------- | ----------------------------------------------------------- |
| `just projects [过滤]`     | 列出项目（等价 `aa-session --app opencode projects`）       |
| `just sessions [过滤]`     | 列出会话                                                    |
| `just dump <ID> [路径] [格式]` | 导出会话，不给路径时落到 `exports/<ID>.<ext>`；格式默认 `json` |
| `just dump-dir <目录子串> [目录] [格式]` | 按目录批量导出，默认落到 `exports/<目录子串>/opencode[-markdown]/` |
| `just zed <ID> [路径]`     | 导出后直接用 Zed 打开                                       |
| `just find <模式>`         | 在 `exports/` 里全文搜索（等价于 Zed 的 cmd+f，但可脚本化） |
| `just ci`                  | `fmt --check` + `clippy` + `test`，提交前跑这个             |
| `just run -- <参数>`       | 透传参数给二进制                                            |

`exports/` 已 gitignore（单个导出可达数百 MB）。要用别的目录：
`AA_SESSION_EXPORTS=/path just find <模式>`。

### justfile 的两个坑（都踩过）

1. **just 只做文本替换，不创建同名 shell 变量。** 想用 shell 的默认值展开
   `${out:-...}`，必须先 `out="{{ out }}"` 赋值；直接写 `${out:-...}` 会**永远取
   默认值**——用户传了输出路径也不生效。同理 `$session_id` 也得先赋值，否则
   `set -u` 下报「未绑定的变量」。
2. **替换发生在 bash 看到脚本之前。** 插值进 shell 的参数一律用 `{{ quote(...) }}`
   转义，否则 `just find 'a$b'` 会被 bash 当变量、`just find '`id`'` 会被当命令
   **执行**。

另外：`export` 作为 recipe 名会让部分编辑器的 justfile 语法插件误报语法错误
（just 自身解析正常），故改名为 `dump`。

## 数据源

v1 **只有 opencode**，直读 `~/.local/share/opencode/opencode.db`（约 6 GB）。

- 读 `session_v2` / `session_message` / `project`（v2 schema）。
- v1 的 `session` / `message` / `part` **不读**——两套都在同一库里，但 v2 才是当前 schema。
- 以 SQLite `mode=ro` 打开，**永不写入**。

### 为什么直读而不是 `opencode serve` HTTP API

opencode 提供 `opencode serve`（`GET /session` 等）。不用它：

1. **它要求 opencode 进程在跑**，而核心场景恰恰是关着 opencode 时翻查历史；
2. 取全部消息正文要逐个 session 发 HTTP（891 个会话 / 20 万条消息），
   本地一次 SQL 即可；
3. 写路径本来就不该碰——那正是与运行中进程抢状态。

### 关于 `app`

`SessionSource::app()` 标识数据由哪个 agent 应用产生（opencode / claude-code /
codex）。它**同时是「来源」和「目标」**——将来 fork 时「从 app A 的会话 fork 出
app A 的新会话」用的是同一个字段。不要叫 `source`，那会被误解成只读导入方向。

## 架构

```
core/              领域模型 + SessionSource trait（零外部依赖）
sources/opencode/  opencode 只读实现（rusqlite）+ 导出格式
apps/cli/          clap 入口
```

依赖方向单向：`core` ← `sources/*` ← `apps/cli`。

**`core` 刻意不依赖 `rusqlite` / `anyhow`**——错误类型只带 `String` 描述，
库层用 `thiserror`，只有二进制入口用 `anyhow` 堆上下文。这样 GUI 阶段能直接
复用库而不必传递 `anyhow::Error`。

新增数据源（Claude Code 的 `~/.claude/projects/**/*.jsonl`、Codex 的
`~/.codex/sessions/**/*.jsonl`）只需再写一个 `SessionSource` 实现直读各自存储。

## 导出格式

`show` 与 `export` 都有 `--format json|markdown`，默认 `json`。两种格式不是
互相替代：**JSON 是给机器和导入用的，Markdown 是给人读的**。

### JSON（默认）：原始记录的无损镜像

单会话导出（`show` / `export <ID>`，以及 `export` 批量模式下的每个 `.json`
文件）是**原始记录的无损镜像**，不做归一：

```json
{
  "app": "opencode",
  "session_id": "ses_...",
  "raw_data": {
    "session": { ... },
    "session_message": [ ... ],
    "message": [ ... ],
    "part": [ ... ]
  }
}
```

- `raw_data` 就是各 source 的 `dump()` 结果：opencode 是四张表原样，codebuddy
  是 JSONL 事件流，zed 是 `columns` + 解压后的 `thread_json`。
- 保留原始字符串（含工具调用的参数与输出）——**为编辑器全文搜索而存在**：
  在 Zed 里搜「音频解码」直接命中原文。
- 代价是体积：单会话实测可达 33 MB（主要是文件全文和命令输出）。批量导出前
  先 `--dry-run` 看清单。
- `export` 批量模式还会在输出目录里写 `_index.json`：筛选条件快照 + 每个文件的
  id / 标题 / 路径 / token / 字节数。它描述**本次运行的结果**，每次重写，别手改。

### Markdown：版式化的对话

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

## 路线图

见 [`ROADMAP.md`](ROADMAP.md)（任务列表树维护）。下一步是 v2 的写操作
（`fork --at <seq>` / `edit`），前置约束见下节。

## 开发

```bash
cargo test --workspace                     # 27 个测试
cargo clippy --workspace --all-targets     # 0 warning
cargo build --release
just ci                                    # fmt + clippy + test，提交前跑这个
```

## 提交与推送

agent 自行决定，准则如下：

- **提交**：每完成一个可独立理解的逻辑单元，且 `just ci` 全绿，就提交一条 commit；
  不积累大块未提交的工作。
  - 消息用 conventional 风格（按改动实质选 `feat:` / `fix:` / `refactor:` /
    `docs:` / `chore:`），描述可写中文。
  - 提交前必须 `just ci` 全绿（fmt + clippy 0 warning + test 全过），不绿不提交。
- **推送**：当工作已完整交付（各逻辑单元都已提交、ci 全绿）时，可自行 push 到
  `origin` 当前分支；半成品或重构中途的状态不 push。
  - 永远不 `--force`、不改写已推送的历史。

## 环境与可用工具

- **remote**：`origin` = `git@github.com:Nahida-aa/aa_session.git`。
- **`gh` CLI 已安装且已登录**（账号 `Nahida-aa`，git 协议 ssh）。
  GitHub 相关操作（看/建 issue、PR、release、查远端状态等）直接跑 `gh`，
  不需要网页或其他途径。

---

完整的通用 Rust 规则索引在 [`.agents/rust-skill-index.md`](.agents/rust-skill-index.md)
（cargo-skill 生成，与本项目无关，仅作参考）。

zed 原版 agent 的工具调用损坏案例（空 id/name、参数丢失）与导出→改→导入的
恢复流程记录在 [`.agents/zed-tool-call-corruption.md`](.agents/zed-tool-call-corruption.md)。
