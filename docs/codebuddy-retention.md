# codebuddy 本地数据的自动清除

codebuddy 会按「保留天数」清理本地数据，**默认 30 天**。这套机制与 aa-session
的读取路径直接相关：会话存在 `~/.codebuddy/projects/<编码路径>/*.jsonl`，
过期后连同文件一起被清掉，aa-session 再读就只剩剩下的那部分。

## 保留天数：`cleanupPeriodDays`

配置项叫 `cleanupPeriodDays`（注意不是 `cleanUpPeriodDays`，部分文档里出现过
这种大小写写法，别照抄）。语义是**按会话最后活动时间**保留本地聊天记录。

三个层级，就近覆盖：

1. 项目级 `settings.json`（目标仓库里，或 `.codebuddy/settings.json`）
2. 用户全局 `~/.codebuddy/settings.json`
3. 产品内置默认：**30 天**

用 CLI 写比手改 JSON 可靠（会做 schema 校验并保留其它字段）：

```bash
# 全局：所有项目都保留 90 天
codebuddy config set -g cleanupPeriodDays 90

# 仅当前项目
codebuddy config set cleanupPeriodDays 90
```

等价的 JSON：

```json
{
  "cleanupPeriodDays": 90
}
```

本机已设为 **90**（`~/.codebuddy/settings.json`）。

## 清理是手动的，但会累积

`cleanupPeriodDays` 只决定**阈值**，真正删除要跑：

```bash
codebuddy cleanup                # 按 cleanupPeriodDays 清理
codebuddy cleanup --days 7       # 临时用 7 天覆盖配置
codebuddy cleanup --days 7 --dry-run   # 只打印将删除的内容，不动盘
```

清理范围（不只是会话）：

- 过期的会话记录（`projects/**/*.jsonl`）
- 会话生成的文件
- `traces/`、`logs/`、`diagnostics/`、`cache/`

所以「不跑 cleanup 就不会删」是真的，但**跑一次就会把阈值内的全部删掉**，
而很多版本会在启动时或会话结束时自动跑一次。把保留天数调大是唯一的保护，
真正的备份要靠导出。

## 对 aa-session 的影响

- codebuddy source（`packages/sources/codebuddy`）是**只读**的，自己不做保留
  策略，也不会因为要清理而写盘——它看见的永远是 codebuddy 还没删的那部分。
- 打算长期归档 codebuddy 会话，用 `--app codebuddy export --all -o <目录>`
  导成 JSON/Markdown，别指望 `~/.codebuddy/projects` 自己留着。
- 如果哪天要做 `import`（写回 codebuddy），注意目标文件可能已经被 cleanup
  删了，导入前先确认目录存在。

## 相关

- 与 opencode 无关：opencode 的会话在 `~/.local/share/opencode/opencode.db`，
  不过这套保留策略。
- 文档出处：codebuddy 内置文档 `cn/cli/settings.md` 的 `cleanupPeriodDays`
  条目与「本地数据清理命令」一节、`cn/cli/release-notes/v2.147.0.md`
 （`codebuddy cleanup` 首次引入）。
