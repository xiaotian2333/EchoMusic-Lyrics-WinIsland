# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## 项目概览

EchoMusic-Lyrics-WinIsland 是一个 Windows 桌面悬浮歌词/媒体“灵动岛”应用，使用 Rust 编写。它依赖 Windows SMTC 获取当前播放信息，通过本机 WebSocket 与 EchoMusic-Lyrics-bridge 对接真实歌词，并用 Skia 绘制透明、置顶、可展开的岛形窗口。

关键技术栈：

- 窗口与事件循环：`winit` + `softbuffer`
- 绘制：`skia-safe`
- 媒体状态：Windows `GlobalSystemMediaTransportControlsSessionManager`（SMTC）
- 音频频谱：`cpal` 输出捕获 + `realfft` 六段频谱
- 异步任务：`tokio`
- 托盘：`tray-icon`
- 配置：`toml` + `serde`

该项目强依赖 Windows API，开发和验证应在 Windows 10/11 上进行。CI 使用 `windows-latest` 和 Rust stable。

## 常用命令

从仓库根目录运行：

```bash
# 快速类型检查
cargo check

# 格式化
cargo fmt --all

# CI 同款格式检查
cargo fmt --all -- --check

# Clippy，警告视为错误
cargo clippy --workspace -- -D warnings

# Debug 构建
cargo build

# Release 构建
cargo build --release

# 运行应用；同一时间只能运行一个主实例
cargo run --package EchoMusic-Lyrics-WinIsland --bin EchoMusic-Lyrics-WinIsland --profile dev

# 打开设置窗口入口
cargo run -- --settings

# 运行全部测试
cargo test

# 运行单个测试（示例）
cargo test parse_music_data_payload_accepts_new_shape
cargo test parse_version_accepts_plain_and_v_tags

# 测试时显示输出
cargo test <test_name> -- --nocapture
```

发布工作流还会使用显式目标构建 AMD64 产物：

```bash
cargo build --release --verbose --target x86_64-pc-windows-msvc
```

构建环境如果缺少 Windows SDK、MSVC/LLVM 或 `ninja`，Skia/链接阶段可能失败；CI 通过 Chocolatey 安装 `ninja`。

## CI 与发布流程

- PR 检查：`.github/workflows/pr-check.yml` 执行 `cargo fmt --all -- --check` 和 `cargo clippy --workspace -- -D warnings`。
- PR 构建：`.github/workflows/pr-build.yml` 执行 `cargo build --release --verbose` 和 `cargo test --verbose`。
- Release：`.github/workflows/rust.yml` 只在 `v*` tag 或手动输入 tag 时运行，且 tag 必须等于 `v{Cargo.toml package.version}`；产物命名为 `EchoMusic-Lyrics-WinIsland-AMD64.exe`。
- PR 模板要求说明摘要、动机、具体改动；UI 改动需要截图或动画说明。

## 高层架构

### 入口与进程模式

- `src/main.rs` 负责加载配置、初始化 i18n、处理命令行参数、创建单实例 mutex、启动 Tokio runtime、启动更新检查器并进入 winit 事件循环。
- 不带参数时运行主灵动岛窗口；`--settings` 运行独立设置窗口；`--restart` 用于重启等待/清理旧进程。
- 配置文件位于用户目录 `~/.echomusic-lyrics-winisland/config.toml`，由 `src/core/persistence.rs` 读写。

### 主窗口编排

- `src/window/app.rs` 定义主 `App`，实现 `winit::application::ApplicationHandler`。
- `App` 持有主窗口、`softbuffer` surface、托盘管理器、`SmtcListener`、`AudioProcessor`、当前配置、弹簧动画状态、歌词过渡状态、自动隐藏/拖拽/触摸/进度条拖动等 UI 状态。
- `resumed()` 创建透明、置顶、跳过任务栏、不可最大化的窗口，并初始化托盘。
- `window_event()` 处理鼠标、触摸、主题变化和重绘；重绘时从 SMTC 获取 `MediaInfo`，从音频处理器取六段频谱，然后调用 `core::render::draw_island()`。
- `about_to_wait()` 是每帧调度核心：保持置顶、处理托盘事件、热加载配置、更新 hover/hit-test、自动隐藏、全屏/隐藏鼠标抑制、歌词变化、进度条 seek、弹簧动画，并按需要 request redraw。

### Core 业务模块

- `src/core/config.rs` 定义 `AppConfig`、默认值和常量。新增配置字段时要考虑 `serde(default = "...")`，避免破坏旧配置文件。
- `src/core/persistence.rs` 负责保存/加载 TOML 配置，并对尺寸/缩放等字段做下限或范围修正。
- `src/core/i18n.rs` 从运行目录 `resources/in_app/lang/*.lang` 加载语言文件，找不到时使用编译期 fallback；`auto` 会通过 Windows locale 识别中文/英文。
- `src/core/smtc.rs` 是媒体状态中枢：
  - 只选择 EchoMusic 的 SMTC 会话，目标 AUMID 为 `com.hoowhoami.echomusic`。
  - 在 `spawn_blocking` 线程中初始化 COM，轮询/监听 SMTC 状态、时间轴、封面、播放控制。
  - 用 `watch::Receiver<MediaInfo>` 向 UI 暴露最新状态，用 mpsc 接收 seek/上一首/下一首/播放暂停请求。
  - 接收 `lyrics_ws` 的 `MusicData` 后，会按当前标题/歌手校验匹配，再合并真实歌词和封面。
- `src/core/lyrics_ws.rs` 在 `127.0.0.1:17195` 启动 WebSocket 服务端。EchoMusic 插件是客户端；服务端接收 `ping`、`subscribe`、`MusicData`，并可发送 `command`（如 `request_track_lyrics`、`seek`）。协议细节见根目录 [接口文档.md](https://raw.githubusercontent.com/xiaotian2333/Lyrics-bridge/refs/heads/main/docs/%E6%8E%A5%E5%8F%A3%E6%96%87%E6%A1%A3.md)。
- `src/core/lyrics.rs` 解析 `MusicData.payload`、校验纯 Base64 封面、排序歌词行，并提供当前歌词/过滤辅助函数；这里已有多组单元测试。
- `src/core/audio.rs` 启动输出设备捕获和音频峰值门控线程，生成 `[f32; 6]` 频谱供渲染频谱条使用。
- `src/core/render.rs` 是主岛绘制入口，接收布局、媒体、歌词、窗口和样式参数，使用 thread-local Skia surface 绘制背景、封面、歌词、频谱、进度条和迷你控制，然后提交到 `softbuffer`。

### UI、窗口和工具模块

- `src/ui/expanded/music_view.rs` 绘制展开态音乐页，包含封面、标题/歌手滚动、进度条、播放控制、频谱、封面翻转/旋转等缓存和动画状态；也提供按钮/进度条 hit rect。
- `src/ui/expanded/widget_view.rs` 绘制展开态的另一页内容。
- `src/window/tray.rs` 创建系统托盘图标和菜单，菜单动作包括显示/隐藏、打开设置、重启和退出。
- `src/window/settings/` 是独立设置窗口，使用 Skia 和 `utils/settings_ui/` 的控件渲染配置界面。
- `src/icons/` 存放用 Skia path 绘制的图标。新增图标时放在这里，不要在业务代码里硬编码 SVG/path。
- `src/utils/` 包含跨模块工具：
  - `glass.rs`、`liquid_glass.rs`、`backdrop.rs`：不同背景/玻璃效果和缓存。
  - `color.rs`：从封面/屏幕取色与边框权重。
  - `font.rs`：字体管理和文本测量/绘制缓存。
  - `physics.rs`、`anim.rs`、`scroll.rs`：弹簧、动画和滚动辅助。
  - `mouse.rs`、`win32.rs`：全局鼠标、全屏检测、Win32 窗口样式/置顶等。
  - `autostart.rs`：注册表自启动。
  - `updater.rs`：检查 GitHub Release、下载新 exe 并用 PowerShell 替换重启；这里也有版本/资产命名单元测试。

## 数据流速览

1. `main.rs` 加载配置和语言，创建 Tokio runtime，启动更新检查，进入 `App`。
2. `App::default()` 创建 `SmtcListener` 和 `AudioProcessor`。
3. `SmtcListener` 启动 SMTC 轮询线程，同时启动 `lyrics_ws` 服务端。
4. EchoMusic 插件连接 `ws://127.0.0.1:17195` 后发送 `subscribe`/`ping`/`MusicData`。
5. `lyrics_ws` 将事件转给 `smtc.rs`；`smtc.rs` 校验曲目匹配后把歌词/封面合并到 `MediaInfo`。
6. 每次重绘时 `window/app.rs` 获取 `MediaInfo` 和音频频谱，传给 `core/render.rs`。
7. `render.rs` 根据配置选择默认、glass、mica、dynamic 或 liquid_glass 背景，并调用 `ui/expanded` 绘制展开页。

## 修改注意事项

- `unsafe` 块必须有 `// SAFETY:` 注释说明安全性；Windows API 调用优先使用 `windows` crate。
- 修改绘制逻辑优先在 `src/core/render.rs`、`src/ui/expanded/` 或对应 `utils/*glass*` 模块内完成，保持模块职责单一。
- 修改设置项时同步考虑：`AppConfig` 字段、默认值、设置窗口 UI、配置持久化兼容性，以及 PR 描述中的兼容性说明。
- 修改 Lyrics-bridge 对接逻辑时先阅读 [接口文档.md](https://raw.githubusercontent.com/xiaotian2333/Lyrics-bridge/refs/heads/main/docs/%E6%8E%A5%E5%8F%A3%E6%96%87%E6%A1%A3.md)。
- `WDA_EXCLUDEFROMCAPTURE` 当前有意不启用；涉及截图排除、玻璃背景或捕获逻辑时先查看 `glass.rs`、`liquid_glass.rs`、`backdrop.rs` 的注释和现有取舍。
- 项目已有 `.ai/ARCHITECTURE.md`、`.ai/STYLE-GUIDE.md` 和 `AGENTS.md`，其中包含更细的模块说明和 agent 行为规则；改动前可先查看对应文件。
- 不要自行执行 `git add`、`git commit` 或 `git push`，除非用户明确要求。
- 不使用 emoji，除非用户明确要求。
