---
title: "根目录备份与同步"
description: "使用 Git 备份完整的 Library root。"
---

根目录备份将整个 Library root 作为一个 Git 工作树处理。

- 备份范围包括技能内容、元数据、备注、预设、配置和删除操作。
- 配置自动同步后，成功的 Library 变更可以触发备份。
- 状态探测可能访问远程，但不会 commit、merge 或 push。
- 冲突会停止同步；程序不会强推或重置 root。

根目录备份与技能各自的上游来源相互独立。配置中可能包含本机路径，因此应使用适合保存这些数据的可信远程仓库。

## 配置备份

Root 必须拥有真实的 `.git` 目录；不支持父目录仓库或链接式 Git worktree。新备份使用空远程仓库；已有备份应先克隆，再将克隆目录作为 Library root。Git 提交身份（`user.name` 和 `user.email`）以及远程凭据需要已能正常使用。

```sh
skills sync status
skills sync configure git@github.com:YOUR-ACCOUNT/YOUR-BACKUP.git --branch main
```

| 配置位置 | 立即产生的效果 |
| --- | --- |
| CLI | 启用自动备份，并立即执行一次完整同步。 |
| TUI | 启用自动备份，但不会立即 commit、merge 或 push。 |

已有 `origin` 指向其他 URL 时，程序不会静默替换。分支默认为 `main`；root 当前检出的分支必须通过验证。

设置保存在 root 的 `.git/config` 中：`remote.origin.url`、`skills.sync-branch`、`skills.autosync`。每份克隆都需要单独配置。旧的单技能备份仓库不会自动转换。

## 手动命令

| 命令 | 效果 |
| --- | --- |
| `skills sync status` | 显示 `url`、`branch`、`enabled`；不报告远程是否最新。 |
| `skills sync --dry-run` | 验证本地 root，并报告将保存的改动；不执行远程 fetch、merge 或 push。 |
| `skills sync` / `skills sync run` | 保存本地修改、合并远程更新，然后 push；不会强推。 |
| `skills sync push` | 保存本地修改并 push，不先 pull。 |
| `skills sync pull` | 保存本地修改并合并远程更新，不 push；可能创建备份提交。 |
| `skills sync disable` | 关闭自动同步，保留历史和配置。 |

`skills sync run`、`skills sync push`、`skills sync pull` 支持 `--dry-run`；`skills sync configure` 和 `skills sync disable` 不支持。关闭自动同步后，仍可手动执行同步。

## 状态探测与写入同步

| 操作 | 可能访问远程 | 修改 Library 或 Git refs | Commit / merge / push |
| --- | --- | --- | --- |
| TUI 状态探测 | 是 | 否 | 否 |
| `skills sync --dry-run` | 否 | 否 | 否 |
| `skills sync pull` | 是 | 可能保存本地工作并 merge | Commit / merge，不 push |
| `skills sync run` | 是 | 是 | Commit / merge / push，不强推 |

无论自动同步是否启用，TUI 都会在启动时及后续定期探测状态。探测只更新缓存中的 dirty、ahead、behind 状态；比较远程时可能使用 `.git` 下的独立缓存。

自动同步依据命令声明的 mutation scope，而不是最终 diff。Library 作用域命令成功后，即使文件没有变化，也可能触发同步。同步会暂存 root 中全部未忽略改动，包括此前由外部产生的文件。启动、读取、预览和仅修改 Agent 部署的操作不会触发自动同步。

## 失败行为

同步会在合并远程更新前保存本地工作。发生冲突时，程序会尝试中止 merge，并保留本地备份提交。检查 Git 状态，明确解决仓库状态后再手动重试。网络或认证失败可能留下待提交的修改或待推送的提交；Library 命令成功不代表远程备份已经成功。

运行时路径 `.skills-meta/.sync`、`.skills-meta/.staging`、`.skills-meta/.repair-backups`、`.skills-meta/.metadata.lock` 和 `.skills-meta/backups` 会通过 Git 本地 exclude 排除；普通 Git ignore 规则同样生效。被忽略的内容不会备份。[故障排查](../troubleshooting/)说明了无需 force-push 或 `reset --hard` 的恢复方式。
