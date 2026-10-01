---
title: "故障排查"
description: "处理根目录、来源、部署、更新和备份错误。"
---

## `skills root not set` 或 `does not exist`

**检查**

```sh
printf '%s\n' "$SKILLS_HOME"
cat "$HOME/.config/skills-tui/root"
```

**原因**

全局命令无法通过 `--root`、`SKILLS_HOME` 或指针文件解析出已存在的 Library。CLI 的 `--local` 命令则要求 `<project>/.agents/skills` 项目 store 已存在。

**修复**

创建目录，通过 `--root`、`SKILLS_HOME` 或指针文件选中它，再运行 `skills init`。已有配置应直接编辑，不要删除后重建。只有需要独立项目 store 时才运行 `--local init`。TUI 的 `--project DIR` 只选择部署上下文，仍使用中心 Library。完整解析规则见[配置与路径](../configuration/)。

## 安装只列出选项，没有新增技能

先查看返回路径：

```sh
skills install owner/repo --list
```

**修复**

- 多次使用 `--select PATH` 选择一个或多个路径。
- 使用 `--all` 安装所有有效的最外层技能。
- 新的归档 URL 需要 `--source-name NAME`。
- `--branch` 只适用于 Git 来源。

归档会保留最外层目录。`--name` 只修改单个所选技能的本地目录名。本地路径会直接复制，`--list` 不会预览本地安装。缺少发布资产或校验文件时，安装器不支持该 tag 和 target 组合。前提条件和支持的来源形式见[安装](../installation/)与[Library 和来源](../library/)。

## Agent 没有看到技能

**检查**

```sh
skills agents status AGENT
skills deploy SKILL_KEY --agent AGENT --dry-run
```

核对 Agent、作用域、配置目录、Library key 和 frontmatter 声明名。Agent 条目只描述目标目录，不代表对应程序已经安装。

| 报告状态 | 下一条命令 |
| --- | --- |
| `broken` | `skills repair` 或 `skills agents clean AGENT --dry-run` |
| `foreign` | `skills agents remove-link … --dry-run` 或 `adopt-link … --dry-run` |
| `shadow`、`agent-only` | 比较内容后运行 `skills agents relink AGENT SKILL --dry-run` |

重新扫描只报告状态，不修复链接。若 Agent 目录与 Library 重叠、使用符号链接路径，或目标不是目录，程序会将其归类为只读。这个安全分类不检查操作系统写权限位。[健康检查与修复](../health/)说明了各项操作和 apply 参数。

## 更新被拒绝或反复提示

| 结果 | 含义 / 操作 |
| --- | --- |
| `needs-resolution` | 预览后，为完整技能选择 `--take local` 或 `--take upstream`。 |
| `--take local` 后仍提示更新 | 符合预期：内容、来源 revision 和基线均未改变。 |
| 内容无效或声明名改变 | 检查上游；已安装副本和部署保持不变。 |
| 缺少基线或 TOML 损坏 | 检查 `skills show`、`skills status`，再修复来源或语法。 |

`skills accept` 只记录远程技能当前内容的哈希，无法恢复文件。`--force` 会采用完整上游版本并丢弃本地技能修改。[更新与基线](../updates/)包含完整流程。

## 根目录备份 pending、暂停或冲突

`skills sync status` 只报告配置，不会重新获取当前的 ahead/behind 数量。自动同步关闭或暂停时，TUI 状态探测仍会运行，但不会 commit、merge 或 push。

**检查**

```sh
git -C "$SKILLS_HOME" status
git -C "$SKILLS_HOME" log -5 --oneline
skills sync --dry-run
```

未设置 `SKILLS_HOME` 时，使用实际 root。检查 Git 身份、凭据、网络、当前分支，并确认 root 拥有自己的 `.git` 目录。

发生冲突后，本地备份提交通常会保留，merge 会中止。解决 Git 状态后，再运行 `skills sync run`。不要将 force-push 或 `reset --hard` 作为常规恢复方式。关闭自动同步会保留历史，也不会关闭独立的 TUI 状态探测。[根目录备份与同步](../sync/)说明了写入策略。

## 缺字或按键行为异常

设置 `[ui] icons = "text"`，并将 `pill_caps` 改为 `"block"` 或 `"none"`。在文本输入框外，`:` 打开命令面板；在输入框内，它会输入冒号。使用页面快捷键前，先关闭覆盖层并离开文本输入。页脚和 `Ctrl+G` 会显示当前上下文的操作。
