# GoosyRenderer

基于 Rust 和 Skia 的歌词视频渲染工具。

## 实验性 RinUI 前端

Goosy GUI Next 使用 [RinUI-rs 0.1.0-alpha.2](https://rinui-rs.undef.work/guide/getting-started.html)、
CXX-Qt 0.10 和 Qt Quick。构建需要 C++17 工具链及 Qt 6 开发环境（Qt Quick、Quick Controls 2、
Qt5Compat、Qt Shader Tools）。如果 Cargo 找不到 Qt 6 的 `qmake`，请将 `QMAKE` 指向该可执行文件。
使用视频导出前，确保 `ffmpeg` 和 `ffprobe` 可从 `PATH` 找到。

```bash
# 默认 egui 界面，无 Qt 依赖；Worker 与主程序放在同一目录
cargo build --bin goosy --bin goosy-render-worker
cargo run --bin goosy

# 开发时一起构建 GUI Next 和 Worker。cargo run --bin 只更新指定的程序。
cargo build --no-default-features --features gui-next --bin goosy-gui-next --bin goosy-render-worker
cargo run --no-default-features --features gui-next --bin goosy-gui-next

# 同时编译两个 GUI 和 Worker
cargo build --features gui-next --bins
```

默认 egui 界面在 Windows 上使用 DirectX 12，避免初始化 Vulkan 时部分 Intel 驱动
（`igvk64.dll`）触发 `0xc0000005` 访问冲突。排查图形问题时仍可通过 `WGPU_BACKEND`
手动指定后端；通常无需设置该变量。

新版需要同一构建产物目录中的 `goosy-render-worker`（Windows 下为 `.exe`）。QML 已嵌入，
不需要工作目录中的源码，但部署仍需要 Qt 运行库、平台插件和 QML 模块。
若开发版任务日志出现 `unexpected argument '--workspace'`，在相同 profile 和 target 目录下执行 `cargo build --no-default-features --bin goosy-render-worker`，然后在 Tasks 中继续已中断的任务。

新版支持素材选择与同名匹配、分辨率、样式、范围与随机采样、即时样式预览、导出进度和实时预览。
选定范围和随机采样均不输出音频，文件名附加 `.sample-<起点毫秒>ms`。零拷贝编码提供进度但不提供
CPU 帧预览。逐行排除、歌曲署名自动排除、PDF 导出和速度曲线仍仅在旧版提供。

界面支持简体中文和英语。首次启动按系统语言选择，可在 **设置 → 外观 → 语言** 中即时切换；GUI Next 将选择保存在自己的 `language.json` 中。歌曲标题、歌词、文件名及原始诊断保留原文。一级导航为 **首页 / 任务 / 设置**（**Home / Tasks / Settings**）。
Home 是简洁欢迎页，点击“创建任务”进入内部四步向导：**素材 → 样式 → 输出 → 确认**；工作区标题为“创建任务”。
步骤条只显示一次步骤名称；素材表单保留未选择、自动匹配、音频元数据等真实状态。Tasks 查看队列、任务详情、日志和实际渲染预览；Settings 设置并发数和主题。
任务列表使用持久 `ListModel`，按 Task ID 原位更新变化的字段；进度、状态、预览或日志更新不替换列表模型、不重建既有任务行，也不重复触发列表入场动画。选择与滚动位置只随显式选择或任务结构变化更新。

维护界面文案时使用 Qt 6 Linguist Tools。从仓库根目录提取英文源文案，在 `zh_CN.ts` 中补全简体中文翻译，再编译并一起提交 TS 和 QM。普通构建直接嵌入仓库内的 QM。

```bash
lupdate gui-next/qml -extensions qml,js -source-language en_US -target-language zh_CN -no-obsolete -ts gui-next/qml/languages/zh_CN.ts
lrelease -fail-on-unfinished gui-next/qml/languages/zh_CN.ts -qm gui-next/qml/languages/zh_CN.qm
```

Tasks 采用 Master–Detail，列表约占 30%，与详情独立滚动。任务按创建顺序从新到旧排列，首页显示同一列表的前 20 项。任务开始、暂停、恢复和结束都保留原位置，重启后仍沿用该顺序。任务画面位于文件详情和日志之前。
列表只显示名称、状态、简要进度和输出文件名；详情集中显示真实状态、帧进度、耗时及当前允许的操作。
完整路径/错误可通过工具提示和“文件与错误详情”查看；日志默认折叠，固定高度独立滚动，停留底部时跟随最新日志。
Worker 实时日志保留最近 8 KiB；任务失败原因随历史记录保存。
左键选中任务查看详情。右键菜单随任务状态显示可用操作。“打开输出目录”可在视频生成前使用，前提是目标目录存在。

“重做”会把历史配置载入 Home 创建向导，保留原任务选用的素材、样式、编码参数和实际采样范围。向导会检查素材大小和 BLAKE3 指纹。找不到素材时可重新定位，内容变化时需要明确接受当前文件。确认创建后，新任务获得独立的 UUID、工作目录、清单和输出路径。

取消会保留历史记录。删除需要确认，有活动 Worker 时须先取消并等待其退出。Goosy 先记录删除意图，再清理工作数据、缩略图和历史记录，保留原始素材和用户导出的最终视频。如果输出盘离线，任务保留“删除中”状态。接回原盘并重新打开任务库后会继续清理。

Tasks 组件审计基于 `rinui-host = 0.1.0-alpha.2` 的文档、Gallery 和随依赖发布的源码。已有 `ListView`/`ListViewDelegate`、`ProgressBar`、`Button`、`Segmented`、`Expander` 均保留原生外观，本轮补齐交互与主题绑定；`InfoBadge` 是通知标记，不强行用于任务状态。
本轮 Tasks 采用原生组件替换表：

| 原实现 | 采用组件 | 原因 |
| --- | --- | --- |
| 已有 `ListViewDelegate`，未绑定键盘焦点状态 | 保留 `ListViewDelegate`，接入 `keyboardNavigation` | 使用原生选中、悬停、按下和焦点指示 |
| 已有 `ProgressBar`，手动指定轨道高度 | 保留 `ProgressBar`，恢复原生高度 | 沿用 Running/Paused/Error 语义与主题动画 |
| 已有操作 `Button`，等待确认时隐藏 | 保留 `Button`，使用 `enabled` 与原生图标 | 显示原生 Disabled 状态，不覆盖内部外观 |
| 已有 `Segmented`/`Expander` | 原样复用，模式项增加原生图标 | 保留键盘切换、折叠动画和状态反馈 |
| Qt 默认附加 Tooltip | RinUI `ToolTip` 实例 | 使用原生主题、背景与动画 |
| 普通操作错误文字 | RinUI `InfoBar` | 使用原生错误背景、图标和主题颜色 |
| 硬编码 Rectangle 预览底板 | RinUI `Frame` + Qt Quick `Image` | 使用主题表面；仍复用真实 RGBA provider 和 16:9 布局 |

未手工覆盖 Button、ListViewDelegate、ProgressBar、Expander、Segmented 的背景、圆角、边框或动画。没有任务专用 Header、状态显示或视频帧控件，保留 Row/Column/Flow 元数据布局、主题语义色状态文字和 Image；分栏分隔线是正常布局，独立 Flickable 使用 RinUI ScrollBar，不为此嵌套会改变滚动语义的页面容器。
任务画面与当前样式为明确独立的预览模式；无任务帧时显示真实空状态，不回退到样式画面。
ViewModel 按任务保存最新预览，最多缓存 16 个任务（640 × 360 RGBA 约 14 MiB），切换时恢复对应帧；被淘汰且已结束的旧任务可能没有可恢复画面。


向导左侧配置独立滚动、底部按钮固定；右侧同一个预览组件保持可见，切换步骤不销毁重建，画面保持 16:9。
支持样式预览与所选既有任务实际渲染帧切换；后者不表示当前草稿正在渲染。主要尺寸 1440 × 900，最小 1280 × 720。
所有步骤复用 Rust ViewModel 的同一份 `RenderOptions` 草稿；前后切换、返回首页和切换 Tasks/Settings 都保留配置及步骤。
草稿只在本次会话保留，未实现跨重启保存。歌词内容来自所选文件或内嵌元数据，仅供只读检查，不新增歌词编辑器。
当前已有字号比例、行高、行间距、翻译/伴唱、留白、模糊步长、调试框及内嵌封面选项均已迁移；没有添加后端未支持的字体/颜色/动画控制。

步骤按钮、下一步、上一步及“修改”统一调用 Rust 导航逻辑；向后直接返回，向前检查目标步骤所需的前置配置，失败定位到最早的问题步骤。当前步骤不重复跳转。
确认页及最终创建仍使用完整校验；修改输入清除过期错误与完成标记，前后切换不清空其他配置。
顶部步骤支持鼠标、Tab、左右箭头/Home/End 聚焦及 Space/Enter 激活；过渡期间忽略重复导航，禁用最终创建，成功后防止重复创建同一草稿。
左侧表单使用 Qt Quick 的方向淡出/滑入，复用 RinUI `Utils.appearanceSpeed`、`Utils.animationSpeedMiddle` 和打包的 `BezierCurve.softDismiss/pointToPoint`；右侧预览不参与过渡。
当前 `rinui-host = 0.1.0-alpha.2` 没有 Stepper/Wizard：PageIndicator 是圆点分页，SelectorBar 会先改变选中索引，因此使用 RinUI Button 组合受 Rust 状态控制的步骤条。
NavigationView 的内置 StackView 过渡用于全局页面，未嵌套复用以免切换整个 Home 或重建表单。曲线通过当前版本的 `qrc:/rinui/RinUI/animations` 资源导入，无新增依赖。
错误提示保留简短的原因和修正操作，技术原因在“错误详情”中展开；歌词内容、详细设置与日志默认收起。
预览只保留来源标签、画面与输出规格；架构、同步与 Worker 机制说明仅保留在开发文档中。
输出检查不占用任务 ID 或预留输出；只有最终创建时生成不可变快照并原子预留路径。完整渲染、范围采样、随机采样均在第三步选择、第四步提交。
创建失败保留草稿和当前步骤；成功自动前往 Tasks 并选中新任务，排队与运行状态取自实际 Task Manager。
参考 [官方窗口文档](https://rinui-rs.undef.work/Windowing/FluentWindow.html)、官方 Gallery 和随 `rinui-host` 发布的组件源码。


### 用户数据与任务恢复

两个 GUI 和 `goosy render` 使用同一个 Task Manager。Home 显示最近 20 项，Tasks 提供完整历史与配置快照。默认 egui 界面的“选择历史任务”也支持继续任务。在提供 CLI 的平台上，`goosy resume <UUID>` 可继续中断的任务。嵌入式库的直接渲染 API 仍由调用方管理输出和生命周期。

用户数据位于 Windows 的 `%LOCALAPPDATA%/Goosy`、macOS 的 `~/Library/Application Support/Goosy` 或 Linux 的 `${XDG_DATA_HOME:-~/.local/share}/goosy`。同一用户目录同时由一个管理器持有。退出应用后可备份整个目录。

| 位置 | 内容 |
| --- | --- |
| `history.sqlite3` | UUID、状态、时间、原始配置、帧数、耗时、输出大小和错误 |
| `settings.toml` | `max_concurrent`（1–8）、`segment_seconds`（默认 5）、`keep_segments`（默认 `false`） |
| `tasks/<UUID>/config.json` | 配置版本、渲染器标识、输入路径、大小与 BLAKE3、实际采样参数、分段策略 |
| `tasks/<UUID>/manifest.json` | 确定的时间范围、连续分段及 Checkpoint 索引、最终输出校验信息 |
| `tasks/<UUID>/segments/`、`checkpoints/` | 独立视频分段与分段末尾的动画状态 |
| `tasks/<UUID>/audio/`、`output/` | 任务私有工作目录。最终封装临时文件与输出文件位于同一文件系统 |
| `thumbnails/`、`logs/`、`cache/` | 可用的预览缩略图、诊断目录及可清理缓存 |

暂停请求在当前分段结束时生效。Worker 排空编码器、完成封装、独立解码校验并刷新视频和 Checkpoint，最后原子替换 Manifest。取消或正常关闭时会等待 Worker 处理当前分段。连续 30 秒没有帧进展或等待达到 120 秒时，管理器终止该 Worker 及其编码子进程。重新打开后可在 Tasks 继续中断的任务。恢复会检查素材与已提交文件的哈希，使用最后一个连续有效分段的动画状态。素材变动或版本不兼容时会显示原因。

最终输出通过校验且完成状态写入 SQLite 后，默认删除该任务的视频分段和 Checkpoint，并把 `manifest.json` 归档为 `manifest.completed.json`。历史记录和缩略图继续保留。运行中、暂停、中断及失败的任务保留已有恢复数据；失败任务目前没有自动过期时间。显式设置 `keep_segments = true` 可在完成后保留分段、Checkpoint 和原清单。已有设置继续生效，编辑 `settings.toml` 前先退出 Goosy。

最终封装使用输出目录下的私有暂存目录，工作区记录对应任务和尝试的 UUID。发布不会覆盖已有文件。任务管理器在后台校验成品，期间其他任务可以查询、控制和调度。旧版遗留的 `.part.*` 文件没有所有权记录，需由用户确认后自行处理。

恢复需要原输入素材和兼容的渲染环境。校验与最终发布涉及完整文件读取，最终封装还需要一份输出文件的可用空间。硬件编码、驱动与系统字体可能影响图像或码流的逐字节一致性。

真实 Worker 生命周期回归（需 PATH 中有 ffmpeg/ffprobe）：

```bash
cargo test --features gui-next --test task_manager -- --nocapture
```

覆盖顺序调度、配置快照、目标任务控制、排队取消、异常退出后继续调度、输出冲突、并发槽位缩减、
不同 Worker PID/预览目录和资源清理；Unix 上还实际强杀 Worker 检查队列恢复。

向导导航回归（验证前置条件、失败定位、入口一致性、配置保留和关闭后禁止创建）：

```bash
cargo test --features gui-next --bin goosy-gui-next wizard_routes_share_prerequisites_and_keep_draft -- --nocapture
```



## 模块

- `src/main.rs`：入口
- `src/lib.rs`：核心 API
- `src/renderer.rs`：画面合成
- `src/lyrics_renderer.rs`：歌词排版与绘制
- `src/background.rs`、`src/cover_renderer.rs`：背景与封面
- `src/lrc.rs`、`src/ttml.rs`、`src/yrc.rs`：歌词解析
- `src/video.rs`、`src/pdf_renderer.rs`：视频与 PDF 输出
- `node/`、`python/`、`cpp/`、`include/`：绑定库

## 视频导出

macOS 的 `goosy render` 默认使用 `ffmpeg-next`，支持 `--codec h264` 和 `--codec hevc`。Metal 可用时，VideoToolbox 通过 GPU 帧编码。

Linux 可用 `--encoder ffmpeg-next-nvenc-zero-copy --codec h264` 启用 Vulkan、CUDA 和 NVENC 零拷贝导出。该模式仅支持 H.264，导出期间仍报告进度。NVENC 预设可用 `--nvenc-preset p1` 至 `p7` 指定，默认为 `p5`。
