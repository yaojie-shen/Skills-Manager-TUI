---
title: "安装"
description: "安装发布版二进制文件，或从源码构建 Skills Manager。"
---

## 安装发布版

在 macOS 或 Linux 上安装最新稳定版：

```sh
curl -fsSL https://github.com/yaojie-shen/Skills-Manager-TUI/raw/main/install.sh | sh
```

安装指定版本：

```sh
curl -fsSL https://github.com/yaojie-shen/Skills-Manager-TUI/raw/main/install.sh | sh -s -- --version TAG
```

安装 `main` 分支的最新构建：

```sh
curl -fsSL https://github.com/yaojie-shen/Skills-Manager-TUI/raw/main/install.sh | sh -s -- --channel nightly
```

这是一个预发布版，每次推送到 `main` 时都会重新构建，用于测试。

安装器会根据发布版中的 `SHA256SUMS` 校验归档文件。如果平台不受支持或缺少对应资产，安装会停止。

确认安装结果：

```sh
skills --version
skills --help
```

### 支持的平台

| 系统 | 架构 / target |
| --- | --- |
| macOS | ARM64（`aarch64-apple-darwin`）、x86-64（`x86_64-apple-darwin`） |
| Linux | ARM64（`aarch64-unknown-linux-musl`）、x86-64（`x86_64-unknown-linux-musl`） |

安装器需要 `curl` 或 `wget`、`tar`，以及 `sha256sum` 或 `shasum`。当前不支持原生 Windows；不受支持的 target 不会自动改为源码构建。

## 选择安装位置

默认安装到 `~/.local/bin/skills`。如果该目录不在 `PATH` 中，请在安装后打开新的 shell，或更新当前 shell：

```sh
export PATH="$HOME/.local/bin:$PATH"
```

可用的覆盖项：

| 参数 | 环境变量 | 用途 |
| --- | --- | --- |
| `--channel CHANNEL` | `SKILLS_CHANNEL` | `stable`（默认）或 `nightly`，即 `main` 分支的最新构建。 |
| `--version TAG` | `SKILLS_VERSION` | 安装指定版本。 |
| `--target TRIPLE` | `SKILLS_TARGET` | 选择发布 target。 |
| `--install-dir DIR` | `SKILLS_INSTALL_DIR` | 更改安装目录。 |
| — | `SKILLS_REPO` | 从其他发布仓库下载。 |
| — | `SKILLS_NO_PATH_UPDATE=1` | 不修改 shell 配置。 |

## 从源码构建

在仓库检出目录中执行：

```sh
cargo build --release --locked
cargo install --path . --locked
```

项目要求 Rust 1.98 或更高版本，并使用 edition 2024。安装位置由 Cargo 配置决定。

从 Git 来源导入技能和备份根目录时需要 `git`。从归档 URL 导入时需要 `curl`，支持 ZIP、TAR、TAR.GZ、TAR.BZ2 和 TAR.XZ。Skills Manager 调用 Git 时不会在终端中请求凭据。

Node 只用于维护本站，运行 `skills` 不需要 Node。
