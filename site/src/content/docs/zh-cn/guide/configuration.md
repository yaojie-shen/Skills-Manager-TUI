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

未设置 `--root` 或 `SKILLS_HOME` 时，程序读取指针文件。`init` 创建带注释说明默认值的 `.skills-meta/config.toml`，且不会覆盖已有配置。环境变量和指针文件中的路径可以用 `~` 开头。在 shell 的 `--root` 参数中，应使用 `"$HOME/path"`，不要引用波浪号字面量。

在 CLI 中，`--local` 和 `--project DIR` 将 `<project>/.agents/skills` 作为独立项目 store。Init、install 和 Agent 注册可以创建这个目录，读取命令则要求它已经存在。在 TUI 中，同名参数只选择部署上下文，程序仍使用中心 Library。作用域规则见 [Agent 部署](../deployment/)。

## Library 中的文件

| 相对于 root 的路径 | 用途 |
| --- | --- |
| `hello-skill/SKILL.md` | 本地技能；有效内容无需元数据记录即可识别。 |
| `repos/<alias>/<local-name>/` | 远程技能内容。 |
| `.skills-meta/config.toml` | Agent、搜索、UI 和同步等待设置，以及标签开关。 |
| `.skills-meta/tags/<tag>.toml` | 单个标签：名称、技能、颜色和描述。 |
| `.skills-meta/repos/<alias>.toml` | 来源身份和远程技能元数据。 |
| `.skills-meta/repos/.root.toml` | 独立远程技能元数据。 |
| `.skills-meta/presets/<name>.toml` | 固定预设成员和目标 Agent。 |
| `.skills-meta/format.toml` | 元数据格式版本，由 Skills Manager 写入，请勿手动修改。 |
| `.git/config` | 根目录备份的远程、分支和启用状态。 |

Library 使用文件存储。本地技能不保存单技能备注或基线。协调过程不会修改技能内容。打开旧版本写入的元数据时，程序会先升级格式，详见[元数据格式升级](#元数据格式升级)。

## 元数据格式升级

`.skills-meta/format.toml` 记录 Library 元数据的格式版本。它会随 root 一起同步，所以请让同步同一个 Library 的每台机器都使用最新版本的 Skills Manager。

打开旧版本写入的元数据时，Skills Manager 会先升级文件，再执行其他操作。旧版元数据包括：写在 `config.toml` 中的标签、按标签而不是技能列出成员的预设，以及没有 `schema` 字段的文件。

- 程序会先检查全部文件。只要有一个文件无法升级，就不写入任何内容，并在错误中指出该文件。
- 旧 `config.toml` 中同名的标签会被合并，技能取并集；如果它们的颜色或描述不同，升级会停止，由你决定保留哪一个。
- 如果 `config.toml` 中的某个标签与 `.skills-meta/tags/` 中对应文件的定义不同（例如使用旧版 Skills Manager 的机器把标签同步回了 `config.toml`），升级会停止并指出两处位置。请让两处定义一致，或删除 `config.toml` 中不需要的那一项，然后重新打开 Library。
- 写入前，原文件会按原相对路径复制到 `.skills-meta/backups/metadata-before-layout-v<from>-to-v<to>-<timestamp>/`。备份只包含这次升级修改或删除的文件。
- CLI 会在 stderr 输出备份路径。TUI 会在启动时显示升级报告；退出程序之前，可在命令面板中通过 **Show metadata migration report** 再次打开。
- 如果此时 root 同步正在进行，升级会停止且不写入。等同步结束后重新打开 Library 即可。

如果升级中途被打断，重新打开 Library 即可继续，并得到相同的结果；错误提示需要手动恢复时除外。要从原文件重新执行升级，把需要的原文件从备份复制回 `.skills-meta`。如果备份中有 `format.toml`，也一并复制回去；否则删除 `.skills-meta/format.toml`。然后重新打开 Library，Skills Manager 会再次升级这些文件。备份不包含升级新建的文件，例如 `tags/*.toml`；这些文件仍留在 `.skills-meta` 中，如果希望只从原文件开始升级，请先删除它们。

## UI 设置

将键加入现有 TOML 表；同一个表不能重复声明。

```toml
schema = 2
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

词典权重为 tech 0.7、common 0.4、user 1。受支持表中的未知字段会导致验证失败。

## 自动同步等待

```toml
[sync]
quiet_seconds = 120
tui_idle_seconds = 10
```

| 设置 | 行为 |
| --- | --- |
| `quiet_seconds` | 等 root 中未提交的改动保持不变达到这么多秒。默认 120。 |
| `tui_idle_seconds` | 最后一次键盘、粘贴、鼠标或窗口大小变化后再等这么多秒。默认 10。 |

取值为整数秒。设为 `0` 只关闭对应的等待，安全检查仍然生效。这些等待只作用于 TUI 中的自动同步，手动同步会立即执行。远程、分支和启用状态不在这里设置，请按[根目录备份](../sync/)所述，通过 `skills sync configure` 配置。
