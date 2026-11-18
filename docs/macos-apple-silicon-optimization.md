# Goosy macOS Apple Silicon Optimization Technical Report

## 1. Test Hardware & Software Environment

- **Host Model**: MacBook Air (Apple M4, 10-core CPU [4P + 6E], 10-core GPU, 16 GB unified memory)
- **Operating System**: macOS 26.3 (Darwin 25.3.0, `aarch64-apple-darwin`)
- **Graphics & Media Stack**:
  - Skia (`skia-safe` 0.99.0) with Ganesh Metal backend (`MTLDevice`, `MTLCommandQueue`)
  - CoreVideo (`CVPixelBufferPool`, `CVMetalTextureCache`) & `IOSurface`
  - VideoToolbox hardware encoding (`h264_videotoolbox` / `hevc_videotoolbox` via `ffmpeg-next` 8.1.0, FFmpeg 8.1)
- **Workload Fixture**: `testdata/zoo.ttml` (timed word-by-word lyrics with CJK translations, background vocals, emphasis words, and interlude dots), $1000 \times 1000$ RGBA cover/background image, and 48 kHz AAC audio track.

---

## 2. Previous Bottlenecks & Root Causes

Auditing and profiling the baseline architecture at $1920 \times 1080 @ 120\text{ FPS}$ (`8.29 MB/frame` RGBA) and $3840 \times 2160 @ 120\text{ FPS}$ (`33.18 MB/frame` RGBA) identified three primary bottlenecks:

1. **Full-Frame Synchronous GPU-to-CPU Readback & CPU Pixel Copies (`78.80 MB/frame` CPU traffic at 4K)**:
   - `MetalBackend::render_into` called `surface.read_pixels(...)` on every frame, forcing `flush_submit_and_sync_cpu()` (`1.83–3.05 ms/frame` synchronous CPU/GPU stall at 4K) and copying `33.18 MB/frame` from GPU memory into `RenderedFrame::pixels` (`Vec<u8>`).
   - `FfmpegNextEncoder` copied `33.18 MB/frame` on the CPU into an `AV_PIX_FMT_RGBA` frame, ran single-threaded CPU `swscale` (`RGBA -> YUV420P`, `8.10 ms/frame` at 4K, 99.9% of one core), and passed CPU `YUV420P` (`12.44 MB/frame`) to `h264_videotoolbox`, which locked a `CVPixelBuffer` and copied the planes again.
   - `PipeEncoder` wrote `33.18 MB/frame` ($3.98\text{ GB/s}$ at 4K120) across an OS `stdin` pipe to an external `ffmpeg` process and waited synchronously for buffer recycling before returning from `submit()`.
2. **Per-Frame Full-Resolution Gaussian Blurs & Offscreen Layer Allocations (`12.60 ms/frame` = `79.33 FPS` at 4K)**:
   - `BackgroundRenderer` (`ImageLayer`) executed two full-frame $3840 \times 2160$ Gaussian blur passes ($\sigma = 96.0\text{ px}$) per frame (`5.65 ms/frame`, ~150 MB transient GPU textures).
   - `LyricsRenderer` re-rasterized and blurred all ~6 visible inactive out-of-focus lyric groups ($\sigma = 6..42$) every frame (`5.50 ms/frame`) and allocated 50–80 offscreen `save_layer`s per frame for active timed-word subpixel vertical lift interpolation and gradient feather masks (`4.75–5.29 ms/frame`).
   - `CoverRenderer` composited a full $3840 \times 2160$ cached image (~85% transparent pixels) across the entire frame (`1.17 ms/frame`) instead of restricting the draw to the cover viewport bounds.
3. **Objective-C Autorelease & GPU Resource Lifetime Retention**:
   - `MetalBackend` executed per-frame Metal command submissions on CLI and worker threads without an inner `objc2::rc::autoreleasepool`, allowing transient `MTLCommandBuffer` and encoder objects to accumulate until thread exit.
   - `MetalBackend` declared `context: DirectContext` before `surface` and `nv12`, causing `DirectContext` to drop before its dependent Skia `Surface`s.

---

## 3. Major Architectural Changes

### 3.1 Less-Copy Metal $\rightarrow$ `IOSurface` `CVPixelBufferPool` $\rightarrow$ `VideoToolbox` Pipeline

- **3-Slot Ring of Metal Render Targets & Compute `RGBA -> NV12` ([src/surface.rs](../src/surface.rs))**:
  - `MetalBackend` manages a 3-slot ring of `MTLStorageModePrivate` `RGBA8Unorm` `MTLTexture`s wrapped as Skia `Surface`s (`wrap_backend_render_target`), allowing Skia to encode GPU commands for frame $N+1$ while frame $N$ is in flight.
  - Allocates an `IOSurface`-backed `CVPixelBufferPool` (`kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange` / `'420v'` NV12, `kCVPixelBufferMetalCompatibilityKey = true`) and a `CVMetalTextureCache` creating zero-copy `R8Unorm` (Y plane) and `RG8Unorm` (UV plane) Metal texture views.
  - Dispatches a Metal compute shader (`rgba_to_nv12`, BT.709 limited range) on the same `MTLCommandQueue` after `context.flush_and_submit()` (`0.36–0.43 ms/frame` at 4K) and returns a `MacosGpuFrame` holding the `CVPixelBufferRef` and un-waited `MTLCommandBuffer`.
  - Wraps per-frame Metal rendering and teardown in `objc2::rc::autoreleasepool` and flushes `CVMetalTextureCache` each frame.
- **Asynchronous Hardware Frame Encoding ([src/encoder.rs](../src/encoder.rs))**:
  - `FfmpegNextEncoder` configures `h264_videotoolbox` and `hevc_videotoolbox` on macOS with `AV_PIX_FMT_VIDEOTOOLBOX` (`realtime=true`, `prio_speed=true`, `AVCOL_RANGE_MPEG`) and spawns a dedicated `videotoolbox-encoder` worker thread fed by a bounded `sync_channel(3)`.
  - The worker waits for the `MacosGpuFrame`'s `MTLCommandBuffer` completion, attaches the `IOSurface`-backed `CVPixelBufferRef` directly to `AVFrame::data[3]` with an `AVBufferRef` release callback, and submits it to VideoToolbox with **0 bytes copied on the CPU**.
- **Copy Accounting (Less-Copy vs. Strict Zero-Copy)**:
  - This pipeline is **zero-CPU-copy** (`0 bytes/frame` read or written by the CPU; zero `read_pixels` stalls) and performs a **strict zero-copy `IOSurface` handoff** from Metal to the VideoToolbox Media Engine.
  - It is **less-copy** (not strict zero-copy) on the GPU because Skia renders to an `RGBA8Unorm` texture (`33.18 MB` at 4K) and the Metal compute kernel reads `33.18 MB` RGBA and writes `12.44 MB` NV12 into the `IOSurface` planes (`45.62 MB/frame` GPU unified-memory traffic).

### 3.2 Scene Renderer & Resource-Management Optimizations

- **Downscaled Gaussian Blur in `BackgroundRenderer` ([src/background.rs](../src/background.rs))**:
  - Composites both background image passes into a $1/8$-scale ($480 \times 270$ at 4K) offscreen GPU surface with a single $\sigma / 8 = 12.0\text{ px}$ Gaussian blur (mathematically equivalent to $\sigma = 96.0\text{ px}$ at $1\times$ scale) and bilinearly upscales to the target canvas, reducing 4K background render time from `5.65 ms` to `1.36–1.59 ms` and saving ~150 MB of GPU memory.
- **Bounded Viewport Draw in `CoverRenderer` ([src/cover_renderer.rs](../src/cover_renderer.rs))**:
  - Preserves full-frame snapshot dimensions while restricting per-frame `draw_image_rect` to the non-transparent cover + shadow `draw_bounds`, reducing 4K cover composite time from `1.17 ms` to `0.42–0.81 ms`.
- **Inactive Blurred Group Cache & Active Word-Mask Batching in `LyricsRenderer` ([src/lyrics_renderer.rs](../src/lyrics_renderer.rs))**:
  - Caches rasterized + blurred offscreen images (`BlurredGroupCacheEntry`) per inactive line and evicts entries as soon as a line becomes active, reaches zero edge opacity, or scrolls off-screen.
  - Batches timed words with `feather == 0.0` that share an identical `lift_offset` (unstarted words at `0.0` and completed words at `lift_progress == 1.0`) and combines non-overlapping `core_mask` + `feather_masks[0..3]` into a single subpixel lift interpolation `save_layer` pair (`paint_lifted_active_masks`), reducing per-frame offscreen layers on active lines from 50–80 to 4–6.

### 3.3 First-Class HEVC (`VideoCodec::Hevc`) Support

- Added `VideoCodec` (`H264` default, `Hevc`) across [src/encoder.rs](../src/encoder.rs), [src/video.rs](../src/video.rs), [src/lib.rs](../src/lib.rs), [src/main.rs](../src/main.rs) (`--codec h264|hevc`), [src/bin/goosy-render-worker.rs](../src/bin/goosy-render-worker.rs) (`--codec h264|hevc`), [src/gui.rs](../src/gui.rs) (`"格式"` selector), and [src/ffi.rs](../src/ffi.rs) (`"codec"` JSON field), tagging HEVC MP4 streams with `hvc1`.

---

## 4. Benchmark Results (`examples/bench_pipeline.rs`)

### 4.1 `1920x1080 @ 120 FPS` (240 Frames, 15 Warmup Frames)

| Stage / Pipeline | Baseline Steady FPS | Optimized Steady FPS | Speedup | Baseline Mean / p50 / p95 (ms) | Optimized Mean / p50 / p95 (ms) | CPU % (Base $\rightarrow$ Opt) | CPU Bytes Copied / Frame |
| :--- | ---: | ---: | ---: | :--- | :--- | :--- | :--- |
| **Renderer-only (GPU flush, no readback)** | 92.68 | **476.33** | **5.14x** | 10.79 / 11.45 / 13.79 | **2.10 / 2.09 / 2.66** | 25.0% $\rightarrow$ 34.4% | 0 MB $\rightarrow$ 0 MB |
| **Renderer + GPU $\rightarrow$ CPU `read_pixels`** | 100.00 | **386.67** | **3.87x** | 10.00 / 10.42 / 11.79 | **2.59 / 2.62 / 3.09** | 22.8% $\rightarrow$ 28.0% | 8.29 MB (fallback only) |
| **Renderer + Metal `RGBA -> NV12` (`IOSurface`)** | 83.75 *(render+swscale)* | **449.83** | **5.37x** | 11.94 *(10.00 + 1.94)* | **2.22 / 2.43 / 3.57** | 122.7% $\rightarrow$ **63.8%** | 19.70 MB $\rightarrow$ **0 MB** |
| **End-to-End Export (`FfmpegNext` H.264)** | 99.38 *(94.09 wall)* | **226.62 *(172.18 wall)*** | **2.28x** | 10.06 / 10.48 / 11.72 | **4.49 / 4.28 / 5.94** | 85.4% $\rightarrow$ **31.5%** | 19.70 MB $\rightarrow$ **0 MB** |
| **End-to-End Export (`FfmpegNext` HEVC)** | — | **350.05 *(242.62 wall)*** | **3.52x** | — | **2.91 / 2.84 / 3.56** | **41.6%** | **0 MB** |
| **End-to-End Export (`Pipe` H.264)** | 91.50 *(81.63 wall)* | **161.21 *(132.62 wall)*** | **1.76x** | 10.93 / 11.09 / 15.72 | **7.08 / 5.54 / 11.61** | 39.8% $\rightarrow$ 32.9% | 16.59 MB (pipe IPC) |

### 4.2 `3840x2160 @ 120 FPS` (120 Frames, 15 Warmup Frames)

| Stage / Pipeline | Baseline Steady FPS | Optimized Steady FPS | Speedup | Baseline Mean / p50 / p95 (ms) | Optimized Mean / p50 / p95 (ms) | CPU % (Base $\rightarrow$ Opt) | CPU Bytes Copied / Frame |
| :--- | ---: | ---: | ---: | :--- | :--- | :--- | :--- |
| **Renderer-only (GPU flush, no readback)** | 79.33 | **480.82** | **6.06x** | 12.60 / 13.70 / 16.01 | **2.08 / 2.06 / 2.67** | 15.4% $\rightarrow$ 28.9% | 0 MB $\rightarrow$ 0 MB |
| **Renderer + GPU $\rightarrow$ CPU `read_pixels`** | 92.58 | **202.10** | **2.18x** | 10.80 / 11.53 / 13.34 | **4.95 / 4.99 / 7.79** | 16.5% $\rightarrow$ 42.8% | 33.18 MB (fallback only) |
| **Renderer + Metal `RGBA -> NV12` (`IOSurface`)** | 52.91 *(render+swscale)* | **414.33–485.31** | **7.83x–9.17x** | 18.90 *(10.80 + 8.10)* | **2.06–2.41 / 2.19 / 4.00** | 116.4% $\rightarrow$ **29.0–51.4%** | 78.80 MB $\rightarrow$ **0 MB** |
| **Pure Metal Compute `RGBA -> NV12` (`IOSurface`)** | — | **2,344–2,800** | — | — | **0.36–0.43 / 0.36 / 0.45** | **4.1%** | **0 MB** |
| **End-to-End Export (`FfmpegNext` H.264)** | 62.65 *(59.56 wall)* | **61.37–65.85 *(50.99–58.59 wall)*** | **1.05x** *(encoder-bound)* | 15.96 / 15.54 / 20.69 | **15.18–16.14 / 15.18 / 16.59** | 70.4% $\rightarrow$ **16.5–19.2%** | 78.80 MB $\rightarrow$ **0 MB** |
| **End-to-End Export (`FfmpegNext` HEVC)** | — | **95.72–101.47 *(79.57 wall, 95.35 10-min wall)*** | **1.62x** | — | **9.86 / 10.43 / 12.14** | **24.8%** | **0 MB** |
| **End-to-End Export (`Pipe` H.264)** | 60.40 *(53.80 wall)* | **68.38–74.15 *(51.95 wall)*** | **1.13x** | 16.55 / 15.80 / 21.30 | **14.62 / 13.05 / 19.05** | 28.5% $\rightarrow$ 27.1% | 66.36 MB (pipe IPC) |

### 4.3 Steady-State vs. Short-Clip Wall-Clock Breakdown

On a 120-frame (1-second) 4K120 clip, total wall-clock time (`1,508.1 ms` = `79.57 FPS` for HEVC) includes two fixed one-time costs outside the steady-state frame loop:
- **Initialization (`144.5 ms`)**: `ffprobe` audio inspection, image asset decoding, Skia paragraph shaping, Metal shader compilation, and `VTCompressionSession` creation.
- **Shutdown & Audio Remux (`181.0 ms`)**: Draining buffered `VTCompressionSession` frames on EOF, writing the MP4 trailer, and running `ffmpeg -c:v copy -c:a aac -movflags +faststart`.
- Over longer exports (such as the 605-second / 57,689-frame 4K120 run), this `~325 ms` fixed overhead amortizes to `<0.05%`, and full wall-clock throughput matches steady-state throughput (`95.35 FPS`).

---

## 5. H.264 vs. HEVC Hardware Encoder Characterization (`examples/investigate_vt_h264.rs`)

Direct `VTCompressionSession` testing across 22 property combinations and 6 resolutions (`1280x720` through `4096x2160`) established the following empirical facts on the MacBook Air M4:

1. **Not an H.264 Level 5.2 Bitstream Cap**:
   - Inspecting the SPS emitted by `VTCompressionSession` (`CMVideoFormatDescriptionGetH264ParameterSetAtIndex`) shows that with `H264_High_AutoLevel`, VideoToolbox already negotiates **Level 6.0 (`level_idc = 60` at `ExpectedFrameRate = 120`)**, **Level 6.1 (`level_idc = 61` at `ExpectedFrameRate = 240`)**, and **Level 6.2 (`level_idc = 62` at `ExpectedFrameRate = 480`)**.
   - Explicitly forcing `kVTProfileLevel_H264_High_5_2` at $3840 \times 2160 @ 120\text{ FPS}$ causes VideoToolbox to reject frame encoding (`0` output packets).
2. **Measured Hardware Throughput Ceiling by Codec & Resolution**:
   - Across `RealTime=true/false`, `PrioritizeEncodingSpeedOverQuality=true/false`, `AllowFrameReordering=false` (no B-frames), `H264EntropyMode=CAVLC/CABAC`, `Baseline`/`Main`/`High` profiles, All-Intra, and `'420v'`/`'420f'`/`'BGRA'` pixel formats, H.264 hardware throughput on this host stays within **60.65–64.55 FPS** at $3840 \times 2160$.
   - Across resolutions from $2560 \times 1440$ (`133.10 FPS`) and $3200 \times 1800$ (`85.65 FPS`) to $3840 \times 2160$ (`61.10 FPS`) and $4096 \times 2160$ (`57.33 FPS`), the M4 H.264 hardware encoder exhibits a constant throughput ceiling of **~491–507 MPix/s (~1.92M–1.98M macroblocks/s)**.
   - The M4 HEVC hardware encoder (`kCMVideoCodecType_HEVC` with `realtime=true, prio_speed=true`) exhibits a higher constant throughput ceiling of **~832–880 MPix/s**, achieving **105.89 FPS** standalone and **95.35–101.47 FPS** end-to-end at $3840 \times 2160$.

---

## 6. Extended 10-Minute 4K120 Stability & Visual Correctness

### 6.1 10-Minute (605s) Continuous 4K120 Stress Test (`examples/stability_10min_4k120.rs`)
- **Throughput & Thermal Stability**: Encoded **57,689 frames** ($3840 \times 2160 @ 120\text{ FPS}$ HEVC, `470.0 MB` verified MP4) in `605.00s` (**`95.35 FPS` average**; `95.47 FPS` at `30s` vs. `95.42 FPS` at `600s`), with `NSProcessInfo.thermalState` remaining **`Nominal(0)`** throughout the entire 10-minute run.
- **Memory & Pool Boundedness**:
  - Peak RSS (`MaxRSS`) stabilized at **`123.6 MB`** at `60s` and grew by **`0.0 MB`** from `60s` to `605s` (active RSS `90.2 MB` at `605s`).
  - Metal `currentAllocatedSize()` plateaued at **`789.6 MB`** from `360s` through `605s` (`0.0 MB` growth over the final 23,361 frames) and dropped to **`1.2 MB`** when `Renderer` was dropped.
  - Across `57,689` acquired and `57,689` released `CVPixelBuffer`s (`0` in-flight after `finish()`), `CVPixelBufferPool` allocated only **11 unique `IOSurface` IDs**, recycling each buffer ~5,244 times.

### 6.2 Visual Correctness (`examples/validate_visual_correctness.rs`)
- **Cover Layer**: Bit-exact (`PSNR = INF`, `MAE = 0.0000`, `100.00%` identical pixels).
- **Lyrics Layer (scroll transitions, feather masks, subpixel word lift, emphasis, inactive blur cache)**: `PSNR = 60.15–60.93 dB`, `MAE = 0.0368–0.0427 / 255`, `99.83–99.90%` of pixels within $\le 2 / 255$.
- **Blurred Background Layer ($1/8\times$ $\sigma=12$ vs. $1\times$ $\sigma=96$)**: `PSNR = 54.00–55.44 dB`, `MAE = 0.1638–0.1952 / 255`, `MaxDiff = 4 / 255`, `98.27–99.67%` of pixels within $\le 2 / 255$.
- **Full Composite 4K Scene**: `PSNR = 55.01 dB`, `MAE = 0.1686 / 255`, `MaxDiff = 5 / 255`, `99.55%` of pixels within $\le 2 / 255$.
- **End-to-End Encoded 4K MP4 vs. Skia RGBA**: `PSNR = 46.91 dB` (H.264) and `46.93 dB` (HEVC).

---

## 7. Known Limitations & Future Work

1. **Single Hardware Encoder Ceiling at 4K120**: While the optimized macOS GPU renderer + Metal NV12 pipeline sustains `414–485 FPS` at $3840 \times 2160$, single-engine hardware encoding on the base M4 caps end-to-end 4K export at `~95–101 FPS` (HEVC) and `~61–66 FPS` (H.264). Exceeding 120 FPS at 4K on single-Media-Engine Apple Silicon would require hybrid GOP-sharded encoding across VideoToolbox and the idle CPU cores.
2. **External Audio Remux Subprocess**: `FfmpegNextEncoder` currently writes video to a staging file and invokes `ffmpeg -c:v copy -c:a aac` during `finish()` when audio is enabled (`~127–180 ms`). In-process audio packet muxing via `ffmpeg-next` would eliminate the staging file.
3. **GUI Default Encoder Selection**: To preserve existing GUI unit-test contracts (`default_app_uses_pipe_encoder`), `GoosyApp::default()` retains `EncoderBackend::Pipe` until switched to `ffmpeg-next Encoder` in the UI (whereas CLI and `RenderOptions::new` default to `EncoderBackend::FfmpegNext` on macOS).
4. **Cross-Platform Validation Scope**: All macOS (`#[cfg(target_os = "macos")]`) paths were compiled and validated on Apple M4 hardware; Linux (`Vulkan` / `NVENC` / `VAAPI`) and Windows (`D3D12` / `ANGLE` / `NVENC` / `AMF` / `QSV`) code paths were preserved via conditional compilation and verified by code inspection, but were not compiled or executed on Linux/Windows hosts in this environment.
