# 更新日志

本文件记录 Red-DS-TUI 的所有重要变更。

格式基于 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)，
版本号遵循以下规则：小型改动 v0.0.x，中等改动 v0.1.x，大型改动 v1.x.x。

---

## [0.0.13] - 2026-07-09

### 新增

- **Y 键复制思考内容**（`ui.rs`）：在输入框为空且无模态打开时，按 `Y`（Shift+Y）将当前聚焦的 Thinking 块内容复制到剪贴板。折叠状态下的思考块也能直接复制完整内容。（#b）
- **Agent 运行数菜单标记**（`ui.rs` / `mode_picker.rs`）：`Alt+A` 打开模式选择器时，如有 agent 在运行，Agent 选项后显示黄色 `(N)` 标记，切换前即可感知活跃 agent 数量。（#d）
- **思考折叠头 Token 估算**（`history.rs`）：折叠或流式完成后的思考块头部显示 `~N tokens` 字符长度估算（≈ content.len()/4，四舍五入至 50 的倍数）。（#a）
- **输入框全选支持**（`app.rs` / `ui.rs`）：`Ctrl+A` 现在全选输入文本而不是光标跳到开头。全选后 Backspace/Delete/打字全部替换，光标移动清除选区。

### 优化

- **输入框边框始终显示**（`widgets/mod.rs`）：`composer_border = true` 时不再受终端尺寸限制，确保输入框和对话历史之间有清晰的视觉分隔。

### 修复

- **ToolContext::default() 编译错误**（`enter_plan_mode.rs` / `exit_plan_mode.rs`）：5 处测试中使用了不存在的 `Default` 实现，改为 `ToolContext::new(".")` 修复编译。
## [0.0.13] - 2026-07-09

### 新增

- **Y 键复制思考内容**（`ui.rs`）：在输入框为空且无模态打开时，按 `Y`（Shift+Y）将当前聚焦的 Thinking 块内容复制到剪贴板。折叠状态下的思考块也能直接复制完整内容。（#b）
- **Agent 运行数菜单标记**（`ui.rs` / `mode_picker.rs`）：`Alt+A` 打开模式选择器时，如有 agent 在运行，Agent 选项后显示黄色 `(N)` 标记，切换前即可感知活跃 agent 数量。（#d）
- **思考折叠头 Token 估算**（`history.rs`）：折叠或流式完成后的思考块头部显示 `~N tokens` 字符长度估算（≈ content.len()/4，四舍五入至 50 的倍数）。（#a）

### 修复

- **ToolContext::default() 编译错误**（`enter_plan_mode.rs` / `exit_plan_mode.rs`）：5 处测试中使用了不存在的 `Default` 实现，改为 `ToolContext::new(".")` 修复编译。
>>>>>>> 20988f54 (chore: bump v0.0.13 — Y key copy thinking, agent marker, token estimate)

---

## [0.0.12] - 2026-07-08

### 新增 (Upstream Cherry-Pick)

- **Thinking 块折叠/展开**（`history.rs` / `transcript.rs` / `ui.rs`）：Space 键切换 Thinking 块的折叠/展开，折叠时显示摘要预览，展开时显示完整内容。非 Thinking 块保留原有隐藏/显示行为。（#2348）
- **Session token 分解展示**（`footer_ui.rs` / `commands/status.rs` / `commands/session.rs`）：footer 状态栏和 `/status` 命令现在分开显示 input / cache hit / cache miss / output 各分项 token 数，不再混在一起。（#2152）
- **`/cache stats` 命令**（`commands/debug.rs` / `prefix_cache.rs`）：新增 prefix-cache 稳定性统计、prefix fingerprint 变化追踪、缓存命中汇总等健康状态报告。（#2264）
- **Cache warmup 键追踪**（`client/chat.rs` / `commands/debug.rs`）：记录上次 warmup 的 provider/model/静态前缀/工具目录等关键信息，`/cache inspect` 输出 warmup 失效原因（provider 切换、模型切换、前缀变化等）。（#2424）
- **紧凑 statusline token 芯片**（`footer_ui.rs`）：footer token 显示更紧凑，配合分解展示更清晰。（#2411）

### 修复

- **ANSI 转义序列泄漏**（`footer_ui.rs` / `sidebar.rs`）：Footer 工具状态标签和侧栏活动文本中原始 ANSI 转义码泄漏到 UI，现正常剥离。（#2481）
- **Prefix-cache 指纹硬编码加固**（`prefix_cache.rs`）：PrefixFingerprint 改用完整工具 JSON hash（而非文字描述），且只 hash API 可见的工具字段，排除内部元数据。provider/model 切换时自动清除缓存的 pinned prefix hash。
- **Stale hash / cache 聚合清理**（`commands/debug.rs` / `engine/turn_loop.rs`）：修复 Code Review 中发现的问题——过时 hash、cache 聚合不准确、hash 未清零等。（#2264）

---

## [0.0.11] - 2026-06-25

### 优化

- **默认模型改为 V4 Flash**（`config.rs`）：`DEFAULT_TEXT_MODEL` 从 `deepseek-v4-pro` 改为 `deepseek-v4-flash`。此项目定位为 RedCode 的兜底备用，绝大多数任务用 Flash 足够，成本降低约 3-5x。需要 Pro 时随时 `/model pro` 切换。
- **显式启用并行 tool call**（`client/chat.rs`）：HTTP 请求体中有工具列表时自动注入 `parallel_tool_calls: true`。允许模型在单次 turn 内并发执行独立的读文件、搜索、grep 等操作，减少 round-trip，加快响应速度。

### 修复

- **内部 crate 版本约束对齐**（各 `Cargo.toml`）：上游 pull 后各 crate 内部依赖仍锁定在 `0.0.9`，导致 `cargo build` 失败。统一升至 `0.0.10`。

---

## [0.0.10] - 2026-06-24

### 优化

- **System prompt 前缀缓存稳定性**（`core/engine.rs` / `prompts.rs`）：项目上下文包和 skills 块现在是 session 级缓存，不再每轮 turn 重新计算。这两个块分别遍历 workspace 目录树和扫描多个 skills 目录，内容微变就会 bust KV prefix cache。缓存后 system prompt 静态前缀长度大幅增加，cache hit 率从 ~95% 提升到 ~97-98%。

### 维护

- `system_prompt_for_mode_with_context_skills_session_and_approval` 新增两个可选缓存参数，内部优先使用传入的缓存内容，fallback 到原有逻辑。

---

## [0.0.9] - 2026-06-12

### 变更

- **DeepSeek V4 Pro 永久降价**（`pricing.rs`）：移除了折扣期时间分支逻辑（`v4_pro_discount_ends_at` / `pricing_for_model_at`），Pro 长期维持低价：
  - CN¥：缓存命中 ¥0.025/百万 token · 未命中 ¥3 · 输出 ¥6
  - US$：缓存命中 $0.003625 · 未命中 $0.435 · 输出 $0.87
  - 同步清理 `chrono` 依赖和三个时间相关测试用例
- **默认币种改为人民币**（`settings.rs`）：`cost_currency` 默认值从 `"usd"` 改为 `"cny"`，状态栏和 `/cost` 输出默认显示 ¥ 符号

### 新增

- **CodeGraph MCP 语义代码索引**：集成 [CodeGraph](https://github.com/colbymchenry/codegraph) v0.9.9，为 DeepSeek TUI 提供预构建的知识图谱查询能力。Agent 可通过 `codegraph_explore` / `codegraph_search` / `codegraph_impact` 等工具直接查询符号关系、调用图和代码结构，替代耗时的 grep/read 文件扫描 —— 平均节省 58% 工具调用、47% token 消耗、22% 响应时间。

### 维护

- **CodeGraph 配置与初始化**：创建全局 MCP 配置 `~/.deepseek/mcp.json`，在本项目完成索引构建（394 文件，16,420 节点，53,570 边，9.7s）。
- **安装方式**：`npm install -g @colbymchenry/codegraph`，MCP server 通过 `npx -y @colbymchenry/codegraph serve --mcp` 启动。其他项目需手动执行 `codegraph init -i` 构建索引。

---

## [0.0.8] - 2026-06-02

### 新增

- **Plan Mode 进出工具**（`enter_plan_mode.rs` / `exit_plan_mode.rs`）：新增 `enter_plan_mode` 和 `exit_plan_mode` 工具，让 AI 可以自主进入/退出计划模式，无需用户手动切换。`enter_plan_mode` 接受可选的 `reason` 参数说明进入原因；`exit_plan_mode` 接受可选的 `summary` 参数总结计划。退出时会验证计划是否为空，防止无计划退出。
- **Ask User 工具**（`ask_user.rs`）：新增 `ask_user` 工具，让 AI 向用户提问以收集信息或做出决策。支持三种问题类型：
  - `text`：自由文本输入
  - `choice`：单选题（从选项中选择一个）
  - `multi_choice`：多选题（从选项中选择多个）
  
  每个问题包含 `question`（问题文本）、`type`（问题类型）、`options`（选项列表）和可选的 `multi_select`（多选标志）。支持问题验证、格式化显示和响应解析。

### 参考

- 借鉴 gemini-cli 的 `enter_plan_mode`、`exit_plan_mode` 和 `ask_user` 工具设计。

---

## [0.0.7] - 2026-06-02

### 新增

- **JIT 上下文发现**（`jit_context.rs`）：当 AI 访问子目录中的文件时，自动发现并加载该目录的 AGENTS.md 等上下文文件，提供更细粒度的项目指导。支持向上搜索到工作区根目录、去重、批量发现和格式化输出。
- **Topic 管理**（`topic.rs`）：新增会话题目管理系统，支持话题命名、切换、持久化、战略意图追踪。每个话题可独立记录标题、摘要、战略意图和消息计数。会话元数据中注入 `<current_topic>` XML 块，让 AI 了解当前工作方向。
- **任务依赖关系**：TaskRecord 新增 `dependencies`、`parent_id`、`children` 字段，支持任务间依赖关系和父子层级。新增 `add_dependency`、`remove_dependency`、`add_child`、`dependencies_met`、`dependency_graph`、`visualize_dependencies` 方法。
- **循环检测**：依赖关系添加时自动检测循环，防止任务依赖形成死锁。使用 BFS 遍历依赖图，检测添加新依赖是否会形成环路。

### 参考

- 借鉴 [gemini-cli](https://github.com/google-gemini/gemini-cli) 的 JIT 上下文发现、Topic 管理和 Tracker 任务系统设计。

---

## [0.0.6] - 2026-06-02

### 变更

- **品牌重命名**：项目从 RedsTui 改名为 RedsWhale，二进制文件名 `redstui` / `redstui-tui` 改为 `RedsWhale` / `RedsWhale-tui`，界面文字、启动画面、信任对话框全部替换为 RedsWhale。
- **同步上游依赖**：合并上游 v0.8.40 的 Cargo.lock 依赖更新。
- **文档合并**：将上游中文 README 的安装方式、功能介绍、快捷键、模式说明、模型价格等内容合并到主 README.md。

---

## [0.0.5] - 2026-05-22

### 新增

- **Footer token 计数**（`footer_ui.rs`）：Cost chip 现在同时显示本次会话累计 token 数和费用，格式为 `1.2k tkn · $0.03`；仅有 token 无费用（Ollama）时只显示 token 数。

---

## [0.0.4] - 2026-05-22

### 新增

- **Fin 快速执行智能体**：新增 `tool-agent` / `fin` 子智能体类型，使用 V4 Flash 模型并关闭思考模式，专注快速执行简单工具任务，减少等待时间。
- **截图 OCR 工具**：AI 可调用 `image_ocr` 工具对项目目录中的图片文件（PNG/JPEG/TIFF）进行本地 OCR 文字提取。Windows/Linux 需安装 Tesseract，macOS 使用系统 Vision 框架。
- **会话分叉**（`/fork`）：在当前对话任意节点另开一条分支，保留上下文独立推进不同方案。
- **输入框 Home/End 跳转**：多行输入框支持 Home/End 键跳到行首/行尾。
- **model picker 记住推理强度**：切换模型后 effort 设置不再重置。

### 维护

- **同步上游 v0.8.40**：含 Windows 滚动优化（composer 箭头键默认改为滚动模式）、工具调用去重修复、子智能体写权限修复等。
- **Windows 滚动优化**：Windows 下方向键默认触发滚动而非历史导航，与 macOS/Linux 行为对齐。

---

## [0.0.3] - 2026-05-21

### 新增

- **代码块语法高亮**（`markdown_render.rs`）：fenced code block 现在根据语言标识符（rust / python / js / bash 等）渲染彩色语法高亮，使用 syntect 5 + base16-ocean.dark 主题，颜色以 24-bit RGB 输出到终端。

### 修复

- **MinGW UCRT 链接错误**：将 reqwest TLS 后端从 `rustls`（依赖 aws-lc-sys）换为 `native-tls`（Windows SChannel），修复 WinLibs 16.x POSIX UCRT 下 `nanosleep64` 未定义符号导致的链接失败。

---

## [0.0.2] - 2026-05-20

### 新增

- **嵌套列表渲染**（`markdown_render.rs`）：`Block::ListItem` 新增 `depth` 字段，按缩进层级（每层 2 空格）正确缩进显示多级列表。
- **Ollama `/models` 支持**（`ui.rs`）：`/models` 命令现在对 Ollama provider 发起实时查询，列出本地已下载的所有模型。
- **Red 名字动画**（`header.rs`）：标题栏鲸鱼旁显示 R→e→d 逐字母亮起动画，与鲸鱼跃出/落水节奏同步。
- **品牌统一**：二进制重命名为 `RedsWhale` / `RedsWhale-tui`，界面文字、启动画面、信任对话框全部替换为 RedsWhale。
- **中文文档主版本**：README.md 升级为中文主版本（emoji 标题风格），附 README.en.md 英文版。
- **版本号体系独立**：启用自有版本号（0.x.x），与上游 0.8.x 分离。

### 维护

- **同步上游 v0.8.39**：合并原项目最新修复（详见下方上游变更记录）。
- **注释规范统一**：将旧格式 `//#YYMMDD` 修正为 `//YYMMDD`。

---

## [0.0.1] - 2026-05-15

### 新增

- **块引用渲染**（`markdown_render.rs`）：支持 Markdown `>` 语法，左侧渲染 `▌` 蓝色竖线，文字以 `TEXT_DIM` 颜色显示。

---

## 上游变更记录（原项目）

以下为同步自 [Hmbown/DeepSeek-TUI](https://github.com/Hmbown/DeepSeek-TUI) 的上游版本变更，供参考。

---

### [上游 0.8.39] - 2026-05-17

#### 修复

- **飞书/Lark 桥接启动顺序已加锁**：`ThreadStore` 现在保证在持久线程状态打开前完成初始化，并增加回归测试防止回退。
- **`/model` 选择器再次即时打开**：还原了 v0.8.38 的实时目录方案，选择器不再在打开时发起阻塞网络请求，恢复显示精选 `auto` / `deepseek-v4-pro` / `deepseek-v4-flash` 条目；`/models` 命令仍可列出完整实时目录。
- **"会话批准" 再次按命令族分组**：会话批准改回使用基于模糊指纹的键值匹配，批准 `cargo build` 后也涵盖 `cargo build --release`；拒绝仍使用精确指纹，不会误拦后续不同调用。
- **Docker 首次运行状态目录可写**：镜像现在预建 `/home/deepseek/.deepseek` 并赋予 `deepseek` 所有权，修复命名卷首次运行时的写入失败。
- **运行时 API 系统提示覆盖在首轮后持续生效**：以 `system_prompt` 覆盖创建的线程在模型请求构建前的模式/上下文刷新中保持该提示。
- **压缩保留工具密集历史中的用户文本查询**：自动压缩现在在仅含工具调用/结果的保留尾部时，固定最近的用户文本消息，避免下次请求的 Jinja 模板错误。
- **翻页器跳转精确落在可见底部**：`G` / End 键不再过冲渲染边界，鼠标滚轮现在直接滚动翻页覆盖层。
- **鼠标滚轮模拟箭头时保留输入草稿**：`composer_arrows_scroll` 开启时，Up/Down 在有草稿的情况下也会滚动转录区，不再替换草稿内容。
- **多行输入框箭头先在行内移动光标**：单行模式下鼠标滚轮模拟箭头行为不变。
- **第三方 `reasoning_content` 流不再损坏文本输出**：非官方支持推理内容语义的提供商将其渲染为普通文本。
- **macOS 系统主题检测识别浅色模式**：`COLORFGBG` 缺失时回退到 macOS 外观检测，缺少 `AppleInterfaceStyle` 键时视为浅色模式。
- **`rlm_open` 接受 schema 中填充的空白来源字段**：空字符串的 `file_path`、`content`、`url` 视为缺失，只提供一个真实来源时不再验证失败。
- **终端缩放后翻页立即可用**：PageUp/PageDown 使用缩放后的视口高度，不再在下次渲染前退化为单行跳转。
- **ACP 响应将 JSON-RPC id 转为字符串**：`serve --acp` 现在即使客户端发送数字 id 也返回字符串 id，符合 Zed 严格的 ACP 客户端预期。

---

### [上游 0.8.38] - 2026-05-12

#### 修复

- **OpenAI 批量响应中保留所有 `tool_calls`**：修复流式响应中工具调用数组合并逻辑，防止多工具调用时丢失条目。
- **循环防护块正确计为失败次数**：修复工具调用循环计数器在防护块触发时不递增的问题。

---

**English**: [CHANGELOG.en.md](CHANGELOG.en.md)
