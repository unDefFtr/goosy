#[cfg(target_os = "macos")]
mod macos_impl {
    use anyhow::{Context, Result};
    use libgoosy::{
        EncoderBackend, LyricsStyle, RenderedFrame, VideoCodec,
        background::BackgroundRenderer,
        cover_renderer::CoverRenderer,
        image_asset::ImageAsset,
        renderer::Renderer,
        surface::{macos_pool_telemetry, reset_macos_pool_telemetry},
        ttml, video,
    };
    use skia_safe::{EncodedImageFormat, ImageInfo, Pixmap};
    use std::path::Path;
    use std::time::Instant;

    #[cfg(target_os = "macos")]
    use objc2::msg_send;
    #[cfg(target_os = "macos")]
    use objc2::runtime::{AnyClass, AnyObject};
    #[cfg(target_os = "macos")]
    use objc2_metal::{MTLCreateSystemDefaultDevice, MTLDevice};

    #[derive(Debug, Clone, Copy)]
    struct ProcessMetrics {
        resident_mb: f64,
        max_rss_mb: f64,
    }

    #[cfg(target_os = "macos")]
    fn get_process_metrics() -> ProcessMetrics {
        unsafe {
            #[repr(C)]
            struct mach_task_basic_info {
                virtual_size: u64,
                resident_size: u64,
                resident_size_max: u64,
                user_time: [u32; 2],
                system_time: [u32; 2],
                policy: i32,
                suspend_count: i32,
            }
            unsafe extern "C" {
                fn mach_task_self() -> u32;
                fn task_info(
                    target_task: u32,
                    flavor: u32,
                    task_info_out: *mut mach_task_basic_info,
                    task_info_outCnt: *mut u32,
                ) -> i32;
            }
            const MACH_TASK_BASIC_INFO: u32 = 20;
            let mut info: mach_task_basic_info = std::mem::zeroed();
            let mut count =
                (std::mem::size_of::<mach_task_basic_info>() / std::mem::size_of::<u32>()) as u32;
            let kr = task_info(
                mach_task_self(),
                MACH_TASK_BASIC_INFO,
                &mut info,
                &mut count,
            );
            if kr == 0 {
                ProcessMetrics {
                    resident_mb: info.resident_size as f64 / (1024.0 * 1024.0),
                    max_rss_mb: info.resident_size_max as f64 / (1024.0 * 1024.0),
                }
            } else {
                ProcessMetrics {
                    resident_mb: 0.0,
                    max_rss_mb: 0.0,
                }
            }
        }
    }

    #[cfg(target_os = "macos")]
    fn get_thermal_state() -> (isize, &'static str) {
        unsafe {
            let cls = AnyClass::get(c"NSProcessInfo").unwrap();
            let info: *mut AnyObject = msg_send![cls, processInfo];
            let state: isize = msg_send![info, thermalState];
            let name = match state {
                0 => "Nominal(0)",
                1 => "Fair(1)",
                2 => "Serious(2)",
                3 => "Critical(3)",
                _ => "Unknown",
            };
            (state, name)
        }
    }

    fn make_test_cover_png(size: i32) -> Vec<u8> {
        let info = ImageInfo::new_n32_premul((size, size), None);
        let mut pixels = vec![0u8; (size * size * 4) as usize];
        for y in 0..size {
            for x in 0..size {
                let idx = ((y * size + x) * 4) as usize;
                pixels[idx] = ((x * 255) / size) as u8;
                pixels[idx + 1] = ((y * 255) / size) as u8;
                pixels[idx + 2] = (((size - x) * 200) / size) as u8;
                pixels[idx + 3] = 255;
            }
        }
        let pixmap = Pixmap::new(&info, &mut pixels, (size * 4) as usize).unwrap();
        pixmap.encode(EncodedImageFormat::PNG, 100).unwrap()
    }

    pub fn run() -> Result<()> {
        let target_duration_secs = std::env::args()
            .nth(1)
            .and_then(|s| s.parse::<f64>().ok())
            .unwrap_or(605.0);

        let width = 3840u32;
        let height = 2160u32;
        let fps = 120u32;

        let cover_png = make_test_cover_png(1000);
        let asset = ImageAsset::from_bytes(&cover_png)?;
        let ttml_text = std::fs::read_to_string(Path::new("testdata/zoo.ttml"))?;
        let lines = ttml::parse_ttml(&ttml_text)?;
        let song_cycle_ms = lines
            .iter()
            .map(|l| l.end_ms)
            .max()
            .unwrap_or(60_000)
            .max(10_000);

        reset_macos_pool_telemetry();

        let out_path = std::env::temp_dir().join("goosy_stability_10min_4k120.mp4");
        let _ = std::fs::remove_file(&out_path);

        let device = MTLCreateSystemDefaultDevice().context("no Metal device")?;
        let m_init = get_process_metrics();
        let gpu_init_mb = device.currentAllocatedSize() as f64 / (1024.0 * 1024.0);

        println!(
            "========================================================================================================================================="
        );
        println!(
            "10-MINUTE EXTENDED STABILITY TEST: 3840x2160 @ 120 FPS (Metal -> IOSurface CVPixelBufferPool -> VideoToolbox HEVC)"
        );
        println!(
            "Initial State: RSS = {:.1} MB | GPU Allocated = {:.1} MB | Target Duration = {:.0}s",
            m_init.resident_mb, gpu_init_mb, target_duration_secs
        );
        println!(
            "========================================================================================================================================="
        );
        println!(
            "{:>7} | {:>8} | {:>10} | {:>10} | {:>9} | {:>9} | {:>8} | {:>10} | {:>10} | {:>8} | {:>8} | {:>11}",
            "Time(s)",
            "Frames",
            "Win FPS",
            "Cum FPS",
            "RSS(MB)",
            "MaxRSS",
            "GPU(MB)",
            "PoolAcq",
            "PoolRel",
            "InFlight",
            "UniqIOS",
            "Thermal"
        );
        println!("{}", "-".repeat(137));

        let total_frames_encoded: u64;
        let loop_elapsed_s: f64;
        let finish_ms: f64;

        {
            let bg = BackgroundRenderer::from_image_asset(&asset)?;
            let cover =
                CoverRenderer::from_image_asset(&asset, Some("Zoo (From Zootopia 2)".to_owned()))?;
            let mut renderer = Renderer::with_scene_options(
                width,
                height,
                fps,
                lines,
                bg,
                Some(cover),
                true,
                true,
                LyricsStyle::default(),
                &[],
            )?;
            let mut sink = video::create_encoder_with_codec(
                EncoderBackend::FfmpegNext,
                VideoCodec::Hevc,
                &out_path,
                width,
                height,
                fps,
                None,
            )?;
            let mut frame = RenderedFrame::new(width, height);

            let start = Instant::now();
            let mut last_report_time = 0.0f64;
            let mut last_report_frames = 0u64;
            let mut frame_idx = 0u64;
            let report_interval = 30.0f64;

            loop {
                let t_ms = ((frame_idx * 1000) / fps as u64) % song_cycle_ms;
                renderer.render_frame_into_opts(t_ms, &mut frame, false)?;
                frame = sink.submit(frame)?;
                frame_idx += 1;

                let elapsed = start.elapsed().as_secs_f64();
                if elapsed - last_report_time >= report_interval || elapsed >= target_duration_secs
                {
                    let win_dt = (elapsed - last_report_time).max(1e-6);
                    let win_frames = frame_idx - last_report_frames;
                    let win_fps = win_frames as f64 / win_dt;
                    let cum_fps = frame_idx as f64 / elapsed.max(1e-6);
                    let m = get_process_metrics();
                    let gpu_mb = device.currentAllocatedSize() as f64 / (1024.0 * 1024.0);
                    let pool = macos_pool_telemetry();
                    let (_, thermal_str) = get_thermal_state();

                    println!(
                        "{:>6.1}s | {:>8} | {:>10.2} | {:>10.2} | {:>9.1} | {:>9.1} | {:>8.1} | {:>10} | {:>10} | {:>8} | {:>8} | {:>11}",
                        elapsed,
                        frame_idx,
                        win_fps,
                        cum_fps,
                        m.resident_mb,
                        m.max_rss_mb,
                        gpu_mb,
                        pool.acquired_count,
                        pool.released_count,
                        pool.in_flight_count,
                        pool.unique_iosurface_count,
                        thermal_str
                    );
                    use std::io::Write;
                    let _ = std::io::stdout().flush();

                    last_report_time = elapsed;
                    last_report_frames = frame_idx;
                    if elapsed >= target_duration_secs {
                        break;
                    }
                }
            }

            loop_elapsed_s = start.elapsed().as_secs_f64();
            total_frames_encoded = frame_idx;

            let finish_t0 = Instant::now();
            sink.finish()?;
            finish_ms = finish_t0.elapsed().as_secs_f64() * 1000.0;
        }

        let pool_after = macos_pool_telemetry();
        let m_after = get_process_metrics();
        let gpu_after_drop_mb = device.currentAllocatedSize() as f64 / (1024.0 * 1024.0);

        println!("{}", "-".repeat(137));
        println!(
            "Post-Finish & Drop Telemetry: Frames={} | Loop={:.2}s ({:.2} FPS) | FinishFlush={:.2}ms | Pool Acq={} Rel={} InFlight={} UniqueIOSurfaces={} | RSS={:.1} MB | GPU After Drop={:.1} MB",
            total_frames_encoded,
            loop_elapsed_s,
            total_frames_encoded as f64 / loop_elapsed_s,
            finish_ms,
            pool_after.acquired_count,
            pool_after.released_count,
            pool_after.in_flight_count,
            pool_after.unique_iosurface_count,
            m_after.resident_mb,
            gpu_after_drop_mb
        );

        // Verify encoded MP4 header & packet count via ffprobe
        let probe = std::process::Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-count_packets",
                "-select_streams",
                "v:0",
                "-show_entries",
                "stream=codec_name,codec_tag_string,width,height,nb_read_packets,r_frame_rate",
                "-of",
                "json",
            ])
            .arg(&out_path)
            .output()
            .context("run ffprobe on 10-min stability MP4")?;
        anyhow::ensure!(
            probe.status.success(),
            "ffprobe failed on {}",
            out_path.display()
        );
        let doc: serde_json::Value = serde_json::from_slice(&probe.stdout)?;
        let stream = &doc["streams"][0];
        let packets: u64 = stream["nb_read_packets"]
            .as_str()
            .unwrap_or("0")
            .parse()
            .unwrap_or(0);
        let file_mb = std::fs::metadata(&out_path)?.len() as f64 / (1024.0 * 1024.0);
        println!(
            "Verified MP4 Output: codec={} tag={} {}x{} packets={}/{} r_frame_rate={} size={:.1} MB",
            stream["codec_name"].as_str().unwrap_or("?"),
            stream["codec_tag_string"].as_str().unwrap_or("?"),
            stream["width"].as_u64().unwrap_or(0),
            stream["height"].as_u64().unwrap_or(0),
            packets,
            total_frames_encoded,
            stream["r_frame_rate"].as_str().unwrap_or("?"),
            file_mb
        );
        let _ = std::fs::remove_file(&out_path);
        anyhow::ensure!(
            packets == total_frames_encoded,
            "packet count mismatch: got {packets}, expected {total_frames_encoded}"
        );
        anyhow::ensure!(
            pool_after.in_flight_count == 0,
            "CVPixelBuffer leak detected: {} buffers still in flight",
            pool_after.in_flight_count
        );

        Ok(())
    }
}

fn main() -> anyhow::Result<()> {
    #[cfg(target_os = "macos")]
    macos_impl::run()?;
    Ok(())
}
