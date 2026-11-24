use libgoosy::{
    LyricsStyle,
    geometry::FrameGeometry,
    layout::Layout,
    lrc::{BackgroundVocal, LyricLine, LyricWord},
    lyrics_renderer::LyricsRenderer,
    ttml,
};
use skia_safe::{AlphaType, Color, ColorType, IPoint, ImageInfo, surfaces};

fn read_surface_rgba(surface: &mut skia_safe::Surface, width: u32, height: u32) -> Vec<u8> {
    let info = ImageInfo::new(
        (width as i32, height as i32),
        ColorType::RGBA8888,
        AlphaType::Premul,
        None,
    );
    let row_bytes = width as usize * 4;
    let mut pixels = vec![0u8; height as usize * row_bytes];
    assert!(surface.read_pixels(&info, &mut pixels, row_bytes, IPoint::new(0, 0)));
    pixels
}

fn region_alpha_sum(
    rgba: &[u8],
    width: usize,
    y_start: usize,
    y_end: usize,
    x_start: usize,
    x_end: usize,
) -> u64 {
    let mut sum = 0u64;
    for y in y_start..y_end {
        for x in x_start..x_end {
            sum += rgba[(y * width + x) * 4 + 3] as u64;
        }
    }
    sum
}

#[test]
fn bilingual_history_stays_visible_and_fades_smoothly_on_scroll() {
    let ttml_text = std::fs::read_to_string("testdata/zoo.ttml").unwrap();
    let lines = ttml::parse_ttml(&ttml_text).unwrap();

    let (width, height, fps) = (1920u32, 1080u32, 60u32);
    let geom = FrameGeometry::for_frame(width, height);
    let lr = LyricsRenderer::new_with_options(
        &lines,
        geom.lyrics,
        true,
        true,
        LyricsStyle::default(),
    )
    .unwrap();
    let mut layout = Layout::new(
        &lines,
        geom.lyrics.height,
        lr.group_heights(),
        lr.group_gap(),
        lr.interlude_slot_height(),
    );
    let mut surface = surfaces::raster_n32_premul((width as i32, height as i32)).unwrap();

    // Advance from 0ms to 25_000ms at 60fps and inspect L4 across L4->L5 (19143ms)
    // and L5->L6 (22838ms).
    let mut l4_visible_right_after_deactivation = false;
    let mut l4_faded_monotonically_on_exit = true;
    let mut prev_exit_alpha: Option<u64> = None;
    let mut saw_partial_fade_step = false;
    let mut l4_reached_zero_on_exit = false;

    for f in 0..=(25_000 * fps as u64 / 1000) {
        let t_ms = f * 1000 / fps as u64;
        layout.update(&lines, t_ms, fps);

        // Right after L5 starts (19143ms): L4 is inactive history above L5
        if t_ms == 19_200 {
            assert!(!layout.is_active(4));
            assert_eq!(layout.focus_idx(), 5);
            surface.canvas().clear(Color::TRANSPARENT);
            lr.draw(surface.canvas(), &lines, &layout, t_ms, height)
                .unwrap();
            let px = read_surface_rgba(&mut surface, width, height);
            let focus_top = (layout.pos_y[5].current_position().max(0.0) as usize).min(height as usize);
            let hist_alpha = region_alpha_sum(
                &px,
                width as usize,
                0,
                focus_top,
                geom.lyrics.x as usize,
                width as usize,
            );
            println!("L4 after deactivation at t={t_ms}ms: top={:.1}, bot={:.1}, hist_alpha={hist_alpha}",
                layout.pos_y[4].current_position(),
                layout.pos_y[4].current_position() + lr.group_heights()[4]
            );
            l4_visible_right_after_deactivation = hist_alpha > 500_000;
        }

        // During L5->L6 scroll (22_600..23_200ms): L4 exits through the top fade band
        // Inspect isolated L4 by checking only the band above L5's top_y
        if (22_650..=23_200).contains(&t_ms) {
            let l4_top = layout.pos_y[4].current_position() as f32;
            let l4_bot = l4_top + lr.group_heights()[4] as f32;
            let l5_top = layout.pos_y[5].current_position() as f32;
            if l4_bot < l5_top - 20.0 {
                surface.canvas().clear(Color::TRANSPARENT);
                lr.draw(surface.canvas(), &lines, &layout, t_ms, height)
                    .unwrap();
                let px = read_surface_rgba(&mut surface, width, height);
                let y_end = ((l4_bot + 24.0).min(l5_top - 8.0).max(0.0) as usize).min(height as usize);
                let l4_alpha = region_alpha_sum(
                    &px,
                    width as usize,
                    0,
                    y_end,
                    geom.lyrics.x as usize,
                    width as usize,
                );
                if let Some(prev) = prev_exit_alpha {
                    if l4_alpha > prev + 5_000 {
                        l4_faded_monotonically_on_exit = false;
                    }
                    if prev > 100_000 && l4_alpha > 0 && l4_alpha < prev {
                        saw_partial_fade_step = true;
                    }
                }
                if l4_alpha == 0 && l4_bot <= 0.05 * height as f32 {
                    l4_reached_zero_on_exit = true;
                }
                println!(
                    "L4 exit t={t_ms}ms: top={l4_top:6.1}, bot={l4_bot:6.1}, y_end={y_end}, l4_alpha={l4_alpha}"
                );
                prev_exit_alpha = Some(l4_alpha);
            }
        }
    }

    assert!(
        l4_visible_right_after_deactivation,
        "historical bilingual line L4 must remain visibly blurred above L5 instead of snap-vanishing"
    );
    assert!(
        l4_faded_monotonically_on_exit,
        "historical bilingual line L4 must fade monotonically as it scrolls off the top"
    );
    assert!(
        saw_partial_fade_step,
        "historical bilingual line L4 must pass through intermediate fade levels instead of popping off"
    );
    assert!(
        l4_reached_zero_on_exit,
        "historical bilingual line L4 must finish fading to zero once its bottom passes 5% height"
    );
}

#[test]
fn full_zoo_audit_has_zero_snap_vanishes_on_landscape_and_portrait() {
    let ttml_text = std::fs::read_to_string("testdata/zoo.ttml").unwrap();
    let lines = ttml::parse_ttml(&ttml_text).unwrap();
    let last_end = lines.iter().map(|l| l.end_ms).max().unwrap_or(0) + 1_000;

    let edge_opacity_for = |top_y: f32, group_h: f32, height: f32, keep_opaque: bool| -> f32 {
        if keep_opaque {
            return 1.0;
        }
        let bottom_y = top_y + group_h.max(0.0);
        let fade_top = ((bottom_y - 0.05 * height) / (0.10 * height)).clamp(0.0, 1.0);
        let fade_bottom = ((height - 0.05 * height - top_y) / (0.10 * height)).clamp(0.0, 1.0);
        fade_top.min(fade_bottom)
    };

    for (label, width, height) in [("1080p", 1920u32, 1080u32), ("portrait", 1080u32, 1920u32)] {
        let fps = 60u32;
        let geom = FrameGeometry::for_frame(width, height);
        let lr = LyricsRenderer::new_with_options(
            &lines,
            geom.lyrics,
            true,
            true,
            LyricsStyle::default(),
        )
        .unwrap();
        let mut layout = Layout::new(
            &lines,
            geom.lyrics.height,
            lr.group_heights(),
            lr.group_gap(),
            lr.interlude_slot_height(),
        );

        let mut prev_op = vec![1.0f32; lines.len()];
        let mut prev_top = vec![0.0f32; lines.len()];
        let mut prev_act = vec![false; lines.len()];
        let mut max_frame_drop = 0.0f32;
        let mut max_deactivation_jump = 0.0f32;
        let mut snap_vanish_count = 0usize;

        for f in 0..=(last_end * fps as u64 / 1000) {
            let t_ms = f * 1000 / fps as u64;
            layout.update(&lines, t_ms, fps);
            let focus_idx = layout.focus_idx();
            for i in 0..lines.len() {
                let top_y = layout.pos_y[i].current_position() as f32;
                let h = lr.group_heights()[i] as f32;
                let act = layout.is_active(i);
                let keep_opaque = act && i >= focus_idx;
                let op = edge_opacity_for(top_y, h, height as f32, keep_opaque);
                if f > 0 {
                    let drop = prev_op[i] - op;
                    if drop > max_frame_drop {
                        max_frame_drop = drop;
                    }
                    if prev_op[i] >= 0.99 && op == 0.0 {
                        snap_vanish_count += 1;
                    }
                    if prev_act[i] && !act {
                        // Deactivation itself must not introduce an opacity jump beyond
                        // normal 1-frame scroll motion across the 0.10*height fade band.
                        let dy = (prev_top[i] - top_y).abs();
                        let max_motion_drop = dy / (0.10 * height as f32) + 1e-4;
                        let excess_jump = (drop - max_motion_drop).max(0.0);
                        if excess_jump > max_deactivation_jump {
                            max_deactivation_jump = excess_jump;
                        }
                    }
                }
                prev_op[i] = op;
                prev_top[i] = top_y;
                prev_act[i] = act;
            }
        }
        println!(
            "{label}: snap_vanish_count={snap_vanish_count}, max_single_frame_opacity_drop={max_frame_drop:.3}, max_deactivation_jump={max_deactivation_jump:.4}"
        );
        assert_eq!(snap_vanish_count, 0, "{label} had instant snap-vanish");
        assert!(
            max_deactivation_jump <= 1e-3,
            "{label} had discontinuous opacity jump on deactivation: {max_deactivation_jump}"
        );
    }
}

#[test]
fn tall_bilingual_group_with_background_vocal_stays_visible_after_deactivation() {
    let lines = vec![
        LyricLine {
            start_ms: 0,
            end_ms: 3_000,
            text: "A much longer bilingual lyric line that wraps onto multiple lines".to_owned(),
            translation: Some("这是一句非常长的双语翻译歌词，会自动换行显示成多行".to_owned()),
            agent_id: None,
            is_duet: false,
            is_background: false,
            background_vocal: Some(BackgroundVocal {
                start_ms: 1_500,
                end_ms: 2_800,
                text: "(Ooh, fading away into the night)".to_owned(),
                translation: Some("（呜，逐渐消逝在夜色中）".to_owned()),
                words: vec![LyricWord {
                    start_ms: 1_500,
                    end_ms: 2_800,
                    text: "(Ooh, fading away into the night)".to_owned(),
                }],
            }),
            words: Vec::new(),
        },
        LyricLine {
            start_ms: 3_000,
            end_ms: 6_000,
            text: "Next bilingual lyric line".to_owned(),
            translation: Some("下一句双语歌词".to_owned()),
            agent_id: None,
            is_duet: false,
            is_background: false,
            background_vocal: None,
            words: Vec::new(),
        },
    ];
    let (width, height, fps) = (1920u32, 1080u32, 60u32);
    let geom = FrameGeometry::for_frame(width, height);
    let lr = LyricsRenderer::new_with_options(
        &lines,
        geom.lyrics,
        true,
        true,
        LyricsStyle::default(),
    )
    .unwrap();
    let mut layout = Layout::new(
        &lines,
        geom.lyrics.height,
        lr.group_heights(),
        lr.group_gap(),
        lr.interlude_slot_height(),
    );
    let mut surface = surfaces::raster_n32_premul((width as i32, height as i32)).unwrap();

    for f in 0..=(3_500 * fps as u64 / 1000) {
        let t_ms = f * 1000 / fps as u64;
        layout.update(&lines, t_ms, fps);
    }
    assert!(!layout.is_active(0));
    surface.canvas().clear(Color::TRANSPARENT);
    lr.draw(surface.canvas(), &lines, &layout, 3_500, height)
        .unwrap();
    let px = read_surface_rgba(&mut surface, width, height);
    let focus_top = (layout.pos_y[1].current_position().max(0.0) as usize).min(height as usize);
    let hist_alpha = region_alpha_sum(
        &px,
        width as usize,
        0,
        focus_top,
        geom.lyrics.x as usize,
        width as usize,
    );
    println!(
        "Tall bg-vocal line at t=3500ms: top={:.1}, bot={:.1}, hist_alpha={hist_alpha}",
        layout.pos_y[0].current_position(),
        layout.pos_y[0].current_position() + lr.group_heights()[0]
    );
    assert!(
        hist_alpha > 500_000,
        "tall bilingual + background vocal history must remain visible above active line"
    );
}

#[test]
fn historical_line_blurs_smoothly_while_scrolling_up_before_next_line_starts() {
    let ttml_text = std::fs::read_to_string("testdata/zoo.ttml").unwrap();
    let lines = ttml::parse_ttml(&ttml_text).unwrap();

    let (width, height, fps) = (1920u32, 1080u32, 60u32);
    let geom = FrameGeometry::for_frame(width, height);
    let lr = LyricsRenderer::new_with_options(
        &lines,
        geom.lyrics,
        true,
        true,
        LyricsStyle::default(),
    )
    .unwrap();
    let mut layout = Layout::new(
        &lines,
        geom.lyrics.height,
        lr.group_heights(),
        lr.group_gap(),
        lr.interlude_slot_height(),
    );

    // L4: [15330..18851], L5: [19143..22609].
    // As soon as L4 ends and scrolls upward at 18851ms, its blur must rise smoothly
    // BEFORE L5 starts at 19143ms, with no single-frame jump at 19143ms.
    let mut prev_blur = 0.0f64;
    let mut max_blur_step = 0.0f64;
    let mut blur_before_l5_start = 0.0f64;
    let mut blur_jump_at_l5_start = 0.0f64;
    let mut saw_intermediate_blur = false;

    for f in 0..=(20_000 * fps as u64 / 1000) {
        let t_ms = f * 1000 / fps as u64;
        layout.update(&lines, t_ms, fps);
        let b = layout.blur[4].current_position();
        if (18_850..=19_600).contains(&t_ms) {
            let step = (b - prev_blur).abs();
            if step > max_blur_step {
                max_blur_step = step;
            }
            if b > 0.2 && b < 1.8 {
                saw_intermediate_blur = true;
            }
            if t_ms <= 19_133 {
                blur_before_l5_start = b;
            }
            if (19_143..=19_160).contains(&t_ms) {
                blur_jump_at_l5_start = step;
            }
            if t_ms % 50 < 17 {
                println!(
                    "L4 blur t={t_ms:5}ms focus={} top={:6.1} blur_l4={b:.3} blur_l5={:.3}",
                    layout.focus_idx(),
                    layout.pos_y[4].current_position(),
                    layout.blur[5].current_position()
                );
            }
        }
        prev_blur = b;
    }

    println!(
        "L4 summary: blur_before_l5_start={blur_before_l5_start:.3}, blur_jump_at_l5_start={blur_jump_at_l5_start:.3}, max_blur_step={max_blur_step:.3}"
    );
    assert!(
        blur_before_l5_start > 1.0,
        "L4 must already be substantially blurred while scrolling up before L5 starts: got {blur_before_l5_start}"
    );
    assert!(
        saw_intermediate_blur,
        "L4 must pass through intermediate fractional blur levels"
    );
    assert!(
        blur_jump_at_l5_start < 0.15,
        "L4 must not jump in blur when L5 starts: got step {blur_jump_at_l5_start}"
    );
    assert!(
        max_blur_step < 0.25,
        "L4 single-frame blur step must stay smooth: got {max_blur_step}"
    );
}
