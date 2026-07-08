# 🐳 RedsWhale

> **Fork 自 [Hmbown/DeepSeek-TUI](https://github.com/Hmbown/DeepSeek-TUI)，感谢原作者的出色工作。**
> 本仓库为 Red 的个人定制版，专为简体中文用户优化。

[![版本](https://img.shields.io/badge/版本-v0.0.12-blue)](CHANGELOG.md)
[![上游同步](https://img.shields.io/badge/上游-v0.8.40-green)](https://github.com/Hmbown/DeepSeek-TUI)
[![许可证](https://img.shields.io/badge/许可证-MIT-lightgrey)](LICENSE)

[English](README.en.md) | [日本語](README.ja-JP.md)

---

## ✨ 这是什么？

面向 DeepSeek / Ollama 等大模型 API 的终端原生对话客户端，基于 Rust + Ratatui 构建。支持流式输出、Markdown 渲染、MCP 工具调用等功能。

**与原项目的主要差异：**

| 改动 | 说明 |
|------|------|
| 🐋 品牌动画 | 标题栏鲸鱼动画旁显示 Red 名字逐字母亮起 |
| 🎨 语法高亮 | 代码块按语言渲染彩色高亮（Rust/Python/JS 等） |
| 📝 块引用渲染 | Markdown `>` 语法显示 `▌` 竖线轨道 |
| 📋 嵌套列表 | 多级列表正确缩进显示 |
| 🦙 Ollama 支持 | `/models` 可列出本地 Ollama 模型 |
| 🔢 版本独立 | 使用自有版本号体系（v0.x.x） |
| 🇨🇳 中文优先 | 文档以简体中文为主版本 |

---

## 🚀 安装

### 从源码构建

本仓库**不提供预编译二进制**，需要自行编译。

#### 前置条件

- Rust 1.88+（通过 [rustup.rs](https://rustup.rs) 安装）
- Windows 用户需安装 [WinLibs MinGW-w64](https://winlibs.com)（提供 GNU 工具链）

#### 编译步骤

```bash
# 克隆仓库
git clone https://github.com/JiaHuiRed/Red-DS-TUI.git
cd Red-DS-TUI

# 完整构建（debug 版，速度快）
cargo build

# 发布版（优化，体积小）
cargo build --release

# 可执行文件位于
# Windows: target/debug/RedsWhale-tui.exe
# Linux/macOS: target/debug/RedsWhale-tui
```

### 其他安装方式

```bash
# npm —— 已装 Node 的最方便方式
npm install -g deepseek-tui

# Cargo —— 无需 Node
cargo install deepseek-tui-cli --locked   # `deepseek` 入口
cargo install deepseek-tui     --locked   # `deepseek-tui` TUI 二进制

# Homebrew —— macOS 包管理器
brew tap Hmbown/deepseek-tui
brew install deepseek-tui

# Docker —— 预构建发布镜像
docker volume create deepseek-tui-home
docker run --rm -it \
  -e DEEPSEEK_API_KEY="$DEEPSEEK_API_KEY" \
  -v deepseek-tui-home:/home/deepseek/.deepseek \
  -v "$PWD:/workspace" \
  -w /workspace \
  ghcr.io/hmbown/deepseek-tui:latest
```

> 中国大陆访问较慢时，npm 可加 `--registry=https://registry.npmmirror.com`。

### 🔄 保持与上游同步

```bash
git remote add upstream https://github.com/Hmbown/DeepSeek-TUI.git
git fetch upstream
git merge upstream/main
```

---

## ⚙️ 配置

首次运行会在以下位置创建配置文件：

- 🪟 **Windows**：`%APPDATA%\deepseek\config.toml`
- 🐧 **Linux/macOS**：`~/.deepseek/config.toml`

参考 [`config.example.toml`](config.example.toml) 查看完整配置项。

### 🔑 DeepSeek API

```toml
[providers.deepseek]
api_key = "sk-your-key-here"
```

或通过环境变量：`DEEPSEEK_API_KEY=sk-your-key-here`

### 🦙 Ollama 本地模型

已安装 [Ollama](https://ollama.com) 并在本地运行时：

**第一步：查看本地已有模型**
```bash
ollama list
```

**第二步：在 `config.toml` 末尾追加以下内容**

配置文件路径：
- 🪟 **Windows**：`%APPDATA%\deepseek\config.toml`
- 🐧 **Linux/macOS**：`~/.deepseek/config.toml`

```toml
# ── Ollama 本地模型配置 ──────────────────────────────────────────
provider = "ollama"

[providers.ollama]
base_url = "http://localhost:11434/v1"
model = "qwen3.5:9b"   # 改为 ollama list 中显示的实际模型名
# api_key = ""          # Ollama 默认不需要 key，可留空
```

**第三步：启动 RedsWhale**
```bash
target/debug/RedsWhale-tui.exe   # Windows debug 版
# 或
target/release/RedsWhale-tui.exe # Windows release 版
```

启动后底部状态栏会显示当前模型名，确认已切换至 Ollama。

**第四步：TUI 内常用命令**

| 命令 | 说明 |
|------|------|
| `/models` | 列出本地所有已下载的 Ollama 模型 |
| `/model <名称>` | 切换到指定模型（如 `/model qwen3:14b`） |
| `/provider deepseek` | 切回 DeepSeek 云端 API |
| `/provider ollama` | 切回本地 Ollama |

> 💡 只想临时用 Ollama？去掉 `provider = "ollama"` 那行，启动后手动 `/provider ollama` 即可，不影响默认配置。

---

## 🎯 使用方式

```bash
RedsWhale                                         # 交互式 TUI
RedsWhale "explain this function"                # 一次性提示
RedsWhale exec --auto --output-format stream-json "fix this bug"  # 面向后端集成的 NDJSON 流
RedsWhale exec --resume <SESSION_ID> "follow up"  # 继续非交互会话
RedsWhale --model deepseek-v4-flash "summarize"  # 指定模型
RedsWhale --model auto "fix this bug"            # 自动选择模型 + 推理强度
RedsWhale --yolo                                  # 自动批准工具
RedsWhale auth set --provider deepseek           # 保存 API key
RedsWhale doctor                                  # 检查配置和连接
RedsWhale doctor --json                           # 机器可读诊断
RedsWhale models                                  # 列出可用 API 模型
RedsWhale sessions                                # 列出已保存会话
RedsWhale resume --last                           # 恢复最近会话
RedsWhale serve --http                            # HTTP/SSE API 服务
RedsWhale mcp list                                # 列出已配置 MCP 服务器
```

---

## ⌨️ 快捷键

| 按键 | 功能 |
|------|------|
| `Tab` | 补全 `/` 或 `@`；运行中则把草稿排队；否则切换模式 |
| `Shift+Tab` | 切换推理强度：off → high → max |
| `F1` | 可搜索帮助面板 |
| `Esc` | 返回 / 关闭 |
| `Ctrl+K` | 命令面板 |
| `Ctrl+R` | 恢复旧会话 |
| `Alt+R` | 搜索提示历史和恢复草稿 |
| `Ctrl+S` | 暂存当前草稿（`/stash list`、`/stash pop` 恢复） |
| `@path` | 在输入框中附加文件或目录上下文 |
| `Enter` | 发送消息 |
| `Ctrl+J` | 插入换行 |
| `Ctrl+C` | 中止生成 |

完整快捷键见 [`docs/KEYBINDINGS.md`](docs/KEYBINDINGS.md)。

---

## 🎭 模式

| 模式 | 行为 |
|---|---|
| **Plan** 🔍 | 只读调查；模型先探索并提出计划，然后再做更改 |
| **Agent** 🤖 | 默认交互模式；多步工具调用带审批门禁 |
| **YOLO** ⚡ | 在可信工作区自动批准工具；仍会维护计划和清单以保持可见性 |

---

## 📊 模型和价格

| 模型 | 上下文 | 输入（缓存命中） | 输入（缓存未命中） | 输出 |
|---|---|---|---|---|
| `deepseek-v4-pro` | 1M | $0.003625 / 1M* | $0.435 / 1M* | $0.87 / 1M* |
| `deepseek-v4-flash` | 1M | $0.0028 / 1M | $0.14 / 1M | $0.28 / 1M |

*DeepSeek Pro 价格是限时 75% 折扣，有效期到 2026-05-31 15:59 UTC。*

---

## 📚 文档

| 文档 | 主题 |
|---|---|
| [ARCHITECTURE.md](docs/ARCHITECTURE.md) | 代码库内部结构 |
| [CONFIGURATION.md](docs/CONFIGURATION.md) | 完整配置参考 |
| [MODES.md](docs/MODES.md) | Plan / Agent / YOLO 模式 |
| [MCP.md](docs/MCP.md) | Model Context Protocol 集成 |
| [INSTALL.md](docs/INSTALL.md) | 各平台安装指南 |
| [KEYBINDINGS.md](docs/KEYBINDINGS.md) | 完整快捷键目录 |
| [LOCALIZATION.md](docs/LOCALIZATION.md) | UI 语言矩阵与切换 |

---

## 📋 更新日志

见 [CHANGELOG.md](CHANGELOG.md)。

---

## 💙 致谢

- 原项目：[Hmbown/DeepSeek-TUI](https://github.com/Hmbown/DeepSeek-TUI)
- 许可证：[MIT](LICENSE)，版权归原作者所有
