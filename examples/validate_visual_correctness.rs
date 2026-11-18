#[cfg(target_os = "macos")]
mod macos_impl {
    use anyhow::{Context, Result};
    use libgoosy::{
        EncoderBackend, LyricsStyle, RenderedFrame, VideoCodec, background::BackgroundRenderer,
        cover_renderer::CoverRenderer, geometry::FrameGeometry, image_asset::ImageAsset,
        layout::Layout, lyrics_renderer::LyricsRenderer, renderer::Renderer, ttml, video,
    };
    use skia_safe::{AlphaType, Color, ColorType, EncodedImageFormat, IPoint, ImageInfo, Pixmap};

    #[cfg(target_os = "macos")]
    use objc2::rc::Retained;
    #[cfg(target_os = "macos")]
    use objc2::runtime::ProtocolObject;
    #[cfg(target_os = "macos")]
    use objc2_metal::{MTLCommandQueue, MTLCreateSystemDefaultDevice, MTLDevice};

    #[derive(Debug, Clone)]
    struct DiffStats {
        psnr_db: f64,
        mae: f64,
        rmse: f64,
        max_diff: u8,
        exact_match_pct: f64,
        within_1_pct: f64,
        within_2_pct: f64,
    }

    fn compute_diff(ref_rgba: &[u8], opt_rgba: &[u8]) -> DiffStats {
        assert_eq!(ref_rgba.len(), opt_rgba.len());
        let num_pixels = ref_rgba.len() / 4;
        let mut sum_abs: u64 = 0;
        let mut sum_sq: u64 = 0;
        let mut max_diff: u8 = 0;
        let mut exact_pixels: usize = 0;
        let mut within_1_pixels: usize = 0;
        let mut within_2_pixels: usize = 0;

        for (r_px, o_px) in ref_rgba.chunks_exact(4).zip(opt_rgba.chunks_exact(4)) {
            let mut px_max: u8 = 0;
            for c in 0..4 {
                let d = r_px[c].abs_diff(o_px[c]);
                sum_abs += d as u64;
                sum_sq += (d as u64) * (d as u64);
                if d > px_max {
                    px_max = d;
                }
            }
            if px_max > max_diff {
                max_diff = px_max;
            }
            if px_max == 0 {
                exact_pixels += 1;
            }
            if px_max <= 1 {
                within_1_pixels += 1;
            }
            if px_max <= 2 {
                within_2_pixels += 1;
            }
        }

        let total_channels = (num_pixels * 4) as f64;
        let mae = sum_abs as f64 / total_channels;
        let mse = sum_sq as f64 / total_channels;
        let rmse = mse.sqrt();
        let psnr_db = if mse == 0.0 {
            f64::INFINITY
        } else {
            10.0 * ((255.0 * 255.0) / mse).log10()
        };

        DiffStats {
            psnr_db,
            mae,
            rmse,
            max_diff,
            exact_match_pct: (exact_pixels as f64 / num_pixels as f64) * 100.0,
            within_1_pct: (within_1_pixels as f64 / num_pixels as f64) * 100.0,
            within_2_pct: (within_2_pixels as f64 / num_pixels as f64) * 100.0,
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

    fn print_row(label: &str, stats: &DiffStats) {
        let psnr_str = if stats.psnr_db.is_infinite() {
            "      INF".to_owned()
        } else {
            format!("{:>9.2}", stats.psnr_db)
        };
        println!(
            "{:<50} | {} | {:>7.4} | {:>7.4} | {:>7} | {:>8.2}% | {:>8.2}% | {:>8.2}%",
            label,
            psnr_str,
            stats.mae,
            stats.rmse,
            stats.max_diff,
            stats.exact_match_pct,
            stats.within_1_pct,
            stats.within_2_pct
        );
    }

    pub fn run() -> Result<()> {
        let width = 3840u32;
        let height = 2160u32;
        let fps = 120u32;

        let cover_png = make_test_cover_png(1000);
        let asset = ImageAsset::from_bytes(&cover_png)?;
        let ttml_text = std::fs::read_to_string("testdata/zoo.ttml")?;
        let lines = ttml::parse_ttml(&ttml_text)?;

        #[cfg(target_os = "macos")]
        let mut gpu_context = {
            let device: Retained<ProtocolObject<dyn MTLDevice>> =
                MTLCreateSystemDefaultDevice().context("no Metal device")?;
            let queue: Retained<ProtocolObject<dyn MTLCommandQueue>> =
                device.newCommandQueue().context("no Metal command queue")?;
            let backend = unsafe {
                skia_safe::gpu::mtl::BackendContext::new(
                    Retained::as_ptr(&device) as skia_safe::gpu::mtl::Handle,
                    Retained::as_ptr(&queue) as skia_safe::gpu::mtl::Handle,
                )
            };
            skia_safe::gpu::direct_contexts::make_metal(&backend, None)
                .context("create Metal DirectContext")?
        };

        let info = ImageInfo::new(
            (width as i32, height as i32),
            ColorType::RGBA8888,
            AlphaType::Premul,
            None,
        );
        let mut surface_ref = skia_safe::gpu::surfaces::render_target(
            &mut gpu_context,
            skia_safe::gpu::Budgeted::Yes,
            &info,
            None,
            skia_safe::gpu::SurfaceOrigin::TopLeft,
            None,
            false,
            None,
        )
        .context("create ref surface")?;
        let mut surface_opt = skia_safe::gpu::surfaces::render_target(
            &mut gpu_context,
            skia_safe::gpu::Budgeted::Yes,
            &info,
            None,
            skia_safe::gpu::SurfaceOrigin::TopLeft,
            None,
            false,
            None,
        )
        .context("create opt surface")?;

        let row_bytes = width as usize * 4;
        let mut buf_ref = vec![0u8; height as usize * row_bytes];
        let mut buf_opt = vec![0u8; height as usize * row_bytes];
        let geometry = FrameGeometry::for_frame(width, height);

        println!(
            "=============================================================================================================================="
        );
        println!("VISUAL CORRECTNESS VALIDATION AT 3840x2160 (Baseline vs. Optimized)");
        println!(
            "=============================================================================================================================="
        );
        println!(
            "{:<50} | {:>9} | {:>7} | {:>7} | {:>7} | {:>9} | {:>9} | {:>9}",
            "Visual Layer / Scene Test Case",
            "PSNR (dB)",
            "MAE",
            "RMSE",
            "MaxDiff",
            "Exact %",
            "Delta<=1%",
            "Delta<=2%"
        );
        println!("{}", "-".repeat(126));

        // 1. Blurred Background Comparison (Baseline 2x4K sigma=96 vs Optimized 1/8x sigma=12)
        let mut ref_bg = BackgroundRenderer::from_image_asset_baseline_reference(&asset)?;
        let mut opt_bg = BackgroundRenderer::from_image_asset(&asset)?;
        for &t_ms in &[0u64, 15_500, 22_350] {
            surface_ref.canvas().clear(Color::BLACK);
            ref_bg.draw(surface_ref.canvas(), &geometry, t_ms)?;
            surface_ref.read_pixels(&info, &mut buf_ref, row_bytes, IPoint::new(0, 0));

            surface_opt.canvas().clear(Color::BLACK);
            opt_bg.draw(surface_opt.canvas(), &geometry, t_ms)?;
            surface_opt.read_pixels(&info, &mut buf_opt, row_bytes, IPoint::new(0, 0));

            let stats = compute_diff(&buf_ref, &buf_opt);
            print_row(&format!("1. Blurred Background (t={t_ms}ms)"), &stats);
        }

        // 2. CoverRenderer Comparison (Uncached 1st-frame build vs Cached bounded draw)
        {
            let mut cover_fresh =
                CoverRenderer::from_image_asset(&asset, Some("Zoo (From Zootopia 2)".to_owned()))?;
            surface_ref.canvas().clear(Color::BLACK);
            cover_fresh.draw(surface_ref.canvas(), &geometry)?;
            surface_ref.read_pixels(&info, &mut buf_ref, row_bytes, IPoint::new(0, 0));

            surface_opt.canvas().clear(Color::BLACK);
            cover_fresh.draw(surface_opt.canvas(), &geometry)?;
            surface_opt.read_pixels(&info, &mut buf_opt, row_bytes, IPoint::new(0, 0));

            let stats = compute_diff(&buf_ref, &buf_opt);
            print_row("2. CoverRenderer (Direct vs Bounded Cache)", &stats);
        }

        // 3. LyricsRenderer Comparison across transitions, feather masks, animated word lift, and blurred groups
        {
            let lyrics_renderer = LyricsRenderer::new_with_options(
                &lines,
                geometry.lyrics,
                true,
                true,
                LyricsStyle::default(),
            )?;
            let mut layout = Layout::new(
                &lines,
                geometry.lyrics.height,
                lyrics_renderer.group_heights(),
                lyrics_renderer.group_gap(),
                lyrics_renderer.interlude_slot_height(),
            );

            // Step layout from 14_000ms to 23_000ms at 120 FPS so spring physics & scroll transitions are active
            let checkpoints = [
                (15_050u64, "3a. Lyrics: Line Scroll Transition + Blur"),
                (15_600u64, "3b. Lyrics: Active Word Feather Mask"),
                (16_450u64, "3c. Lyrics: Subpixel Word Lift + Emphasis"),
                (18_100u64, "3d. Lyrics: Multi-word Lift + Bg Vocal"),
                (22_750u64, "3e. Lyrics: Fast Syllable Sweep + Inactive Blur"),
            ];
            let mut next_cp = 0;
            let mut step_idx = 0u64;
            while next_cp < checkpoints.len() {
                let t_ms = 14_000 + (step_idx * 1000) / fps as u64;
                layout.update(&lines, t_ms, fps);
                if t_ms >= checkpoints[next_cp].0 {
                    let (_, label) = checkpoints[next_cp];
                    surface_ref.canvas().clear(Color::BLACK);
                    lyrics_renderer.draw_unoptimized_reference(
                        surface_ref.canvas(),
                        &lines,
                        &layout,
                        t_ms,
                        height,
                    )?;
                    surface_ref.read_pixels(&info, &mut buf_ref, row_bytes, IPoint::new(0, 0));

                    // Run twice so blurred_group_cache is populated and then read from cache
                    surface_opt.canvas().clear(Color::BLACK);
                    lyrics_renderer.draw(surface_opt.canvas(), &lines, &layout, t_ms, height)?;
                    surface_opt.canvas().clear(Color::BLACK);
                    lyrics_renderer.draw(surface_opt.canvas(), &lines, &layout, t_ms, height)?;
                    surface_opt.read_pixels(&info, &mut buf_opt, row_bytes, IPoint::new(0, 0));

                    let stats = compute_diff(&buf_ref, &buf_opt);
                    print_row(label, &stats);
                    next_cp += 1;
                }
                step_idx += 1;
            }
        }

        // 4. Full Composite Scene (Baseline Reference Background + Cover + Unoptimized Lyrics vs Optimized Full Scene)
        {
            let mut ref_bg = BackgroundRenderer::from_image_asset_baseline_reference(&asset)?;
            let mut opt_bg = BackgroundRenderer::from_image_asset(&asset)?;
            let mut ref_cover =
                CoverRenderer::from_image_asset(&asset, Some("Zoo (From Zootopia 2)".to_owned()))?;
            let mut opt_cover =
                CoverRenderer::from_image_asset(&asset, Some("Zoo (From Zootopia 2)".to_owned()))?;
            let lyrics_renderer = LyricsRenderer::new_with_options(
                &lines,
                geometry.lyrics,
                true,
                true,
                LyricsStyle::default(),
            )?;
            let mut layout = Layout::new(
                &lines,
                geometry.lyrics.height,
                lyrics_renderer.group_heights(),
                lyrics_renderer.group_gap(),
                lyrics_renderer.interlude_slot_height(),
            );
            for i in 0..=300 {
                let t_ms = 15_000 + (i * 1000) / fps as u64;
                layout.update(&lines, t_ms, fps);
            }
            let t_ms = 17_500;
            surface_ref.canvas().clear(Color::BLACK);
            ref_bg.draw(surface_ref.canvas(), &geometry, t_ms)?;
            ref_cover.draw(surface_ref.canvas(), &geometry)?;
            lyrics_renderer.draw_unoptimized_reference(
                surface_ref.canvas(),
                &lines,
                &layout,
                t_ms,
                height,
            )?;
            surface_ref.read_pixels(&info, &mut buf_ref, row_bytes, IPoint::new(0, 0));

            surface_opt.canvas().clear(Color::BLACK);
            opt_bg.draw(surface_opt.canvas(), &geometry, t_ms)?;
            opt_cover.draw(surface_opt.canvas(), &geometry)?;
            lyrics_renderer.draw(surface_opt.canvas(), &lines, &layout, t_ms, height)?;
            surface_opt.read_pixels(&info, &mut buf_opt, row_bytes, IPoint::new(0, 0));

            let stats = compute_diff(&buf_ref, &buf_opt);
            print_row("4. Full Composite Scene (t=17500ms)", &stats);
        }

        // 5. End-to-End Encoded Output Comparison: Metal->IOSurface->VideoToolbox H.264 & HEVC vs Skia RGBA
        {
            for (codec, label) in [
                (
                    VideoCodec::H264,
                    "5a. E2E Encoded MP4 (H.264 VideoToolbox vs RGBA)",
                ),
                (
                    VideoCodec::Hevc,
                    "5b. E2E Encoded MP4 (HEVC  VideoToolbox vs RGBA)",
                ),
            ] {
                let bg = BackgroundRenderer::from_image_asset(&asset)?;
                let cover = CoverRenderer::from_image_asset(
                    &asset,
                    Some("Zoo (From Zootopia 2)".to_owned()),
                )?;
                let mut renderer = Renderer::with_scene_options(
                    width,
                    height,
                    fps,
                    lines.clone(),
                    bg,
                    Some(cover),
                    true,
                    true,
                    LyricsStyle::default(),
                    &[],
                )?;
                let out_mp4 = std::env::temp_dir()
                    .join(format!("goosy_visual_verify_{}.mp4", codec.cli_value()));
                let mut sink = video::create_encoder_with_codec(
                    EncoderBackend::FfmpegNext,
                    codec,
                    &out_mp4,
                    width,
                    height,
                    fps,
                    None,
                )?;
                let mut frame = RenderedFrame::new(width, height);
                let mut target_rgba = Vec::new();
                for i in 0..30u64 {
                    let t_ms = 16_000 + (i * 1000) / fps as u64;
                    let need_cpu = i == 29;
                    renderer.render_frame_into_opts(t_ms, &mut frame, need_cpu)?;
                    if need_cpu {
                        target_rgba = frame.pixels().to_vec();
                    }
                    frame = sink.submit(frame)?;
                }
                sink.finish()?;

                // Decode frame 29 from the encoded MP4 using ffmpeg to raw RGBA
                let raw_out = std::process::Command::new("ffmpeg")
                    .args(["-v", "error", "-i"])
                    .arg(&out_mp4)
                    .args([
                        "-vf",
                        "select=eq(n\\,29)",
                        "-frames:v",
                        "1",
                        "-f",
                        "rawvideo",
                        "-pix_fmt",
                        "rgba",
                        "pipe:1",
                    ])
                    .output()
                    .context("decode frame 29 with ffmpeg")?;
                let _ = std::fs::remove_file(&out_mp4);
                anyhow::ensure!(
                    raw_out.stdout.len() == target_rgba.len(),
                    "decoded frame size mismatch: got {}, expected {}",
                    raw_out.stdout.len(),
                    target_rgba.len()
                );
                let stats = compute_diff(&target_rgba, &raw_out.stdout);
                print_row(label, &stats);
            }
        }

        Ok(())
    }
}

fn main() -> anyhow::Result<()> {
    #[cfg(target_os = "macos")]
    macos_impl::run()?;
    Ok(())
}
