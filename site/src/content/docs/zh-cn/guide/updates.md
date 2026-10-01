---
title: "更新与基线"
description: "检查上游变化，并选择保留本地内容或采用上游内容。"
---

`skills check` 将已安装的远程技能与记录的上游 revision 比较，不会修改 Library 内容。下面继续使用[Library 与来源](../library/)中安装的 `review` 技能；它的完整 Library key 是 `repos/toolbox/review`。

## 1. 检查

```sh
skills check repos/toolbox/review
skills check --repo toolbox
skills check --all
```

按仓库或全部技能检查时，只更新已安装技能的状态。检查可能报告新发现的上游技能，但不会安装它们。

## 2. 预览

```sh
skills update repos/toolbox/review --dry-run
```

预览可能会下载上游内容用于比较，但不会应用到 Library。

## 3. 解决本地修改

上游副本和本地副本都发生变化时，需要选择保留哪一个完整副本：

| 选择 | 更新后的内容 | Revision 与基线 | 后续检查 |
| --- | --- | --- | --- |
| `--take local` | 保留本地内容 | 不变 | 可能再次报告同一上游更新 |
| `--take upstream` | 替换完整技能，并删除仅存在于本地的文件 | 更新 | 与新的上游状态比较 |

```sh
skills update repos/toolbox/review --take local
skills update repos/toolbox/review --take upstream
```

未指定选择时，结果为 `needs-resolution`。`--force` 是 `--take upstream` 的别名。不支持使用 `--take-file` 逐文件合并。采用上游副本前，请先备份需要保留的本地修改。

上游内容无效或声明名意外变化时，替换会停止，已安装内容和部署保持不变。

## 4. 应用

```sh
skills update repos/toolbox/review
```

Revision 未变化时，Skills Manager 会保留现有技能，不会重新安装。应用更新后，可能触发已配置的[根目录备份](../sync/)。

## 接受本地内容或更换来源

```sh
skills accept KEY
skills set-source KEY REFERENCE --subpath PATH
```

`skills accept` 将当前内容记录为基线，不会修改来源 revision，也不会上传修改。`skills set-source` 只修改来源记录，不替换内容，也不创建新基线。执行后可运行 `skills show` 和 `skills status` 检查结果。

## 基线记录什么

远程技能会保存安装 revision 和内容哈希。Git 来源使用仓库 revision；归档来源使用内容哈希。仓库 revision 更新不代表每个已安装技能都发生了变化。

哈希涵盖相对路径、文件内容和符号链接目标，不包含时间戳、权限、`.git`、`__pycache__`、`.DS_Store` 和 `*.pyc`。哈希可以检测变化，但不能恢复文件。本地技能在记录远程来源前没有远程基线。
