#[cfg(target_os = "macos")]
mod macos_impl {
    use anyhow::{Context, Result};
    use ffmpeg_next as ffmpeg;
    use libgoosy::{
        EncoderBackend, LyricsStyle, RenderControl, RenderOptions, RenderedFrame, Renderer,
        background::BackgroundRenderer,
        cover_renderer::CoverRenderer,
        geometry::FrameGeometry,
        image_asset::ImageAsset,
        layout::Layout,
        lrc::{BackgroundVocal, LyricLine, LyricWord},
        lyrics_renderer::LyricsRenderer,
        ttml, video,
    };
    use objc2::rc::Retained;
    use objc2_metal::{MTLCreateSystemDefaultDevice, MTLDevice};
    use skia_safe::{
        AlphaType, Color, ColorSpace, ColorType, EncodedImageFormat, IPoint, ISize, ImageInfo,
        Pixmap,
        gpu::{self, Budgeted, SurfaceOrigin, mtl},
    };
    use std::path::Path;
    use std::time::Instant;

    #[repr(C)]
    struct Timeval {
        tv_sec: i64,
        tv_usec: i32,
    }

    #[repr(C)]
    struct Rusage {
        ru_utime: Timeval,
        ru_stime: Timeval,
        ru_maxrss: i64,
        ru_ixrss: i64,
        ru_idrss: i64,
        ru_isrss: i64,
        ru_minflt: i64,
        ru_majflt: i64,
        ru_nswap: i64,
        ru_inblock: i64,
        ru_oublock: i64,
        ru_msgsnd: i64,
        ru_msgrcv: i64,
        ru_nsignals: i64,
        ru_nvcsw: i64,
        ru_nivcsw: i64,
    }

    unsafe extern "C" {
        fn getrusage(who: i32, usage: *mut Rusage) -> i32;
    }

    #[derive(Clone, Copy, Debug)]
    struct ProcessMetrics {
        user_sec: f64,
        sys_sec: f64,
        #[allow(dead_code)]
        max_rss_mb: f64,
        resident_mb: f64,
    }

    // Mach task_vm_info for current resident & phys_footprint on macOS
    #[repr(C)]
    #[derive(Default)]
    struct TaskBasicInfo64 {
        suspend_count: i32,
        virtual_size: u64,
        resident_size: u64,
        user_time: [u32; 2],
        system_time: [u32; 2],
        policy: i32,
    }

    unsafe extern "C" {
        fn mach_task_self() -> u32;
        fn task_info(
            target_task: u32,
            flavor: u32,
            task_info_out: *mut TaskBasicInfo64,
            task_info_out_cnt: *mut u32,
        ) -> i32;
    }

    fn get_process_metrics() -> ProcessMetrics {
        let mut usage = std::mem::MaybeUninit::<Rusage>::zeroed();
        unsafe {
            getrusage(0, usage.as_mut_ptr());
        }
        let usage = unsafe { usage.assume_init() };
        let user_sec = usage.ru_utime.tv_sec as f64 + usage.ru_utime.tv_usec as f64 * 1e-6;
        let sys_sec = usage.ru_stime.tv_sec as f64 + usage.ru_stime.tv_usec as f64 * 1e-6;
        let max_rss_mb = usage.ru_maxrss as f64 / (1024.0 * 1024.0);

        let mut basic = TaskBasicInfo64::default();
        let mut count =
            (std::mem::size_of::<TaskBasicInfo64>() / std::mem::size_of::<u32>()) as u32;
        let kr = unsafe {
            task_info(
                mach_task_self(),
                20, // MACH_TASK_BASIC_INFO
                &mut basic,
                &mut count,
            )
        };
        let resident_mb = if kr == 0 {
            basic.resident_size as f64 / (1024.0 * 1024.0)
        } else {
            0.0
        };

        ProcessMetrics {
            user_sec,
            sys_sec,
            max_rss_mb,
            resident_mb,
        }
    }

    fn make_test_cover_png(side: i32) -> Vec<u8> {
        let info = ImageInfo::new_n32_premul((side, side), None);
        let row_bytes = side as usize * 4;
        let mut pixels = vec![0u8; row_bytes * side as usize];
        for y in 0..side as usize {
            for x in 0..side as usize {
                let offset = (y * side as usize + x) * 4;
                pixels[offset] = ((x * 255) / side as usize) as u8;
                pixels[offset + 1] = ((y * 255) / side as usize) as u8;
                pixels[offset + 2] = (((x + y) * 127) / side as usize) as u8;
                pixels[offset + 3] = 255;
            }
        }
        let pixmap = Pixmap::new(&info, &mut pixels, row_bytes).unwrap();
        pixmap.encode(EncodedImageFormat::PNG, 90).unwrap()
    }

    fn load_benchmark_lines() -> Result<Vec<LyricLine>> {
        let text =
            std::fs::read_to_string("testdata/zoo.ttml").context("read testdata/zoo.ttml")?;
        let mut lines = ttml::parse_ttml(&text)?;
        // Enrich a couple of early lines with background vocals so background vocal paths are also exercised
        for (i, line) in lines.iter_mut().enumerate() {
            if i % 3 == 1 && line.background_vocal.is_none() {
                line.background_vocal = Some(BackgroundVocal {
                    start_ms: line.start_ms + 100,
                    end_ms: line.end_ms.saturating_sub(100).max(line.start_ms + 200),
                    text: "Background vocal harmony (伴唱测试)".to_owned(),
                    translation: Some("伴唱翻译测试".to_owned()),
                    words: vec![
                        LyricWord {
                            start_ms: line.start_ms + 100,
                            end_ms: (line.start_ms + line.end_ms) / 2,
                            text: "Background ".to_owned(),
                        },
                        LyricWord {
                            start_ms: (line.start_ms + line.end_ms) / 2,
                            end_ms: line.end_ms.saturating_sub(100).max(line.start_ms + 200),
                            text: "vocal harmony (伴唱测试)".to_owned(),
                        },
                    ],
                });
            }
        }
        Ok(lines)
    }

    fn summarize_latencies(
        name: &str,
        mut latencies_ms: Vec<f64>,
        total_elapsed_s: f64,
        cpu_s: f64,
        rss_before: f64,
        rss_after: f64,
        gpu_mb: f64,
    ) {
        latencies_ms.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let n = latencies_ms.len();
        let mean = latencies_ms.iter().sum::<f64>() / n as f64;
        let p50 = latencies_ms[n / 2];
        let p95 = latencies_ms[(n * 95) / 100];
        let p99 = latencies_ms[(n * 99) / 100];
        let steady_fps = 1000.0 / mean;
        let wall_fps = n as f64 / total_elapsed_s;
        let cpu_pct = (cpu_s / total_elapsed_s) * 100.0;
        println!(
            "{:<46} | {:>7.2} FPS (steady {:>7.2}) | mean {:>6.2}ms p50 {:>6.2}ms p95 {:>6.2}ms p99 {:>6.2}ms | CPU {:>5.1}% | RSS {:.1}->{:.1} MB | GPU {:.1} MB",
            name, wall_fps, steady_fps, mean, p50, p95, p99, cpu_pct, rss_before, rss_after, gpu_mb
        );
    }

    fn bench_resolution(
        width: u32,
        height: u32,
        fps: u32,
        num_frames: u64,
        cover_png: &[u8],
        cover_path: &Path,
        song_path: &Path,
    ) -> Result<()> {
        println!(
            "\n================================================================================"
        );
        println!(
            "BENCHMARKING {}x{} @ {} FPS ({} steady-state frames, start_ms=15000)",
            width, height, fps, num_frames
        );
        println!(
            "================================================================================"
        );

        let lines = load_benchmark_lines()?;
        let asset = ImageAsset::from_bytes(cover_png)?;
        let start_ms = 15_000u64; // active singing section in zoo.ttml

        // 1. Sub-stage breakdown on Skia Metal (GPU render without CPU readback)
        {
            let device = MTLCreateSystemDefaultDevice().context("no Metal device")?;
            let queue = device.newCommandQueue().context("no Metal queue")?;
            let backend = unsafe {
                mtl::BackendContext::new(
                    Retained::as_ptr(&device) as mtl::Handle,
                    Retained::as_ptr(&queue) as mtl::Handle,
                )
            };
            let mut context =
                gpu::direct_contexts::make_metal(&backend, None).context("make_metal")?;
            context.set_resource_cache_limits(skia_safe::gpu::ganesh::ResourceCacheLimits {
                max_resources: 4_096,
                max_resource_bytes: 256 * 1024 * 1024,
            });
            let info = ImageInfo::new(
                ISize::new(width as i32, height as i32),
                ColorType::RGBA8888,
                AlphaType::Premul,
                ColorSpace::new_srgb(),
            );
            let mut surface = gpu::surfaces::render_target(
                &mut context,
                Budgeted::No,
                &info,
                0,
                SurfaceOrigin::TopLeft,
                None,
                false,
                false,
            )
            .context("create Metal surface")?;

            let geometry = FrameGeometry::for_frame(width, height);
            let mut bg_image = BackgroundRenderer::from_image_asset(&asset)?;
            let mut bg_grad = BackgroundRenderer::dynamic();
            let mut cover =
                CoverRenderer::from_image_asset(&asset, Some("Zoo (From Zootopia 2)".to_owned()))?;
            let lyrics = LyricsRenderer::new_with_options(
                &lines,
                geometry.lyrics,
                true,
                true,
                LyricsStyle::default(),
            )?;
            let mut layout = Layout::new(
                &lines,
                geometry.lyrics.height,
                lyrics.group_heights(),
                lyrics.group_gap(),
                lyrics.interlude_slot_height(),
            );

            // Warmup 5 frames
            for f in 0..5 {
                let t_ms = start_ms + f * 1000 / fps as u64;
                layout.update(&lines, t_ms, fps);
                let canvas = surface.canvas();
                canvas.clear(Color::BLACK);
                bg_image.draw(canvas, &geometry, t_ms)?;
                cover.draw(canvas, &geometry)?;
                lyrics.draw(canvas, &lines, &layout, t_ms, geometry.lyrics.height as u32)?;
                context.flush_submit_and_sync_cpu();
            }

            // 1a: Background (ImageLayer sigma=96 blur) GPU-only
            {
                let m0 = get_process_metrics();
                let t0 = Instant::now();
                let mut lats = Vec::with_capacity(num_frames as usize);
                for f in 0..num_frames {
                    let f0 = Instant::now();
                    let t_ms = start_ms + f * 1000 / fps as u64;
                    let canvas = surface.canvas();
                    canvas.clear(Color::BLACK);
                    bg_image.draw(canvas, &geometry, t_ms)?;
                    context.flush_submit_and_sync_cpu();
                    lats.push(f0.elapsed().as_secs_f64() * 1000.0);
                }
                let elapsed = t0.elapsed().as_secs_f64();
                let m1 = get_process_metrics();
                let gpu_mb = device.currentAllocatedSize() as f64 / (1024.0 * 1024.0);
                summarize_latencies(
                    "1a. GPU Render: ImageBackground only (sync)",
                    lats,
                    elapsed,
                    (m1.user_sec - m0.user_sec) + (m1.sys_sec - m0.sys_sec),
                    m0.resident_mb,
                    m1.resident_mb,
                    gpu_mb,
                );
            }

            // 1b: Background (Dynamic Gradient) GPU-only
            {
                let m0 = get_process_metrics();
                let t0 = Instant::now();
                let mut lats = Vec::with_capacity(num_frames as usize);
                for f in 0..num_frames {
                    let f0 = Instant::now();
                    let t_ms = start_ms + f * 1000 / fps as u64;
                    let canvas = surface.canvas();
                    canvas.clear(Color::BLACK);
                    bg_grad.draw(canvas, &geometry, t_ms)?;
                    context.flush_submit_and_sync_cpu();
                    lats.push(f0.elapsed().as_secs_f64() * 1000.0);
                }
                let elapsed = t0.elapsed().as_secs_f64();
                let m1 = get_process_metrics();
                let gpu_mb = device.currentAllocatedSize() as f64 / (1024.0 * 1024.0);
                summarize_latencies(
                    "1b. GPU Render: GradientBackground only (sync)",
                    lats,
                    elapsed,
                    (m1.user_sec - m0.user_sec) + (m1.sys_sec - m0.sys_sec),
                    m0.resident_mb,
                    m1.resident_mb,
                    gpu_mb,
                );
            }

            // 1c: CoverRenderer GPU-only
            {
                let m0 = get_process_metrics();
                let t0 = Instant::now();
                let mut lats = Vec::with_capacity(num_frames as usize);
                for _f in 0..num_frames {
                    let f0 = Instant::now();
                    let canvas = surface.canvas();
                    cover.draw(canvas, &geometry)?;
                    context.flush_submit_and_sync_cpu();
                    lats.push(f0.elapsed().as_secs_f64() * 1000.0);
                }
                let elapsed = t0.elapsed().as_secs_f64();
                let m1 = get_process_metrics();
                let gpu_mb = device.currentAllocatedSize() as f64 / (1024.0 * 1024.0);
                summarize_latencies(
                    "1c. GPU Render: CoverLayer only (sync)",
                    lats,
                    elapsed,
                    (m1.user_sec - m0.user_sec) + (m1.sys_sec - m0.sys_sec),
                    m0.resident_mb,
                    m1.resident_mb,
                    gpu_mb,
                );
            }

            // 1d: LyricsRenderer GPU-only
            {
                let m0 = get_process_metrics();
                let t0 = Instant::now();
                let mut lats = Vec::with_capacity(num_frames as usize);
                for f in 0..num_frames {
                    let f0 = Instant::now();
                    let t_ms = start_ms + f * 1000 / fps as u64;
                    layout.update(&lines, t_ms, fps);
                    let canvas = surface.canvas();
                    canvas.clear(Color::BLACK);
                    lyrics.draw(canvas, &lines, &layout, t_ms, geometry.lyrics.height as u32)?;
                    context.flush_submit_and_sync_cpu();
                    lats.push(f0.elapsed().as_secs_f64() * 1000.0);
                }
                let elapsed = t0.elapsed().as_secs_f64();
                let m1 = get_process_metrics();
                let gpu_mb = device.currentAllocatedSize() as f64 / (1024.0 * 1024.0);
                summarize_latencies(
                    "1d. GPU Render: LyricsLayer only (sync)",
                    lats,
                    elapsed,
                    (m1.user_sec - m0.user_sec) + (m1.sys_sec - m0.sys_sec),
                    m0.resident_mb,
                    m1.resident_mb,
                    gpu_mb,
                );
            }

            // 1e: Full Scene GPU-only (ImageBg + Cover + Lyrics, sync, NO CPU readback)
            {
                let m0 = get_process_metrics();
                let t0 = Instant::now();
                let mut lats = Vec::with_capacity(num_frames as usize);
                for f in 0..num_frames {
                    let f0 = Instant::now();
                    let t_ms = start_ms + f * 1000 / fps as u64;
                    layout.update(&lines, t_ms, fps);
                    let canvas = surface.canvas();
                    canvas.clear(Color::BLACK);
                    bg_image.draw(canvas, &geometry, t_ms)?;
                    cover.draw(canvas, &geometry)?;
                    lyrics.draw(canvas, &lines, &layout, t_ms, geometry.lyrics.height as u32)?;
                    context.flush_submit_and_sync_cpu();
                    lats.push(f0.elapsed().as_secs_f64() * 1000.0);
                }
                let elapsed = t0.elapsed().as_secs_f64();
                let m1 = get_process_metrics();
                let gpu_mb = device.currentAllocatedSize() as f64 / (1024.0 * 1024.0);
                summarize_latencies(
                    "1e. GPU Render: Full Scene (ImageBg, no read)",
                    lats,
                    elapsed,
                    (m1.user_sec - m0.user_sec) + (m1.sys_sec - m0.sys_sec),
                    m0.resident_mb,
                    m1.resident_mb,
                    gpu_mb,
                );
            }

            // 1f: Full Scene GPU-only (GradBg + Cover + Lyrics, sync, NO CPU readback)
            {
                let m0 = get_process_metrics();
                let t0 = Instant::now();
                let mut lats = Vec::with_capacity(num_frames as usize);
                for f in 0..num_frames {
                    let f0 = Instant::now();
                    let t_ms = start_ms + f * 1000 / fps as u64;
                    layout.update(&lines, t_ms, fps);
                    let canvas = surface.canvas();
                    canvas.clear(Color::BLACK);
                    bg_grad.draw(canvas, &geometry, t_ms)?;
                    cover.draw(canvas, &geometry)?;
                    lyrics.draw(canvas, &lines, &layout, t_ms, geometry.lyrics.height as u32)?;
                    context.flush_submit_and_sync_cpu();
                    lats.push(f0.elapsed().as_secs_f64() * 1000.0);
                }
                let elapsed = t0.elapsed().as_secs_f64();
                let m1 = get_process_metrics();
                let gpu_mb = device.currentAllocatedSize() as f64 / (1024.0 * 1024.0);
                summarize_latencies(
                    "1f. GPU Render: Full Scene (GradBg, no read)",
                    lats,
                    elapsed,
                    (m1.user_sec - m0.user_sec) + (m1.sys_sec - m0.sys_sec),
                    m0.resident_mb,
                    m1.resident_mb,
                    gpu_mb,
                );
            }

            // 1g: Pure readback cost (surface.read_pixels on already-rendered surface)
            {
                let row_bytes = width as usize * 4;
                let mut pixels = vec![0u8; row_bytes * height as usize];
                let m0 = get_process_metrics();
                let t0 = Instant::now();
                let mut lats = Vec::with_capacity(num_frames as usize);
                for _f in 0..num_frames {
                    let f0 = Instant::now();
                    assert!(surface.read_pixels(&info, &mut pixels, row_bytes, IPoint::new(0, 0)));
                    lats.push(f0.elapsed().as_secs_f64() * 1000.0);
                }
                let elapsed = t0.elapsed().as_secs_f64();
                let m1 = get_process_metrics();
                let gpu_mb = device.currentAllocatedSize() as f64 / (1024.0 * 1024.0);
                summarize_latencies(
                    "2a. GPU->CPU Readback only (read_pixels)",
                    lats,
                    elapsed,
                    (m1.user_sec - m0.user_sec) + (m1.sys_sec - m0.sys_sec),
                    m0.resident_mb,
                    m1.resident_mb,
                    gpu_mb,
                );
            }
        }

        // 2. Full Renderer::render_frame_into (Render + Readback)
        let mut sample_frame = RenderedFrame::new(width, height);
        {
            let init_t0 = Instant::now();
            let mut renderer = Renderer::with_scene(
                width,
                height,
                fps,
                lines.clone(),
                BackgroundRenderer::from_image_asset(&asset)?,
                Some(CoverRenderer::from_image_asset(
                    &asset,
                    Some("Zoo (From Zootopia 2)".to_owned()),
                )?),
            )?;
            let init_ms = init_t0.elapsed().as_secs_f64() * 1000.0;
            let first_t0 = Instant::now();
            renderer.render_frame_into(start_ms, &mut sample_frame)?;
            let first_ms = first_t0.elapsed().as_secs_f64() * 1000.0;
            println!(
                "    [Startup] Renderer init: {:.2}ms, First frame warmup: {:.2}ms",
                init_ms, first_ms
            );

            let m0 = get_process_metrics();
            let t0 = Instant::now();
            let mut lats = Vec::with_capacity(num_frames as usize);
            for f in 0..num_frames {
                let f0 = Instant::now();
                let t_ms = start_ms + f * 1000 / fps as u64;
                renderer.render_frame_into(t_ms, &mut sample_frame)?;
                lats.push(f0.elapsed().as_secs_f64() * 1000.0);
            }
            let elapsed = t0.elapsed().as_secs_f64();
            let m1 = get_process_metrics();
            let device = MTLCreateSystemDefaultDevice().unwrap();
            let gpu_mb = device.currentAllocatedSize() as f64 / (1024.0 * 1024.0);
            summarize_latencies(
                "2b. Renderer::render_frame_into (ImageBg+Read)",
                lats,
                elapsed,
                (m1.user_sec - m0.user_sec) + (m1.sys_sec - m0.sys_sec),
                m0.resident_mb,
                m1.resident_mb,
                gpu_mb,
            );

            // 2c. Zero-Readback GPU Render + Metal RGBA->NV12 into IOSurface CVPixelBufferPool
            let mut gpu_frame = RenderedFrame::new(width, height);
            renderer.render_frame_into_opts(start_ms, &mut gpu_frame, false)?;
            let m0 = get_process_metrics();
            let t0 = Instant::now();
            let mut lats = Vec::with_capacity(num_frames as usize);
            for f in 0..num_frames {
                let f0 = Instant::now();
                let t_ms = start_ms + f * 1000 / fps as u64;
                renderer.render_frame_into_opts(t_ms, &mut gpu_frame, false)?;
                lats.push(f0.elapsed().as_secs_f64() * 1000.0);
            }
            drop(gpu_frame);
            let elapsed = t0.elapsed().as_secs_f64();
            let m1 = get_process_metrics();
            let gpu_mb = device.currentAllocatedSize() as f64 / (1024.0 * 1024.0);
            summarize_latencies(
                "2c. Renderer::render_frame_into_opts (GPU NV12)",
                lats,
                elapsed,
                (m1.user_sec - m0.user_sec) + (m1.sys_sec - m0.sys_sec),
                m0.resident_mb,
                m1.resident_mb,
                gpu_mb,
            );
        }

        // 3. Color conversion only (CPU swscale RGBA -> YUV420P as in FfmpegNextEncoder)
        {
            ffmpeg::init()?;
            let mut scaler = ffmpeg::software::scaling::Context::get(
                ffmpeg::format::Pixel::RGBA,
                width,
                height,
                ffmpeg::format::Pixel::YUV420P,
                width,
                height,
                ffmpeg::software::scaling::flag::Flags::BILINEAR,
            )?;
            let mut rgba =
                ffmpeg::util::frame::video::Video::new(ffmpeg::format::Pixel::RGBA, width, height);
            let mut yuv420p = ffmpeg::util::frame::video::Video::new(
                ffmpeg::format::Pixel::YUV420P,
                width,
                height,
            );
            let row_bytes = width as usize * 4;
            let m0 = get_process_metrics();
            let t0 = Instant::now();
            let mut lats = Vec::with_capacity(num_frames as usize);
            for _f in 0..num_frames {
                let f0 = Instant::now();
                let stride = rgba.stride(0);
                for row in 0..height as usize {
                    let source = &sample_frame.pixels()[row * row_bytes..(row + 1) * row_bytes];
                    let destination = &mut rgba.data_mut(0)[row * stride..row * stride + row_bytes];
                    destination.copy_from_slice(source);
                }
                scaler.run(&rgba, &mut yuv420p)?;
                lats.push(f0.elapsed().as_secs_f64() * 1000.0);
            }
            let elapsed = t0.elapsed().as_secs_f64();
            let m1 = get_process_metrics();
            summarize_latencies(
                "3. CPU Color Conv (memcpy + swscale RGBA->I420)",
                lats,
                elapsed,
                (m1.user_sec - m0.user_sec) + (m1.sys_sec - m0.sys_sec),
                m0.resident_mb,
                m1.resident_mb,
                0.0,
            );
        }

        // 4a. Encoder-only: PipeEncoder (external ffmpeg rawvideo -> h264_videotoolbox)
        {
            let out_path =
                std::env::temp_dir().join(format!("goosy_bench_pipe_{width}x{height}.mp4"));
            let mut sink =
                video::create_encoder(EncoderBackend::Pipe, &out_path, width, height, fps, None)?;
            let mut frame = sample_frame.clone();
            let m0 = get_process_metrics();
            let t0 = Instant::now();
            let mut lats = Vec::with_capacity(num_frames as usize);
            for _f in 0..num_frames {
                let f0 = Instant::now();
                frame = sink.submit(frame)?;
                lats.push(f0.elapsed().as_secs_f64() * 1000.0);
            }
            let finish_t0 = Instant::now();
            sink.finish()?;
            let finish_ms = finish_t0.elapsed().as_secs_f64() * 1000.0;
            let elapsed = t0.elapsed().as_secs_f64();
            let m1 = get_process_metrics();
            let _ = std::fs::remove_file(&out_path);
            summarize_latencies(
                "4a. Encoder-only: PipeEncoder (rawvideo pipe)",
                lats,
                elapsed,
                (m1.user_sec - m0.user_sec) + (m1.sys_sec - m0.sys_sec),
                m0.resident_mb,
                m1.resident_mb,
                0.0,
            );
            println!(
                "    [Shutdown] PipeEncoder finish flush: {:.2}ms",
                finish_ms
            );
        }

        // 4b. Encoder-only: FfmpegNextEncoder (in-process swscale + h264_videotoolbox)
        {
            let out_path =
                std::env::temp_dir().join(format!("goosy_bench_ffnext_{width}x{height}.mp4"));
            let mut sink = video::create_encoder(
                EncoderBackend::FfmpegNext,
                &out_path,
                width,
                height,
                fps,
                None,
            )?;
            let mut frame = sample_frame.clone();
            let m0 = get_process_metrics();
            let t0 = Instant::now();
            let mut lats = Vec::with_capacity(num_frames as usize);
            for _f in 0..num_frames {
                let f0 = Instant::now();
                frame = sink.submit(frame)?;
                lats.push(f0.elapsed().as_secs_f64() * 1000.0);
            }
            let finish_t0 = Instant::now();
            sink.finish()?;
            let finish_ms = finish_t0.elapsed().as_secs_f64() * 1000.0;
            let elapsed = t0.elapsed().as_secs_f64();
            let m1 = get_process_metrics();
            let _ = std::fs::remove_file(&out_path);
            summarize_latencies(
                "4b. Encoder-only: FfmpegNextEncoder (in-proc)",
                lats,
                elapsed,
                (m1.user_sec - m0.user_sec) + (m1.sys_sec - m0.sys_sec),
                m0.resident_mb,
                m1.resident_mb,
                0.0,
            );
            println!(
                "    [Shutdown] FfmpegNextEncoder finish flush: {:.2}ms",
                finish_ms
            );
        }

        // 5. Complete Export (End-to-End): FfmpegNextEncoder H.264 vs HEVC (Metal NV12 -> VideoToolbox) vs PipeEncoder
        for (label, backend, codec) in [
            (
                "5a. End-to-End Export: FfmpegNext H.264 (Metal->VT)",
                EncoderBackend::FfmpegNext,
                libgoosy::VideoCodec::H264,
            ),
            (
                "5b. End-to-End Export: FfmpegNext HEVC  (Metal->VT)",
                EncoderBackend::FfmpegNext,
                libgoosy::VideoCodec::Hevc,
            ),
            (
                "5c. End-to-End Export: PipeEncoder H.264 (legacy)",
                EncoderBackend::Pipe,
                libgoosy::VideoCodec::H264,
            ),
        ] {
            let out_path = std::env::temp_dir().join(format!(
                "goosy_bench_e2e_{}_{}_{width}x{height}.mp4",
                backend.cli_value(),
                codec.cli_value()
            ));
            let duration_ms = (num_frames * 1000) / fps as u64;
            let options = RenderOptions {
                song: song_path.to_path_buf(),
                lyrics: Some(Path::new("testdata/zoo.ttml").to_path_buf()),
                output: out_path.clone(),
                width,
                height,
                fps,
                background: None,
                cover: Some(cover_path.to_path_buf()),
                title: Some("Zoo (From Zootopia 2)".to_owned()),
                no_embedded_cover: true,
                no_audio: false,
                render_translation: true,
                lyrics_style: LyricsStyle::default(),
                render_background_vocal: true,
                excluded_lines: Vec::new(),
                format: libgoosy::LyricFormat::Ttml,
                sample_start_ms: Some(start_ms),
                sample_duration_ms: Some(duration_ms),
                encoder: backend,
                codec,
                nvenc_preset: "p5".to_owned(),
            };
            let m0 = get_process_metrics();
            let t0 = Instant::now();
            let mut last_elapsed = 0.0f64;
            let mut lats = Vec::with_capacity(num_frames as usize);
            let stats = libgoosy::render_with_frame_progress_control_stats(
                &options,
                |_done, _total, elapsed, _pixels| {
                    lats.push((elapsed - last_elapsed) * 1000.0);
                    last_elapsed = elapsed;
                },
                || RenderControl::Continue,
            )?;
            let total_wall_s = t0.elapsed().as_secs_f64();
            let m1 = get_process_metrics();
            let device = MTLCreateSystemDefaultDevice().unwrap();
            let gpu_mb = device.currentAllocatedSize() as f64 / (1024.0 * 1024.0);
            let warmup_count = 15.min(lats.len());
            let warmup_ms: f64 = lats[..warmup_count].iter().sum();
            let steady_ms: f64 = lats[warmup_count..].iter().sum();
            let steady_frames = lats.len().saturating_sub(warmup_count);
            let init_ms = (total_wall_s - stats.elapsed_seconds).max(0.0) * 1000.0;
            let finish_and_mux_ms = (stats.elapsed_seconds - last_elapsed).max(0.0) * 1000.0;
            summarize_latencies(
                label,
                lats,
                stats.elapsed_seconds,
                (m1.user_sec - m0.user_sec) + (m1.sys_sec - m0.sys_sec),
                m0.resident_mb,
                m1.resident_mb,
                gpu_mb,
            );
            println!(
                "    [Wall-Clock Breakdown] total_wall={:.1}ms ({:.2} FPS) = init:{:.1}ms + warmup({}f):{:.1}ms ({:.1} FPS) + steady({}f):{:.1}ms ({:.2} FPS) + finish_mux:{:.1}ms | render_loop_with_mux={:.1}ms ({:.2} FPS)",
                total_wall_s * 1000.0,
                stats.frames as f64 / total_wall_s,
                init_ms,
                warmup_count,
                warmup_ms,
                warmup_count as f64 / (warmup_ms / 1000.0).max(1e-6),
                steady_frames,
                steady_ms,
                steady_frames as f64 / (steady_ms / 1000.0).max(1e-6),
                finish_and_mux_ms,
                stats.elapsed_seconds * 1000.0,
                stats.frames as f64 / stats.elapsed_seconds,
            );
            verify_encoded_mp4(&out_path, width, height, stats.frames)?;
            let _ = std::fs::remove_file(&out_path);
        }

        Ok(())
    }

    fn verify_encoded_mp4(
        path: &Path,
        expected_w: u32,
        expected_h: u32,
        expected_frames: u64,
    ) -> Result<()> {
        let out = std::process::Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-count_frames",
                "-select_streams",
                "v:0",
                "-show_entries",
                "stream=codec_name,pix_fmt,width,height,nb_read_frames,r_frame_rate,color_range",
                "-of",
                "json",
            ])
            .arg(path)
            .output()
            .context("run ffprobe on encoded output")?;
        anyhow::ensure!(out.status.success(), "ffprobe failed on {}", path.display());
        let doc: serde_json::Value = serde_json::from_slice(&out.stdout)?;
        let stream = &doc["streams"][0];
        let w = stream["width"].as_u64().unwrap_or(0) as u32;
        let h = stream["height"].as_u64().unwrap_or(0) as u32;
        let frames: u64 = stream["nb_read_frames"]
            .as_str()
            .unwrap_or("0")
            .parse()
            .unwrap_or(0);
        let codec = stream["codec_name"].as_str().unwrap_or("?");
        let pix_fmt = stream["pix_fmt"].as_str().unwrap_or("?");
        let fps_str = stream["r_frame_rate"].as_str().unwrap_or("?");
        println!(
            "    [Verified MP4] codec={} pix_fmt={} {}x{} frames={}/{} r_frame_rate={}",
            codec, pix_fmt, w, h, frames, expected_frames, fps_str
        );
        anyhow::ensure!(w == expected_w && h == expected_h, "resolution mismatch");
        anyhow::ensure!(
            frames == expected_frames,
            "frame count mismatch: got {frames}, expected {expected_frames}"
        );
        Ok(())
    }

    fn ensure_test_audio(path: &Path) -> Result<()> {
        if path.exists() {
            return Ok(());
        }
        let status = std::process::Command::new("ffmpeg")
            .args([
                "-y",
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:sample_rate=48000:duration=60",
                "-c:a",
                "aac",
                "-b:a",
                "192k",
            ])
            .arg(path)
            .status()
            .context("generate test audio with ffmpeg")?;
        anyhow::ensure!(status.success(), "ffmpeg failed to generate test audio");
        Ok(())
    }

    pub fn run() -> Result<()> {
        let cover_png = make_test_cover_png(1000);
        let cover_path = std::env::temp_dir().join("goosy_bench_cover.png");
        std::fs::write(&cover_path, &cover_png)?;
        let song_path = std::env::temp_dir().join("goosy_bench_song.m4a");
        ensure_test_audio(&song_path)?;

        // Run 1080p @ 120 FPS (240 frames = 2.0s)
        bench_resolution(1920, 1080, 120, 240, &cover_png, &cover_path, &song_path)?;

        // Run 4K @ 120 FPS (120 frames = 1.0s)
        bench_resolution(3840, 2160, 120, 120, &cover_png, &cover_path, &song_path)?;

        // Long-running 4K120 stability & memory leak verification (480 frames = 4.0s @ 120 FPS)
        println!(
            "\n================================================================================"
        );
        println!("LONG-RUNNING STABILITY & MEMORY LEAK CHECK: 3840x2160 @ 120 FPS (480 frames)");
        println!(
            "================================================================================"
        );
        {
            let out_path = std::env::temp_dir().join("goosy_bench_stability_4k120.mp4");
            let options = RenderOptions {
                song: song_path.clone(),
                lyrics: Some(Path::new("testdata/zoo.ttml").to_path_buf()),
                output: out_path.clone(),
                width: 3840,
                height: 2160,
                fps: 120,
                background: None,
                cover: Some(cover_path.clone()),
                title: Some("Zoo (From Zootopia 2)".to_owned()),
                no_embedded_cover: true,
                no_audio: false,
                render_translation: true,
                lyrics_style: LyricsStyle::default(),
                render_background_vocal: true,
                excluded_lines: Vec::new(),
                format: libgoosy::LyricFormat::Ttml,
                sample_start_ms: Some(15_000),
                sample_duration_ms: Some(4_000),
                encoder: EncoderBackend::FfmpegNext,
                codec: libgoosy::VideoCodec::Hevc,
                nvenc_preset: "p5".to_owned(),
            };
            let device = MTLCreateSystemDefaultDevice().unwrap();
            let mut checkpoints = Vec::new();
            let stats = libgoosy::render_with_progress(&options, |done, total, elapsed| {
                if done % 120 == 0 || done == total {
                    let m = get_process_metrics();
                    let gpu_mb = device.currentAllocatedSize() as f64 / (1024.0 * 1024.0);
                    checkpoints.push((done, elapsed, m.resident_mb, gpu_mb));
                }
            });
            stats?;
            for (done, elapsed, rss_mb, gpu_mb) in &checkpoints {
                println!(
                    "    Frame {:>4}/480 ({:>5.2}s, {:>6.2} FPS): RSS = {:>6.1} MB | GPU = {:>6.1} MB",
                    done,
                    elapsed,
                    *done as f64 / elapsed,
                    rss_mb,
                    gpu_mb
                );
            }
            verify_encoded_mp4(&out_path, 3840, 2160, 480)?;
            let _ = std::fs::remove_file(&out_path);
        }

        let _ = std::fs::remove_file(&cover_path);
        let _ = std::fs::remove_file(&song_path);
        Ok(())
    }
}

fn main() -> anyhow::Result<()> {
    #[cfg(target_os = "macos")]
    macos_impl::run()?;
    Ok(())
}
