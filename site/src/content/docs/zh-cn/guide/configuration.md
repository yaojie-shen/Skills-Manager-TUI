---
title: "配置与路径"
description: "选择 Library root，并配置 UI、Agent 和搜索行为。"
---

## 选择 Library root

全局命令按以下顺序解析已存在的目录：

1. `skills --root DIR …`
2. `SKILLS_HOME`
3. `~/.config/skills-tui/root` 中保存的路径

程序没有隐式默认目录；不存在的路径和非目录会被拒绝。

```sh
mkdir -p "$HOME/skills-library" "$HOME/.config/skills-tui"
printf '%s\n' "$HOME/skills-library" > "$HOME/.config/skills-tui/root"
skills init
```

未设置 `--root` 或 `SKILLS_HOME` 时，程序读取指针文件。`init` 创建 `.skills-meta/config.toml`，且不会覆盖已有配置。环境变量和指针文件中的路径可以用 `~` 开头。在 shell 的 `--root` 参数中，应使用 `"$HOME/path"`，不要引用波浪号字面量。

在 CLI 中，`--local` 和 `--project DIR` 将 `<project>/.agents/skills` 作为独立项目 store。Init、install 和 Agent 注册可以创建这个目录，读取命令则要求它已经存在。在 TUI 中，同名参数只选择部署上下文，程序仍使用中心 Library。作用域规则见 [Agent 部署](../deployment/)。

## Library 中的文件

| 相对于 root 的路径 | 用途 |
| --- | --- |
| `hello-skill/SKILL.md` | 本地技能；有效内容无需元数据记录即可识别。 |
| `repos/<alias>/<local-name>/` | 远程技能内容。 |
| `.skills-meta/config.toml` | Agent、标签、搜索和 UI 设置。 |
| `.skills-meta/repos/<alias>.toml` | 来源身份和远程技能元数据。 |
| `.skills-meta/repos/.root.toml` | 独立远程技能元数据。 |
| `.skills-meta/presets/<name>.toml` | 固定预设成员和目标 Agent。 |
| `.git/config` | 根目录备份的远程、分支和启用状态。 |

Library 使用文件存储。本地技能不保存单技能备注或基线。协调过程不会修改技能内容。不过，打开旧版预设数据时，程序可能先创建备份，再迁移文件格式。

## UI 设置

将键加入现有 TOML 表；同一个表不能重复声明。

```toml
schema = 1
tags_enabled = true

[ui]
layout = "grid"
pill_caps = "block"
icons = "text"
```

| 设置 | 取值 / 行为 |
| --- | --- |
| `layout` | `grid`、`list`、`compact`；`split` 是兼容别名。 |
| `pill_caps` | 默认使用圆形 Powerline 端帽；缺字时改为 `block` 或 `none`。 |
| `icons` | 默认使用 Nerd Font 图标；缺字时改为 `text`。 |
| `tags_enabled` | 显示或隐藏标签分类，不删除标签或预设数据。 |

仅在当前会话切换布局，不会更新启动配置。

## Agent 路径

每个 `[[agents]]` 条目包含 `key`、可选的显示名 `name` 和 `skills_dir`。路径可以用 `~` 开头。新的全局配置默认包含 Claude 和 Codex。项目路径必须位于项目内部。

```sh
skills agents catalog
skills agents add cursor
skills agents add custom --dir /path/to/skills
```

## 搜索设置

```toml
[search]
prefix = true
fuzzy = true
dictionary = true
```

前缀、模糊匹配和词典扩展默认启用。

| 字段权重 | 默认值 |
| --- | ---: |
| name | 6 |
| tag | 4 |
| description | 2 |
| note | 2 |
| heading | 1.5 |
| body | 1 |

词典权重为 tech 0.7、common 0.4、user 1。受支持表中的未知字段会导致验证失败。根目录同步不使用 `[sync]` 表。请按[根目录备份](../sync/)所述，通过 `skills sync configure` 配置。
