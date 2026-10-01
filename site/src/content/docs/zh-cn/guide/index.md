---
title: "概览"
description: "了解 Library、来源、部署和安全边界。"
---

Skills Manager 将 Agent Skills 保存在你选择的 **Library** 中，再通过文件系统链接部署到 Agent。`skills` 同时提供 CLI 和 TUI。不带子命令运行时会打开 TUI。

## 核心概念

| 概念 | 含义 |
| --- | --- |
| Library / root | 保存技能文件和 `.skills-meta` 的目录。每个技能都是一个包含有效、可读 `SKILL.md` 的目录。 |
| Source（来源） | Git 仓库、归档 URL 或本地目录。从远程来源导入的技能会记录来源信息和内容基线。 |
| Deployment（部署） | Agent 目录中指向 Library 技能的链接。通过链接编辑文件时，修改的是 Library 副本。 |
| Tags / presets | 标签用于整理技能。预设保存一组固定的技能和部署目标，其成员不会随标签变化。 |

## 常见任务

- [安装 Skills Manager](./installation/)并[创建 Library](./quickstart/)。
- [导入技能](./library/)并[部署到 Agent](./deployment/)。
- [使用标签和预设整理技能](./tags-presets/)。
- [检查更新](./updates/)、[修复问题](./health/)并[备份 Library](./sync/)。

## 安全边界

- 扫描和预览不会执行修复或部署；但扫描时加载旧版预设数据，仍可能先创建备份，再迁移格式。
- Git 状态探测可能访问远程，但不会 commit、merge 或 push。
- 根目录自动同步是独立流程，只会在配置完成后由 Library 变更触发。
