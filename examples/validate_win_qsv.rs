#[cfg(not(target_os = "windows"))]
fn main() {
    eprintln!("validate_win_qsv is Windows-only");
}

#[cfg(target_os = "windows")]
mod win_validate {
    use anyhow::{Context, Result, bail};
    use libgoosy::{
        EncoderBackend, LyricsStyle, RenderedFrame, Renderer, VideoCodec,
        background::BackgroundRenderer,
        cover_renderer::CoverRenderer,
        image_asset::ImageAsset,
        lrc::{BackgroundVocal, LyricLine, LyricWord},
        ttml, video,
        win_qsv::{reset_win_qsv_telemetry, win_qsv_telemetry},
    };
    use skia_safe::{EncodedImageFormat, ImageInfo, Pixmap};
    use std::path::Path;
    use std::process::Command;

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

    fn load_lines() -> Result<Vec<LyricLine>> {
        let text = std::fs::read_to_string("testdata/zoo.ttml")?;
        let mut lines = ttml::parse_ttml(&text)?;
        for (i, line) in lines.iter_mut().enumerate() {
            if i % 3 == 1 && line.background_vocal.is_none() {
                line.background_vocal = Some(BackgroundVocal {
                    start_ms: line.start_ms + 100,
                    end_ms: line.end_ms.saturating_sub(100).max(line.start_ms + 200),
                    text: "Background vocal harmony (伴唱测试)".to_owned(),
                    translation: Some("伴唱翻译测试".to_owned()),
                    words: vec![LyricWord {
                        start_ms: line.start_ms + 100,
                        end_ms: line.end_ms.saturating_sub(100).max(line.start_ms + 200),
                        text: "Background vocal harmony".to_owned(),
                    }],
                });
            }
        }
        Ok(lines)
    }

    fn decode_mp4_frame_rgba(mp4: &Path, frame_idx: usize, width: u32, height: u32) -> Result<Vec<u8>> {
        let output = Command::new("ffmpeg")
            .args(["-v", "error", "-i"])
            .arg(mp4)
            .args([
                "-vf",
                &format!("select=eq(n\\,{frame_idx})"),
                "-vframes",
                "1",
                "-f",
                "rawvideo",
                "-pix_fmt",
                "rgba",
                "pipe:1",
            ])
            .output()
            .context("run ffmpeg to decode frame")?;
        if !output.status.success() {
            bail!(
                "ffmpeg decode failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        let expected = width as usize * height as usize * 4;
        if output.stdout.len() != expected {
            bail!(
                "decoded frame size mismatch: got {}, expected {}",
                output.stdout.len(),
                expected
            );
        }
        Ok(output.stdout)
    }

    fn probe_mp4_summary(mp4: &Path) -> Result<String> {
        let out = Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-select_streams",
                "v:0",
                "-count_frames",
                "-show_entries",
                "stream=codec_name,codec_tag_string,width,height,pix_fmt,color_space,color_range,nb_read_frames",
                "-of",
                "default=noprint_wrappers=1",
            ])
            .arg(mp4)
            .output()
            .context("run ffprobe")?;
        if !out.status.success() {
            bail!("ffprobe failed: {}", String::from_utf8_lossy(&out.stderr));
        }
        Ok(String::from_utf8_lossy(&out.stdout)
            .lines()
            .collect::<Vec<_>>()
            .join(", "))
    }

    fn compute_psnr_mae(a: &[u8], b: &[u8]) -> (f64, f64, u8) {
        assert_eq!(a.len(), b.len());
        let mut sum_sq = 0.0f64;
        let mut sum_abs = 0.0f64;
        let mut max_diff = 0u8;
        let mut count = 0u64;
        for (pa, pb) in a.chunks_exact(4).zip(b.chunks_exact(4)) {
            for c in 0..3 {
                let d = pa[c].abs_diff(pb[c]);
                if d > max_diff {
                    max_diff = d;
                }
                sum_abs += d as f64;
                sum_sq += (d as f64) * (d as f64);
                count += 1;
            }
        }
        let mae = sum_abs / count as f64;
        let mse = sum_sq / count as f64;
        let psnr = if mse == 0.0 {
            f64::INFINITY
        } else {
            10.0 * (255.0 * 255.0 / mse).log10()
        };
        (psnr, mae, max_diff)
    }

    fn validate_case(
        label: &str,
        width: u32,
        height: u32,
        fps: u32,
        codec: VideoCodec,
        env_overrides: &[(&str, Option<&str>)],
        expect_gpu_zero_copy: bool,
        cover_png: &[u8],
        lines: &[LyricLine],
        ref_frame_rgba: &[u8],
        check_frame_idx: usize,
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
        let out_mp4 = dir.path().join("out.mp4");
        let asset = ImageAsset::from_bytes(cover_png)?;
        let bg = BackgroundRenderer::from_image_asset(&asset)?;
        let cover = CoverRenderer::from_image_asset(&asset, Some("Zoo (Test)".to_owned()))?;
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
            EncoderBackend::FfmpegNext,
        )?;
        let mut sink = video::create_encoder_with_codec(
            EncoderBackend::FfmpegNext,
            codec,
            &out_mp4,
            width,
            height,
            fps,
            None,
        )?;

        let num_frames = 30usize;
        let start_ms = 15_000u64;
        let require_cpu = sink.requires_cpu_pixels();
        let mut frame = RenderedFrame::new(width, height);
        for i in 0..num_frames {
            let t_ms = start_ms + (i as u64) * 1000 / fps as u64;
            if require_cpu {
                renderer.render_frame_into(t_ms, &mut frame)?;
            } else {
                renderer.render_frame_into_opts(t_ms, &mut frame, false)?;
            }
            frame = sink.submit(frame)?;
        }
        sink.finish()?;

        let telem = win_qsv_telemetry();
        let probe = probe_mp4_summary(&out_mp4)?;
        let decoded = decode_mp4_frame_rgba(&out_mp4, check_frame_idx, width, height)?;
        let (psnr, mae, max_diff) = compute_psnr_mae(ref_frame_rgba, &decoded);

        println!(
            "[{label}]\n  ffprobe : {probe}\n  quality : PSNR={psnr:.2} dB, MAE={mae:.3}/255, MaxDiff={max_diff}\n  telemetry: acquired={} released={} in_flight={} gpu_frames={} cpu_fallback={}",
            telem.acquired_count,
            telem.released_count,
            telem.in_flight_count,
            telem.gpu_frames_encoded,
            telem.cpu_fallback_frames,
        );

        let min_psnr = if expect_gpu_zero_copy { 35.5 } else { 33.0 };
        if psnr < min_psnr {
            bail!("{label}: PSNR too low ({psnr:.2} dB < {min_psnr:.1} dB) — possible color/stride/sync corruption!");
        }
        if expect_gpu_zero_copy {
            if telem.gpu_frames_encoded != num_frames as u64 {
                bail!(
                    "{label}: expected {} GPU zero-copy frames, got {}",
                    num_frames,
                    telem.gpu_frames_encoded
                );
            }
            if telem.in_flight_count != 0 {
                bail!(
                    "{label}: NV12 surface pool leak! in_flight_count={}",
                    telem.in_flight_count
                );
            }
            if telem.cpu_fallback_frames != 0 {
                bail!(
                    "{label}: unexpected CPU fallback frames ({})",
                    telem.cpu_fallback_frames
                );
            }
        }
        Ok(())
    }

    pub fn run() -> Result<()> {
        let cover_png = make_test_cover_png(512);
        let lines = load_lines()?;
        let check_idx = 15usize;

        for (width, height) in [(1920u32, 1080u32), (3840u32, 2160u32)] {
            println!("\n==================== VALIDATING {}x{} ====================", width, height);
            // Reset backend env so previous iteration's raster fallback does not leak into reference frame.
            unsafe {
                std::env::set_var("GOOSY_RENDER_BACKEND", "d3d12");
                std::env::remove_var("GOOSY_QSV_CONVERTER");
                std::env::remove_var("GOOSY_QSV_DISABLE_ZEROCOPY");
                std::env::remove_var("GOOSY_QSV_BITRATE_KBPS");
            }
            // Generate Skia RGBA reference frame at check_idx
            let ref_rgba = {
                let asset = ImageAsset::from_bytes(&cover_png)?;
                let bg = BackgroundRenderer::from_image_asset(&asset)?;
                let cover = CoverRenderer::from_image_asset(&asset, Some("Zoo (Test)".to_owned()))?;
                let mut r = Renderer::with_scene_options_for_encoder(
                    width,
                    height,
                    60,
                    lines.clone(),
                    bg,
                    Some(cover),
                    true,
                    true,
                    LyricsStyle::default(),
                    &[],
                    EncoderBackend::Pipe,
                )?;
                let mut f = RenderedFrame::new(width, height);
                for i in 0..=check_idx {
                    let t_ms = 15_000 + (i as u64) * 1000 / 60;
                    r.render_frame_into(t_ms, &mut f)?;
                }
                f.pixels().to_vec()
            };

            validate_case(
                &format!("{}x{} H.264 Zero-Copy (ComputeDirect NV12 UAV)", width, height),
                width,
                height,
                60,
                VideoCodec::H264,
                &[
                    ("GOOSY_QSV_DISABLE_ZEROCOPY", None),
                    ("GOOSY_QSV_CONVERTER", Some("compute-direct")),
                    ("GOOSY_RENDER_BACKEND", Some("d3d12")),
                ],
                true,
                &cover_png,
                &lines,
                &ref_rgba,
                check_idx,
            )?;

            validate_case(
                &format!("{}x{} HEVC Zero-Copy (ComputeDirect NV12 UAV)", width, height),
                width,
                height,
                60,
                VideoCodec::Hevc,
                &[
                    ("GOOSY_QSV_DISABLE_ZEROCOPY", None),
                    ("GOOSY_QSV_CONVERTER", Some("compute-direct")),
                    ("GOOSY_RENDER_BACKEND", Some("d3d12")),
                ],
                true,
                &cover_png,
                &lines,
                &ref_rgba,
                check_idx,
            )?;

            validate_case(
                &format!("{}x{} H.264 Zero-Copy (D3D11 VideoProcessor)", width, height),
                width,
                height,
                60,
                VideoCodec::H264,
                &[
                    ("GOOSY_QSV_DISABLE_ZEROCOPY", None),
                    ("GOOSY_QSV_CONVERTER", Some("vp")),
                    ("GOOSY_RENDER_BACKEND", Some("d3d12")),
                ],
                true,
                &cover_png,
                &lines,
                &ref_rgba,
                check_idx,
            )?;

            validate_case(
                &format!("{}x{} H.264 Less-Copy (ComputeIntermediate + RTV)", width, height),
                width,
                height,
                60,
                VideoCodec::H264,
                &[
                    ("GOOSY_QSV_DISABLE_ZEROCOPY", None),
                    ("GOOSY_QSV_CONVERTER", Some("compute-intermediate")),
                    ("GOOSY_RENDER_BACKEND", Some("d3d12")),
                ],
                true,
                &cover_png,
                &lines,
                &ref_rgba,
                check_idx,
            )?;

            validate_case(
                &format!("{}x{} H.264 Zero-Copy (ComputeDirect, 25Mbps fidelity check)", width, height),
                width,
                height,
                60,
                VideoCodec::H264,
                &[
                    ("GOOSY_QSV_DISABLE_ZEROCOPY", None),
                    ("GOOSY_QSV_CONVERTER", Some("compute-direct")),
                    ("GOOSY_QSV_BITRATE_KBPS", Some("25000")),
                    ("GOOSY_RENDER_BACKEND", Some("d3d12")),
                ],
                true,
                &cover_png,
                &lines,
                &ref_rgba,
                check_idx,
            )?;
            unsafe { std::env::remove_var("GOOSY_QSV_BITRATE_KBPS") };

            validate_case(
                &format!("{}x{} H.264 Baseline (CPU swscale NV12 -> FFmpeg h264_qsv)", width, height),
                width,
                height,
                60,
                VideoCodec::H264,
                &[
                    ("GOOSY_QSV_DISABLE_ZEROCOPY", Some("1")),
                    ("GOOSY_RENDER_BACKEND", Some("d3d12")),
                ],
                false,
                &cover_png,
                &lines,
                &ref_rgba,
                check_idx,
            )?;

            if width == 1920 {
                validate_case(
                    "1920x1080 H.264 Fallback (Raster CPU -> QSV upload)",
                    width,
                    height,
                    60,
                    VideoCodec::H264,
                    &[
                        ("GOOSY_QSV_DISABLE_ZEROCOPY", None),
                        ("GOOSY_QSV_CONVERTER", Some("compute-direct")),
                        ("GOOSY_RENDER_BACKEND", Some("raster")),
                    ],
                    false,
                    &cover_png,
                    &lines,
                    &ref_rgba,
                    check_idx,
                )?;
            }
        }

        println!("\nALL WINDOWS QSV VISUAL CORRECTNESS & INTEGRITY CHECKS PASSED!");
        Ok(())
    }
}

#[cfg(target_os = "windows")]
fn main() -> anyhow::Result<()> {
    win_validate::run()
}
