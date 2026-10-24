---
title: "CLI 与 TUI 参考"
description: "命令分类、全局参数、搜索语法和键盘操作。"
---

## 全局入口

```sh
skills --help
skills COMMAND --help
skills --root "$HOME/skills-library" list
skills --json status
```

| 参数 | 行为 |
| --- | --- |
| 不带子命令 | 打开 TUI。 |
| `--root DIR` | 覆盖全局 root 解析。 |
| `--local`、`--project DIR` | 选择 CLI 项目 store 或 TUI 部署上下文；差异见[部署](../deployment/)。 |
| `--json` | 输出机器可读的 CLI 结果；进度和顶层错误仍可能写入 stderr。 |
| `--same-name replace` | 允许替换声明名冲突的部署。 |

错误返回非零状态码。

## 命令分类

大写单词表示占位符。运行表中的命令时需在前面加上 `skills`；其他页面的示例已包含完整调用形式。

| 分类 | 命令 / 用途 |
| --- | --- |
| 初始化 | `init` 在所选 root 中创建 `.skills-meta/config.toml` |
| 检查 | `list [QUERY…]`、`show KEY`、`status`（别名 `rescan`） |
| 导入与重命名 | `install REFERENCE`、`adopt PATH`、`repos`、`repos rename ALIAS NAME`、`rename OLD NEW`、`set-source KEY REFERENCE` |
| 删除 | `remove KEY --yes`；`--keep-meta` 保留元数据，不保留文件 |
| 标签 | `tag add/remove/set KEY [TAGS…]`、`tag list [KEY]`、`tag rename OLD NEW [--merge]`、`tag delete TAG --yes` |
| 备注 | `note get/set/clear/edit`；set 接受 `KEY TEXT` 或 `KEY -`；edit 使用 `$EDITOR`；仅支持远程技能 |
| 上游 | `accept KEY`、`check KEY/--all/--repo ALIAS`、`update KEY/--all/--repo ALIAS` |
| 部署 | `deploy/undeploy KEYS… --agent AGENT`（可重复），或 `--all-agents`；`--dry-run` 用于预览 |
| Agent | `agents list/catalog/add/status/clean/remove-link/adopt-link/relink` |
| 预设 | `preset list/show/create/delete/add/remove/deploy/undeploy/describe`、`preset rename OLD NEW [--merge]`；deploy/undeploy 使用 `--agent AGENT` 或 `--all-agents` |
| 健康 | `repair [--deployment AGENT/LINK=KEY]`；`--apply` 执行计划 |
| 根目录备份 | `sync configure/disable/status/push/pull/run`；单独使用 `sync` 执行完整同步 |

`--dry-run` 只适用于明确支持它的命令。删除和清理通常要求 `--yes`；[健康检查](../health/)和[根目录同步](../sync/)说明了安全边界。

## 搜索

```sh
skills list "incident response" tag:operations
```

```text
incident-triage  local                   [operations]  Triage production incidents with safe checklists
                 ↳ [name,description,tag] incident response runbook and safe checklists
```

方括号列出自由文本命中的字段：

- `name`、`tag`、`description`、`note`
- Markdown `heading` 和 `body`

过滤器包括 `tag:`、`preset:`、`repo:`、`agent:`、`status:`、`source:`。`list` 还支持可重复的 `--tag/-t`、`--agent/-a`、`--status/-s`，以及 `--untagged`。修改操作使用 `list` 输出的 Library key，不使用显示标签。

## TUI 按键

快捷键提示只显示在屏幕底部，并随当前获得焦点的页面、面板或弹窗更新。

| 按键 | 动作 |
| --- | --- |
| `:` | 在文本输入框外打开命令面板 |
| `a` / 右键 | 打开所选对象的 Actions |
| `?` / `Ctrl+G` | 打开帮助；编辑时 `?` 输入普通文本 |
| `F1` | 展开当前焦点的完整提示，在弹窗内也可使用；再次按下关闭 |
| `Tab` / `Shift+Tab` | 关闭编辑对话框后，切换到下一页 / 上一页 |
| `1` 到 `6` | 在文本输入框外选择可见标签页；隐藏 Tags 后顺序会变化 |
| `Enter` / 向下 | 从顶部标签栏进入页面；其他位置激活当前控件 |
| `F2` | 打开 Library 的 View 菜单，设置布局、分组、排序和分组顺序 |
| `Ctrl+←` / `Ctrl+→` | 结果列表获得焦点时，折叠 / 展开当前所有 Library 分组 |
| `Esc` / `q` | 返回；在顶部标签栏退出；编辑时 `q` 输入普通文本 |
| `Ctrl+R` | 重新扫描工作区 |
| `Ctrl+Z` / `Ctrl+Y` | 撤销 / 重做受支持的会话操作 |
| `m`、空格、`Ctrl+A` | 进入多选、切换单个技能、加入当前结果 |
| `u` / `U` | 检查 / 更新上游内容 |

命令面板会随输入过滤；方向键选择命令，Enter 执行，Esc 关闭。在 Actions 或 Commands 中，单击鼠标左键即可执行可用命令。Library 的 View 菜单只包含设置；使用 `←`/`→` 切换类别，`Tab` 前进，`Shift+Tab` 返回。使用 `↑`/`↓`、`j`/`k`、点击或滚轮切换设置会立即生效，菜单保持打开。按 `Enter`、`Esc` 或 `F2` 关闭后保留已生效的更改。全部折叠和全部展开仍位于结果框边框，并可使用 `Ctrl+←` / `Ctrl+→`。View 菜单快捷键只显示在屏幕底部。标签选择器中的改动立即生效；暂存的成员选择需要 Apply。关闭标签选择器不会撤销已经写入的改动。在弹窗外按下鼠标左键、右键或中键只关闭最上层弹窗，该事件不会传到下层；弹窗外的滚动会被忽略。补全弹窗先于父对话框关闭。暂存操作正在写入时无法关闭。点击帮助或消息正文会保持打开；在外部按下鼠标键或使用对应键盘快捷键关闭。备份设置中，外部按键关闭整个对话框，Esc 则返回上一步。底部提示区固定为一行，同时显示当前控件的说明或不可用原因及快捷键。较长的消息会截断显示；点击消息区域或按 `F1`，即可在可滚动的通知窗口中查看完整说明和快捷键，编辑文本或打开其他弹窗时也可使用。按 `F1`、`Esc` 或 `Enter` 关闭后回到原来的焦点。键盘与鼠标焦点使用同一套提示。
