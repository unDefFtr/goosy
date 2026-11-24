# GoosyRenderer

基于 Rust 和 Skia 的歌词视频渲染工具。

## 实验性 RinUI 前端

Goosy GUI Next 使用 [RinUI-rs 0.1.0-alpha.2](https://rinui-rs.undef.work/guide/getting-started.html)、
CXX-Qt 0.10 和 Qt Quick。构建需要 C++17 工具链及 Qt 6 开发环境（Qt Quick、Quick Controls 2、
Qt5Compat、Qt Shader Tools）。如果 Cargo 找不到 Qt 6 的 `qmake`，请将 `QMAKE` 指向该可执行文件。
使用视频导出前，确保 `ffmpeg` 和 `ffprobe` 可从 `PATH` 找到。

```bash
# 默认 egui 界面，无 Qt 依赖
cargo run --bin goosy

# 只构建新版与 Worker；首次运行或更新 Worker 后需要执行
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

新版支持素材选择与同名匹配、分辨率、样式、范围与随机采样、即时样式预览、导出进度和实时预览。
选定范围和随机采样均不输出音频，文件名附加 `.sample-<起点毫秒>ms`。零拷贝编码提供进度但不提供
CPU 帧预览。逐行排除、歌曲署名自动排除、PDF 导出和速度曲线仍仅在旧版提供。

界面使用 RinUI 原生 `FluentWindow` 导航、列表、输入和主题组件，一级导航为 **Home / Tasks / Settings**。
Home 是简洁欢迎页，点击“创建任务”进入内部四步向导：**素材 → 样式 → 输出 → 确认**；工作区标题为“创建任务”。
步骤条只显示一次步骤名称；素材表单保留未选择、自动匹配、音频元数据等真实状态。Tasks 查看队列、任务详情、日志和实际渲染预览；Settings 设置并发数和主题。
任务列表使用持久 `ListModel`，按 Task ID 原位更新变化的字段；进度、状态、预览或日志更新不替换列表模型、不重建既有任务行，也不重复触发列表入场动画。选择与滚动位置只随显式选择或任务结构变化更新。

Tasks 采用 Master–Detail：列表约占 30%，按创建顺序稳定排列；列表和详情独立滚动，任务画面在文件详情和日志之前。
列表只显示名称、状态、简要进度和输出文件名；详情集中显示真实状态、帧进度、耗时及当前允许的操作。
完整路径/错误可通过工具提示和“文件与错误详情”查看；日志默认折叠，固定高度独立滚动，停留底部时跟随最新日志。
现有日志协议仅保留最近 8 KiB，未扩展为完整持久历史；没有新增重试、删除或打开输出等任务能力。
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
Home/四步向导、渲染器、Worker 协议、任务状态模型和调度算法未因 Tasks 工作区调整而改变。


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


`gui-next/` 持有 RinUI Host、QML 和 CXX-Qt 桥；`src/gui_next/model.rs` 只持有可编辑草稿和 UI 适配。
`libgoosy::task_manager` 不依赖 Qt/QML：独立调度线程持有队列，每个任务在提交时保存 `Arc<RenderOptions>`
不可变快照、任务 ID、独立进程控制通道、进度、8 KiB 日志尾部和最新预览槽位。无需 GUI tick 驱动调度，
关闭页面不会停止 Worker；退出应用时管理器协作停止、超时终止并回收进程。任务记录仅在本次会话保留。

并发默认 1，可设置 1–8；暂停任务仍占槽位，降低上限不会中断正在运行的任务。**大于 1 为实验性**：
进程和文件隔离不等于 GPU/显存/硬件编码资源隔离。本机已验证两个 640 × 360 PipeEncoder Worker
同时运行及独立控制，尚未验证高分辨率、零拷贝和跨平台并行稳定性。

输出目录必须存在；拒绝已有文件以及本会话已使用的路径，规范化父目录和大小写别名（macOS/Windows
保守忽略大小写），用输出旁的 `.文件名.goosy-lock` 避免多个新版实例争用。正常结束或退出时移除锁；
宿主被强制杀死可能留下锁，确认无 Worker 后才能手动删除。锁不约束外部软件或旧 GUI 写入同一路径。
取消或失败可能留下不完整视频，需换用新路径或手动清理；不会自动删除已有输出。

Worker 沿用原有命令和帧协议；完成必须等待成功退出和管道排空，不以最后一帧进度判定。
暂停/继续等收到 `GOOSY_CONTROL paused|resumed` 才改变已确认状态；取消等确认及退出，期间显示待确认请求。
取消等待最多 10 秒，随后强制终止并记为 Failed，而非伪造 Cancelled。预览临时目录随 Worker 回收，事件只保留最新帧。

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
