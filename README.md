# GoosyRenderer

基于 Rust 和 Skia 的歌词视频渲染工具。

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
