#[cfg(not(target_os = "windows"))]
fn main() {
    eprintln!("bench_win_qsv is Windows-only");
}

#[cfg(target_os = "windows")]
mod win_bench {
    use anyhow::{Context, Result};
    use ffmpeg_next as ffmpeg;
    use libgoosy::{
        EncoderBackend, LyricsStyle, RenderedFrame, Renderer, VideoCodec,
        background::BackgroundRenderer,
        cover_renderer::CoverRenderer,
        image_asset::ImageAsset,
        lrc::{BackgroundVocal, LyricLine, LyricWord},
        ttml, video,
        win_qsv::{
            ColorMatrix, ColorRange, Nv12ConverterMode, QsvEncodeConfig, QsvLowPowerMode,
            SharedD3d11Context, WinQsvAsyncEncoder, d3d11_pipeline::{D3d11Nv12Converter, Nv12SurfacePool},
            probe_qsv_capabilities, reset_win_qsv_telemetry, win_qsv_telemetry,
        },
    };
    use skia_safe::{EncodedImageFormat, ImageInfo, Pixmap};
    use std::time::Instant;
    use windows::Win32::Foundation::{CloseHandle, FILETIME};
    use windows::Win32::Graphics::Direct3D::D3D_SRV_DIMENSION_TEXTURE2D;
    use windows::Win32::Graphics::Direct3D11::*;
    use windows::Win32::Graphics::Dxgi::Common::*;
    use windows::Win32::System::ProcessStatus::{
        GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
    };
    use windows::Win32::System::Threading::{
        CreateEventW, GetCurrentProcess, GetProcessTimes, INFINITE, WaitForSingleObject,
    };

    #[derive(Clone, Copy, Debug)]
    struct ProcMetrics {
        cpu_sec: f64,
        rss_mb: f64,
        peak_rss_mb: f64,
    }

    fn filetime_to_sec(ft: FILETIME) -> f64 {
        let ticks = ((ft.dwHighDateTime as u64) << 32) | (ft.dwLowDateTime as u64);
        ticks as f64 * 1e-7
    }

    fn proc_metrics() -> ProcMetrics {
        let proc = unsafe { GetCurrentProcess() };
        let mut create = FILETIME::default();
        let mut exit = FILETIME::default();
        let mut kernel = FILETIME::default();
        let mut user = FILETIME::default();
        let cpu_sec = if unsafe {
            GetProcessTimes(proc, &mut create, &mut exit, &mut kernel, &mut user).is_ok()
        } {
            filetime_to_sec(kernel) + filetime_to_sec(user)
        } else {
            0.0
        };
        let mut pmc = PROCESS_MEMORY_COUNTERS::default();
        let (rss_mb, peak_rss_mb) = if unsafe {
            GetProcessMemoryInfo(
                proc,
                &mut pmc,
                std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32,
            )
            .is_ok()
        } {
            (
                pmc.WorkingSetSize as f64 / (1024.0 * 1024.0),
                pmc.PeakWorkingSetSize as f64 / (1024.0 * 1024.0),
            )
        } else {
            (0.0, 0.0)
        };
        ProcMetrics {
            cpu_sec,
            rss_mb,
            peak_rss_mb,
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

    fn print_summary(
        name: &str,
        mut lats_ms: Vec<f64>,
        wall_s: f64,
        m0: ProcMetrics,
        m1: ProcMetrics,
        extra: &str,
    ) {
        lats_ms.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let n = lats_ms.len();
        let mean = lats_ms.iter().sum::<f64>() / n as f64;
        let p50 = lats_ms[n / 2];
        let p95 = lats_ms[(n * 95) / 100];
        let p99 = lats_ms[(n * 99) / 100];
        let steady_fps = 1000.0 / mean;
        let wall_fps = n as f64 / wall_s;
        let cpu_pct = ((m1.cpu_sec - m0.cpu_sec) / wall_s) * 100.0;
        println!(
            "{:<48} | wall {:>6.2} FPS | steady {:>6.2} FPS | mean {:>5.2}ms p50 {:>5.2}ms p95 {:>5.2}ms p99 {:>5.2}ms | CPU {:>5.1}% | RSS {:.0}MB (pk {:.0}) | {}",
            name,
            wall_fps,
            steady_fps,
            mean,
            p50,
            p95,
            p99,
            cpu_pct,
            m1.rss_mb,
            m1.peak_rss_mb,
            extra
        );
    }

    fn bench_color_converters(width: u32, height: u32, iters: usize) -> Result<()> {
        println!(
            "\n--- Stage B: Pure RGBA -> NV12 Conversion Benchmark ({}x{}, {} iters) ---",
            width, height, iters
        );

        // 1. CPU swscale RGBA -> NV12
        {
            ffmpeg::init()?;
            let mut scaler = ffmpeg::software::scaling::Context::get(
                ffmpeg::format::Pixel::RGBA,
                width,
                height,
                ffmpeg::format::Pixel::NV12,
                width,
                height,
                ffmpeg::software::scaling::flag::Flags::BILINEAR,
            )?;
            let rgba = ffmpeg::util::frame::video::Video::new(
                ffmpeg::format::Pixel::RGBA,
                width,
                height,
            );
            let mut nv12 = ffmpeg::util::frame::video::Video::new(
                ffmpeg::format::Pixel::NV12,
                width,
                height,
            );
            for _ in 0..5 {
                scaler.run(&rgba, &mut nv12)?;
            }
            let m0 = proc_metrics();
            let t0 = Instant::now();
            let mut lats = Vec::with_capacity(iters);
            for _ in 0..iters {
                let f0 = Instant::now();
                scaler.run(&rgba, &mut nv12)?;
                lats.push(f0.elapsed().as_secs_f64() * 1000.0);
            }
            let wall = t0.elapsed().as_secs_f64();
            let m1 = proc_metrics();
            let mb = (width as f64 * height as f64 * 5.5) / (1024.0 * 1024.0);
            print_summary(
                "CPU swscale (RGBA -> NV12)",
                lats,
                wall,
                m0,
                m1,
                &format!("CPU mem {:.1} MB/f", mb),
            );
        }

        // 2. GPU D3D11 Converters (ComputeDirect, ComputeIntermediate, VideoProcessor)
        let d3d11 = SharedD3d11Context::get_or_create_intel()?;
        let rgba_desc = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_R8G8B8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: (D3D11_BIND_SHADER_RESOURCE.0 | D3D11_BIND_RENDER_TARGET.0) as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let mut rgba_tex = None;
        unsafe { d3d11.device.CreateTexture2D(&rgba_desc, None, Some(&mut rgba_tex))? };
        let rgba_tex = rgba_tex.unwrap();
        let srv_desc = D3D11_SHADER_RESOURCE_VIEW_DESC {
            Format: DXGI_FORMAT_R8G8B8A8_UNORM,
            ViewDimension: D3D_SRV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_SHADER_RESOURCE_VIEW_DESC_0 {
                Texture2D: D3D11_TEX2D_SRV {
                    MostDetailedMip: 0,
                    MipLevels: 1,
                },
            },
        };
        let mut srv = None;
        unsafe {
            d3d11
                .device
                .CreateShaderResourceView(&rgba_tex, Some(&srv_desc), Some(&mut srv))?;
        }
        let srv = srv.unwrap();

        let mut fence: Option<ID3D11Fence> = None;
        unsafe { d3d11.device5.CreateFence(0, D3D11_FENCE_FLAG_NONE, &mut fence)? };
        let fence = fence.unwrap();
        let ev = unsafe { CreateEventW(None, false, false, None)? };
        let mut fv = 0u64;

        for mode in [
            Nv12ConverterMode::ComputeDirect,
            Nv12ConverterMode::ComputeIntermediate,
            Nv12ConverterMode::VideoProcessor,
        ] {
            let Ok(conv) = D3d11Nv12Converter::new(
                d3d11.clone(),
                width,
                height,
                mode,
                ColorMatrix::Bt709,
                ColorRange::Limited,
            ) else {
                println!("  {:<46} | UNSUPPORTED on this GPU", mode.label());
                continue;
            };
            let pool = Nv12SurfacePool::new_for_mode(
                d3d11.clone(),
                width,
                height,
                4,
                conv.mode,
                conv.vp_enumerator(),
            )?;
            let vp_in = if let (Some(vdev), Some(vpenum)) =
                (&d3d11.video_device, conv.vp_enumerator())
            {
                let in_desc = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
                    FourCC: 0,
                    ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
                    Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 {
                        Texture2D: D3D11_TEX2D_VPIV {
                            MipSlice: 0,
                            ArraySlice: 0,
                        },
                    },
                };
                let mut v = None;
                unsafe { vdev.CreateVideoProcessorInputView(&rgba_tex, vpenum, &in_desc, Some(&mut v))? };
                v
            } else {
                None
            };
            let lease = pool.acquire();

            // Warmup
            for _ in 0..5 {
                let _g = d3d11.lock_immediate_context();
                conv.convert_locked(&srv, vp_in.as_ref(), lease.slot())?;
                unsafe {
                    fv += 1;
                    d3d11.context4.Signal(&fence, fv)?;
                    fence.SetEventOnCompletion(fv, ev)?;
                    WaitForSingleObject(ev, INFINITE);
                }
            }

            // Per-frame sync timing (latency) + pipelined throughput
            let m0 = proc_metrics();
            let t0 = Instant::now();
            let mut lats = Vec::with_capacity(iters);
            for _ in 0..iters {
                let f0 = Instant::now();
                let _g = d3d11.lock_immediate_context();
                conv.convert_locked(&srv, vp_in.as_ref(), lease.slot())?;
                unsafe {
                    fv += 1;
                    d3d11.context4.Signal(&fence, fv)?;
                    fence.SetEventOnCompletion(fv, ev)?;
                    WaitForSingleObject(ev, INFINITE);
                }
                lats.push(f0.elapsed().as_secs_f64() * 1000.0);
            }
            let wall = t0.elapsed().as_secs_f64();
            let m1 = proc_metrics();
            print_summary(
                &format!("GPU {}", mode.label()),
                lats,
                wall,
                m0,
                m1,
                "CPU transfer: 0.0 MB/f",
            );
        }
        unsafe {
            let _ = CloseHandle(ev);
        }
        Ok(())
    }

    fn bench_qsv_matrix_4k(num_frames: usize) -> Result<()> {
        println!(
            "\n--- Stage C: Pure QSV 4K Zero-Copy Encoder Sweep ({} frames, GOP / AsyncDepth / TU / Codec) ---",
            num_frames
        );
        let d3d11 = SharedD3d11Context::get_or_create_intel()?;
        let pool = Nv12SurfacePool::new(d3d11.clone(), 3840, 2160, 16, None)?;
        let dir = tempfile::tempdir()?;

        let cases: &[(&str, VideoCodec, u32, u16, u16, u16, QsvLowPowerMode)] = &[
            ("4K60  H.264 VME   GOP=60  AD=1 TU=4", VideoCodec::H264, 60, 60, 1, 4, QsvLowPowerMode::Off),
            ("4K60  H.264 VME   GOP=60  AD=2 TU=4", VideoCodec::H264, 60, 60, 2, 4, QsvLowPowerMode::Off),
            ("4K60  H.264 VME   GOP=60  AD=4 TU=4", VideoCodec::H264, 60, 60, 4, 4, QsvLowPowerMode::Off),
            ("4K60  H.264 VME   GOP=60  AD=8 TU=4", VideoCodec::H264, 60, 60, 8, 4, QsvLowPowerMode::Off),
            ("4K60  H.264 VME   GOP=30  AD=4 TU=4", VideoCodec::H264, 60, 30, 4, 4, QsvLowPowerMode::Off),
            ("4K60  H.264 VME   GOP=120 AD=4 TU=4", VideoCodec::H264, 60, 120, 4, 4, QsvLowPowerMode::Off),
            ("4K60  H.264 VME   GOP=240 AD=4 TU=4", VideoCodec::H264, 60, 240, 4, 4, QsvLowPowerMode::Off),
            ("4K60  H.264 VME   GOP=120 AD=4 TU=7 (fast)", VideoCodec::H264, 60, 120, 4, 7, QsvLowPowerMode::Off),
            ("4K120 H.264 VME   GOP=120 AD=4 TU=7 (fast)", VideoCodec::H264, 120, 120, 4, 7, QsvLowPowerMode::Off),
            ("4K60  H.264 VDENC GOP=120 AD=4 TU=4", VideoCodec::H264, 60, 120, 4, 4, QsvLowPowerMode::On),
            ("4K60  HEVC  VME   GOP=60  AD=4 TU=4", VideoCodec::Hevc, 60, 60, 4, 4, QsvLowPowerMode::Off),
            ("4K60  HEVC  VME   GOP=120 AD=4 TU=7 (fast)", VideoCodec::Hevc, 60, 120, 4, 7, QsvLowPowerMode::Off),
        ];

        for &(label, codec, fps, gop, ad, tu, lp) in cases {
            let out_mp4 = dir.path().join("sweep.mp4");
            let cfg = QsvEncodeConfig {
                codec,
                width: 3840,
                height: 2160,
                fps,
                target_kbps: 8000,
                max_kbps: 12000,
                gop_size: gop,
                gop_ref_dist: 1,
                async_depth: ad,
                target_usage: tu,
                low_power: lp,
                color_matrix: ColorMatrix::Bt709,
                color_range: ColorRange::Limited,
            };
            let mut enc = match WinQsvAsyncEncoder::new(&out_mp4, None, None, cfg) {
                Ok(e) => e,
                Err(err) => {
                    println!("  {:<46} | SKIP ({err:#})", label);
                    continue;
                }
            };
            let m0 = proc_metrics();
            let t0 = Instant::now();
            let mut lats = Vec::with_capacity(num_frames);
            for _ in 0..num_frames {
                let f0 = Instant::now();
                let lease = pool.acquire();
                let mut frame = RenderedFrame::new(2, 2);
                frame.set_win_gpu_frame(Some(libgoosy::win_qsv::WinGpuFrame {
                    lease,
                    width: 3840,
                    height: 2160,
                    aligned_width: 3840,
                    aligned_height: 2160,
                    color_matrix: ColorMatrix::Bt709,
                    color_range: ColorRange::Limited,
                    converter_used: Nv12ConverterMode::ComputeDirect,
                }));
                let _ = enc.submit_frame(frame)?;
                lats.push(f0.elapsed().as_secs_f64() * 1000.0);
            }
            enc.finish()?;
            let wall = t0.elapsed().as_secs_f64();
            let m1 = proc_metrics();
            let kb = std::fs::metadata(&out_mp4).map(|m| m.len() / 1024).unwrap_or(0);
            print_summary(label, lats, wall, m0, m1, &format!("mp4={} KB", kb));
        }
        Ok(())
    }

    fn bench_end_to_end(
        label: &str,
        width: u32,
        height: u32,
        fps: u32,
        num_frames: usize,
        warmup: usize,
        backend: EncoderBackend,
        codec: VideoCodec,
        env_overrides: &[(&str, Option<&str>)],
        cover_png: &[u8],
        lines: &[LyricLine],
        copy_note: &str,
    ) -> Result<()> {
        for &(k, v) in env_overrides {
            unsafe {
                match v {
                    Some(val) => std::env::set_var(k, val),
                    None => std::env::remove_var(k),
                }
            }
        }
        reset_win_qsv_telemetry();

        let dir = tempfile::tempdir()?;
        let out_mp4 = dir.path().join("bench.mp4");
        let asset = ImageAsset::from_bytes(cover_png)?;
        let bg = BackgroundRenderer::from_image_asset(&asset)?;
        let cover = CoverRenderer::from_image_asset(&asset, Some("Zoo (From Zootopia 2)".to_owned()))?;
        let mut renderer = Renderer::with_scene_options_for_encoder(
            width,
            height,
            fps,
            lines.to_vec(),
            bg,
            Some(cover),
            true,
            true,
            LyricsStyle::default(),
            &[],
            backend,
        )?;
        let mut sink = video::create_encoder_with_codec(
            backend,
            codec,
            &out_mp4,
            width,
            height,
            fps,
            None,
        )?;
        let require_cpu = sink.requires_cpu_pixels();
        let mut frame = RenderedFrame::new(width, height);
        let start_ms = 15_000u64;

        // Warmup
        for i in 0..warmup {
            let t_ms = start_ms + (i as u64) * 1000 / fps as u64;
            if require_cpu {
                renderer.render_frame_into(t_ms, &mut frame)?;
            } else {
                renderer.render_frame_into_opts(t_ms, &mut frame, false)?;
            }
            frame = sink.submit(frame)?;
        }

        let m0 = proc_metrics();
        let t0 = Instant::now();
        let mut lats = Vec::with_capacity(num_frames);
        let mut render_ms_sum = 0.0f64;
        let mut submit_ms_sum = 0.0f64;

        for i in 0..num_frames {
            let t_ms = start_ms + ((warmup + i) as u64) * 1000 / fps as u64;
            let f0 = Instant::now();
            if require_cpu {
                renderer.render_frame_into(t_ms, &mut frame)?;
            } else {
                renderer.render_frame_into_opts(t_ms, &mut frame, false)?;
            }
            let r_el = f0.elapsed().as_secs_f64() * 1000.0;
            render_ms_sum += r_el;
            let s0 = Instant::now();
            frame = sink.submit(frame)?;
            let s_el = s0.elapsed().as_secs_f64() * 1000.0;
            submit_ms_sum += s_el;
            lats.push(r_el + s_el);
        }
        let fin0 = Instant::now();
        sink.finish()?;
        let fin_ms = fin0.elapsed().as_secs_f64() * 1000.0;
        let wall = t0.elapsed().as_secs_f64();
        let m1 = proc_metrics();

        let telem = win_qsv_telemetry();
        let extra = format!(
            "ren={:.2}ms sub={:.2}ms fin={:.0}ms | {} | pool={}/{} in_flt={}",
            render_ms_sum / num_frames as f64,
            submit_ms_sum / num_frames as f64,
            fin_ms,
            copy_note,
            telem.gpu_frames_encoded,
            telem.pool_capacity,
            telem.in_flight_count
        );
        print_summary(label, lats, wall, m0, m1, &extra);
        Ok(())
    }

    fn bench_renderer_stages(
        width: u32,
        height: u32,
        fps: u32,
        num_frames: usize,
        cover_png: &[u8],
        lines: &[LyricLine],
    ) -> Result<()> {
        println!(
            "\n--- Stage A: Renderer-Only Breakdown ({}x{} @ {} FPS, {} frames) ---",
            width, height, fps, num_frames
        );
        let asset = ImageAsset::from_bytes(cover_png)?;
        let start_ms = 15_000u64;

        // 1. D3D12 Render + GPU->CPU read_pixels (Baseline render path)
        {
            unsafe { std::env::set_var("GOOSY_RENDER_BACKEND", "d3d12") };
            let bg = BackgroundRenderer::from_image_asset(&asset)?;
            let cover = CoverRenderer::from_image_asset(&asset, Some("Zoo (From Zootopia 2)".to_owned()))?;
            let mut r = Renderer::with_scene_options_for_encoder(
                width,
                height,
                fps,
                lines.to_vec(),
                bg,
                Some(cover),
                true,
                true,
                LyricsStyle::default(),
                &[],
                EncoderBackend::Pipe,
            )?;
            let mut frame = RenderedFrame::new(width, height);
            for i in 0..5 {
                r.render_frame_into(start_ms + i * 1000 / fps as u64, &mut frame)?;
            }
            let m0 = proc_metrics();
            let t0 = Instant::now();
            let mut lats = Vec::with_capacity(num_frames);
            for i in 0..num_frames {
                let f0 = Instant::now();
                r.render_frame_into(start_ms + (5 + i as u64) * 1000 / fps as u64, &mut frame)?;
                lats.push(f0.elapsed().as_secs_f64() * 1000.0);
            }
            let wall = t0.elapsed().as_secs_f64();
            let m1 = proc_metrics();
            let mb = (width as f64 * height as f64 * 4.0) / (1024.0 * 1024.0);
            print_summary(
                "D3D12 Render + GPU->CPU read_pixels",
                lats,
                wall,
                m0,
                m1,
                &format!("CPU readback {:.2} MB/f", mb),
            );
        }

        // 2. D3D12 Render + Shared GPU + D3D11 ComputeDirect NV12 (no encoder, no readback)
        for (tag, mode_env) in [
            ("D3D12 Render + D3D11 ComputeDirect NV12", "compute-direct"),
            ("D3D12 Render + D3D11 VideoProcessor NV12", "vp"),
        ] {
            unsafe {
                std::env::set_var("GOOSY_RENDER_BACKEND", "d3d12");
                std::env::set_var("GOOSY_QSV_CONVERTER", mode_env);
            }
            let bg = BackgroundRenderer::from_image_asset(&asset)?;
            let cover = CoverRenderer::from_image_asset(&asset, Some("Zoo (From Zootopia 2)".to_owned()))?;
            let mut r = Renderer::with_scene_options_for_encoder(
                width,
                height,
                fps,
                lines.to_vec(),
                bg,
                Some(cover),
                true,
                true,
                LyricsStyle::default(),
                &[],
                EncoderBackend::FfmpegNext,
            )?;
            let mut frame = RenderedFrame::new(width, height);
            for i in 0..5 {
                r.render_frame_into_opts(start_ms + i * 1000 / fps as u64, &mut frame, false)?;
                let _ = frame.take_win_gpu_frame();
            }
            let m0 = proc_metrics();
            let t0 = Instant::now();
            let mut lats = Vec::with_capacity(num_frames);
            for i in 0..num_frames {
                let f0 = Instant::now();
                r.render_frame_into_opts(
                    start_ms + (5 + i as u64) * 1000 / fps as u64,
                    &mut frame,
                    false,
                )?;
                let _ = frame.take_win_gpu_frame();
                lats.push(f0.elapsed().as_secs_f64() * 1000.0);
            }
            let wall = t0.elapsed().as_secs_f64();
            let m1 = proc_metrics();
            print_summary(tag, lats, wall, m0, m1, "CPU transfer: 0.0 MB/f");
        }
        Ok(())
    }

    pub fn run() -> Result<()> {
        let args: Vec<String> = std::env::args().collect();
        let cover_png = make_test_cover_png(1000);
        let lines = load_benchmark_lines()?;
        if let Some(pos) = args.iter().position(|a| a == "--gpu-load") {
            let mode = args.get(pos + 1).map(String::as_str).unwrap_or("vp-vdenc");
            let (label, env): (&str, Vec<(&str, Option<&str>)>) = match mode {
                "baseline" => (
                    "4K60 Baseline (D3D12->CPU->swscale->h264_qsv)",
                    vec![
                        ("GOOSY_RENDER_BACKEND", Some("d3d12")),
                        ("GOOSY_QSV_DISABLE_ZEROCOPY", Some("1")),
                    ],
                ),
                "cs-vme" => (
                    "4K60 Zero-Copy (CS-Direct + H.264 VME TU=4)",
                    vec![
                        ("GOOSY_RENDER_BACKEND", Some("d3d12")),
                        ("GOOSY_QSV_DISABLE_ZEROCOPY", None),
                        ("GOOSY_QSV_CONVERTER", Some("compute-direct")),
                        ("GOOSY_QSV_LOW_POWER", Some("off")),
                        ("GOOSY_QSV_TU", Some("4")),
                    ],
                ),
                _ => (
                    "4K60 Zero-Copy (VideoProc + H.264 VDENC TU=4)",
                    vec![
                        ("GOOSY_RENDER_BACKEND", Some("d3d12")),
                        ("GOOSY_QSV_DISABLE_ZEROCOPY", None),
                        ("GOOSY_QSV_CONVERTER", Some("vp")),
                        ("GOOSY_QSV_LOW_POWER", Some("on")),
                        ("GOOSY_QSV_TU", Some("4")),
                    ],
                ),
            };
            let frames = if mode == "baseline" { 100 } else { 240 };
            bench_end_to_end(
                label,
                3840,
                2160,
                60,
                frames,
                15,
                EncoderBackend::FfmpegNext,
                VideoCodec::H264,
                &env,
                &cover_png,
                &lines,
                "gpu-load-profile",
            )?;
            return Ok(());
        }

        let tune_only = args.iter().any(|a| a == "--tune");

        if tune_only {
            println!("==========================================================================================");
            println!("  3-ENGINE CONTENTION & OPTIMIZATION SWEEP (4K60, 120 steady frames, 10 warmup)");
            println!("==========================================================================================");
            for &(label, conv, lp, tu, codec) in &[
                ("4K60 CS-Direct + H.264 VME   (TU=4)", "compute-direct", "off", "4", VideoCodec::H264),
                ("4K60 CS-Direct + H.264 VDENC (TU=4)", "compute-direct", "on",  "4", VideoCodec::H264),
                ("4K60 CS-Direct + H.264 VDENC (TU=7)", "compute-direct", "on",  "7", VideoCodec::H264),
                ("4K60 VideoProc + H.264 VME   (TU=4)", "vp",             "off", "4", VideoCodec::H264),
                ("4K60 VideoProc + H.264 VDENC (TU=4)", "vp",             "on",  "4", VideoCodec::H264),
                ("4K60 VideoProc + H.264 VDENC (TU=7)", "vp",             "on",  "7", VideoCodec::H264),
                ("4K60 VideoProc + HEVC  VME   (TU=4)", "vp",             "off", "4", VideoCodec::Hevc),
                ("4K60 VideoProc + HEVC  VME   (TU=7)", "vp",             "off", "7", VideoCodec::Hevc),
                ("4K60 CS-Direct + HEVC  VME   (TU=7)", "compute-direct", "off", "7", VideoCodec::Hevc),
            ] {
                bench_end_to_end(
                    label,
                    3840,
                    2160,
                    60,
                    120,
                    10,
                    EncoderBackend::FfmpegNext,
                    codec,
                    &[
                        ("GOOSY_RENDER_BACKEND", Some("d3d12")),
                        ("GOOSY_QSV_DISABLE_ZEROCOPY", None),
                        ("GOOSY_QSV_CONVERTER", Some(conv)),
                        ("GOOSY_QSV_LOW_POWER", Some(lp)),
                        ("GOOSY_QSV_TU", Some(tu)),
                        ("GOOSY_QSV_ASYNC_DEPTH", Some("4")),
                    ],
                    &cover_png,
                    &lines,
                    "CPU: 0.0 MB/f",
                )?;
            }
            return Ok(());
        }

        println!("==========================================================================================");
        println!("  GOOSY WINDOWS INTEL QSV ZERO-COPY / LESS-COPY BENCHMARK SUITE");
        println!("==========================================================================================");
        for codec in [VideoCodec::H264, VideoCodec::Hevc] {
            match probe_qsv_capabilities(codec, 3840, 2160, 60) {
                Ok(cap) => println!(
                    "QSV Probe 4K60 {:?}: {} (0x{:04X}:0x{:04X}) | MFX v{}.{} ({}) | UAV={} VP={} VME={} VDENC={}",
                    codec,
                    cap.adapter_name,
                    cap.vendor_id,
                    cap.device_id,
                    cap.mfx_version.major,
                    cap.mfx_version.minor,
                    cap.runtime_dll,
                    cap.supports_direct_nv12_uav,
                    cap.supports_vp_rgba_to_nv12,
                    cap.supports_vme,
                    cap.supports_vdenc
                ),
                Err(e) => println!("QSV Probe 4K60 {:?}: UNAVAILABLE ({e:#})", codec),
            }
        }

        // 4K Stage Breakdown
        bench_renderer_stages(3840, 2160, 60, 60, &cover_png, &lines)?;
        bench_color_converters(3840, 2160, 60)?;
        bench_qsv_matrix_4k(120)?;

        // 4K End-to-End Pipelines (60 FPS & 120 FPS)
        println!("\n==========================================================================================");
        println!("  END-TO-END 3840x2160 (4K) PIPELINE COMPARISON (120 steady frames, 10 warmup)");
        println!("==========================================================================================");
        for &(fps, tag) in &[(60u32, "4K60 "), (120u32, "4K120")] {
            let frames = 120;
            let warmup = 10;
            bench_end_to_end(
                &format!("{tag} Baseline (D3D12->CPU->swscale->h264_qsv)"),
                3840,
                2160,
                fps,
                frames,
                warmup,
                EncoderBackend::FfmpegNext,
                VideoCodec::H264,
                &[
                    ("GOOSY_RENDER_BACKEND", Some("d3d12")),
                    ("GOOSY_QSV_DISABLE_ZEROCOPY", Some("1")),
                ],
                &cover_png,
                &lines,
                "CPU: 78.8 MB/f (3 copies)",
            )?;
            bench_end_to_end(
                &format!("{tag} Less-Copy (D3D12->CS-Staged->QSV H.264)"),
                3840,
                2160,
                fps,
                frames,
                warmup,
                EncoderBackend::FfmpegNext,
                VideoCodec::H264,
                &[
                    ("GOOSY_RENDER_BACKEND", Some("d3d12")),
                    ("GOOSY_QSV_DISABLE_ZEROCOPY", None),
                    ("GOOSY_QSV_CONVERTER", Some("compute-intermediate")),
                    ("GOOSY_QSV_TU", Some("4")),
                    ("GOOSY_QSV_ASYNC_DEPTH", Some("4")),
                ],
                &cover_png,
                &lines,
                "CPU: 0.0 MB/f (+1 GPU pass)",
            )?;
            bench_end_to_end(
                &format!("{tag} Zero-Copy (D3D12->VP->QSV H.264)"),
                3840,
                2160,
                fps,
                frames,
                warmup,
                EncoderBackend::FfmpegNext,
                VideoCodec::H264,
                &[
                    ("GOOSY_RENDER_BACKEND", Some("d3d12")),
                    ("GOOSY_QSV_DISABLE_ZEROCOPY", None),
                    ("GOOSY_QSV_CONVERTER", Some("vp")),
                    ("GOOSY_QSV_TU", Some("4")),
                    ("GOOSY_QSV_ASYNC_DEPTH", Some("4")),
                ],
                &cover_png,
                &lines,
                "CPU: 0.0 MB/f (0 extra GPU)",
            )?;
            bench_end_to_end(
                &format!("{tag} Zero-Copy (D3D12->CS-Direct->QSV H.264)"),
                3840,
                2160,
                fps,
                frames,
                warmup,
                EncoderBackend::FfmpegNext,
                VideoCodec::H264,
                &[
                    ("GOOSY_RENDER_BACKEND", Some("d3d12")),
                    ("GOOSY_QSV_DISABLE_ZEROCOPY", None),
                    ("GOOSY_QSV_CONVERTER", Some("compute-direct")),
                    ("GOOSY_QSV_TU", Some("4")),
                    ("GOOSY_QSV_ASYNC_DEPTH", Some("4")),
                ],
                &cover_png,
                &lines,
                "CPU: 0.0 MB/f (0 extra GPU)",
            )?;
            if fps == 60 {
                bench_end_to_end(
                    &format!("{tag} Zero-Copy (D3D12->CS-Direct->QSV H.264 TU=7)"),
                    3840,
                    2160,
                    fps,
                    frames,
                    warmup,
                    EncoderBackend::FfmpegNext,
                    VideoCodec::H264,
                    &[
                        ("GOOSY_RENDER_BACKEND", Some("d3d12")),
                        ("GOOSY_QSV_DISABLE_ZEROCOPY", None),
                        ("GOOSY_QSV_CONVERTER", Some("compute-direct")),
                        ("GOOSY_QSV_TU", Some("7")),
                        ("GOOSY_QSV_ASYNC_DEPTH", Some("4")),
                    ],
                    &cover_png,
                    &lines,
                    "CPU: 0.0 MB/f (0 extra GPU)",
                )?;
                bench_end_to_end(
                    &format!("{tag} Baseline (D3D12->CPU->swscale->hevc_qsv)"),
                    3840,
                    2160,
                    fps,
                    60,
                    5,
                    EncoderBackend::FfmpegNext,
                    VideoCodec::Hevc,
                    &[
                        ("GOOSY_RENDER_BACKEND", Some("d3d12")),
                        ("GOOSY_QSV_DISABLE_ZEROCOPY", Some("1")),
                    ],
                    &cover_png,
                    &lines,
                    "CPU: 78.8 MB/f (3 copies)",
                )?;
                bench_end_to_end(
                    &format!("{tag} Zero-Copy (D3D12->CS-Direct->QSV HEVC)"),
                    3840,
                    2160,
                    fps,
                    60,
                    5,
                    EncoderBackend::FfmpegNext,
                    VideoCodec::Hevc,
                    &[
                        ("GOOSY_RENDER_BACKEND", Some("d3d12")),
                        ("GOOSY_QSV_DISABLE_ZEROCOPY", None),
                        ("GOOSY_QSV_CONVERTER", Some("compute-direct")),
                        ("GOOSY_QSV_TU", Some("4")),
                        ("GOOSY_QSV_ASYNC_DEPTH", Some("4")),
                    ],
                    &cover_png,
                    &lines,
                    "CPU: 0.0 MB/f (0 extra GPU)",
                )?;
                bench_end_to_end(
                    &format!("{tag} Fallback (Raster CPU->QSV GPU Upload)"),
                    3840,
                    2160,
                    fps,
                    30,
                    3,
                    EncoderBackend::FfmpegNext,
                    VideoCodec::H264,
                    &[
                        ("GOOSY_RENDER_BACKEND", Some("raster")),
                        ("GOOSY_QSV_DISABLE_ZEROCOPY", None),
                        ("GOOSY_QSV_CONVERTER", Some("compute-direct")),
                        ("GOOSY_QSV_TU", Some("4")),
                    ],
                    &cover_png,
                    &lines,
                    "CPU->GPU: 33.2 MB/f",
                )?;
            }
        }

        // 1080p End-to-End Comparison
        println!("\n==========================================================================================");
        println!("  END-TO-END 1920x1080 (1080p) PIPELINE COMPARISON (180 steady frames, 15 warmup)");
        println!("==========================================================================================");
        bench_end_to_end(
            "1080p60 Baseline (D3D12->CPU->swscale->h264_qsv)",
            1920,
            1080,
            60,
            180,
            15,
            EncoderBackend::FfmpegNext,
            VideoCodec::H264,
            &[
                ("GOOSY_RENDER_BACKEND", Some("d3d12")),
                ("GOOSY_QSV_DISABLE_ZEROCOPY", Some("1")),
            ],
            &cover_png,
            &lines,
            "CPU: 19.7 MB/f (3 copies)",
        )?;
        bench_end_to_end(
            "1080p60 Zero-Copy (D3D12->CS-Direct->QSV H.264)",
            1920,
            1080,
            60,
            180,
            15,
            EncoderBackend::FfmpegNext,
            VideoCodec::H264,
            &[
                ("GOOSY_RENDER_BACKEND", Some("d3d12")),
                ("GOOSY_QSV_DISABLE_ZEROCOPY", None),
                ("GOOSY_QSV_CONVERTER", Some("compute-direct")),
                ("GOOSY_QSV_TU", Some("4")),
                ("GOOSY_QSV_ASYNC_DEPTH", Some("4")),
            ],
            &cover_png,
            &lines,
            "CPU: 0.0 MB/f (0 extra GPU)",
        )?;
        bench_end_to_end(
            "1080p60 Zero-Copy (D3D12->CS-Direct->QSV HEVC)",
            1920,
            1080,
            60,
            180,
            15,
            EncoderBackend::FfmpegNext,
            VideoCodec::Hevc,
            &[
                ("GOOSY_RENDER_BACKEND", Some("d3d12")),
                ("GOOSY_QSV_DISABLE_ZEROCOPY", None),
                ("GOOSY_QSV_CONVERTER", Some("compute-direct")),
                ("GOOSY_QSV_TU", Some("4")),
                ("GOOSY_QSV_ASYNC_DEPTH", Some("4")),
            ],
            &cover_png,
            &lines,
            "CPU: 0.0 MB/f (0 extra GPU)",
        )?;

        Ok(())
    }
}

#[cfg(target_os = "windows")]
fn main() -> anyhow::Result<()> {
    win_bench::run()
}
