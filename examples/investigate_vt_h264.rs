#[cfg(target_os = "macos")]
mod macos_probe {
    use std::ffi::{CStr, CString, c_void};
    use std::ptr::{null, null_mut};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Instant;

    #[repr(C)]
    #[derive(Copy, Clone)]
    struct CMTime {
        value: i64,
        timescale: i32,
        flags: u32,
        epoch: i64,
    }

    type VTCompressionOutputCallback = unsafe extern "C" fn(
        output_callback_ref_con: *mut c_void,
        source_frame_ref_con: *mut c_void,
        status: i32,
        info_flags: u32,
        sample_buffer: *mut c_void,
    );

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        static kCFAllocatorDefault: *const c_void;
        static kCFBooleanTrue: *const c_void;
        static kCFBooleanFalse: *const c_void;
        static kCFTypeDictionaryKeyCallBacks: c_void;
        static kCFTypeDictionaryValueCallBacks: c_void;

        fn CFDictionaryCreateMutable(
            allocator: *const c_void,
            capacity: isize,
            key_callbacks: *const c_void,
            value_callbacks: *const c_void,
        ) -> *mut c_void;
        fn CFDictionarySetValue(dict: *mut c_void, key: *const c_void, value: *const c_void);
        fn CFNumberCreate(
            allocator: *const c_void,
            the_type: isize,
            value_ptr: *const c_void,
        ) -> *mut c_void;
        fn CFStringCreateWithCString(
            alloc: *const c_void,
            c_str: *const i8,
            encoding: u32,
        ) -> *mut c_void;
        fn CFStringGetCString(
            the_string: *const c_void,
            buffer: *mut i8,
            buffer_size: isize,
            encoding: u32,
        ) -> bool;
        fn CFCopyDescription(cf: *const c_void) -> *mut c_void;
        fn CFRelease(cf: *const c_void);
    }

    #[link(name = "CoreMedia", kind = "framework")]
    unsafe extern "C" {
        fn CMSampleBufferGetFormatDescription(sbuf: *const c_void) -> *const c_void;
        fn CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
            video_desc: *const c_void,
            parameter_set_index: usize,
            parameter_set_pointer_out: *mut *const u8,
            parameter_set_size_out: *mut usize,
            parameter_set_count_out: *mut usize,
            nal_unit_header_length_out: *mut i32,
        ) -> i32;
    }

    #[link(name = "CoreVideo", kind = "framework")]
    unsafe extern "C" {
        static kCVPixelBufferPixelFormatTypeKey: *const c_void;
        static kCVPixelBufferWidthKey: *const c_void;
        static kCVPixelBufferHeightKey: *const c_void;
        static kCVPixelBufferMetalCompatibilityKey: *const c_void;
        static kCVPixelBufferIOSurfacePropertiesKey: *const c_void;

        fn CVPixelBufferPoolCreate(
            allocator: *const c_void,
            pool_attributes: *const c_void,
            pixel_buffer_attributes: *const c_void,
            pool_out: *mut *mut c_void,
        ) -> i32;
        fn CVPixelBufferPoolCreatePixelBuffer(
            allocator: *const c_void,
            pixel_buffer_pool: *mut c_void,
            pixel_buffer_out: *mut *mut c_void,
        ) -> i32;
        fn CVPixelBufferRelease(texture: *mut c_void);
        fn CVPixelBufferLockBaseAddress(buffer: *mut c_void, flags: u64) -> i32;
        fn CVPixelBufferUnlockBaseAddress(buffer: *mut c_void, flags: u64) -> i32;
        fn CVPixelBufferGetBaseAddressOfPlane(buffer: *mut c_void, plane: usize) -> *mut u8;
        fn CVPixelBufferGetBytesPerRowOfPlane(buffer: *mut c_void, plane: usize) -> usize;
        fn CVPixelBufferGetBaseAddress(buffer: *mut c_void) -> *mut u8;
        fn CVPixelBufferGetBytesPerRow(buffer: *mut c_void) -> usize;
    }

    #[link(name = "VideoToolbox", kind = "framework")]
    unsafe extern "C" {
        static kVTVideoEncoderSpecification_EnableHardwareAcceleratedVideoEncoder: *const c_void;
        static kVTVideoEncoderSpecification_RequireHardwareAcceleratedVideoEncoder: *const c_void;
        static kVTCompressionPropertyKey_RealTime: *const c_void;
        static kVTCompressionPropertyKey_PrioritizeEncodingSpeedOverQuality: *const c_void;
        static kVTCompressionPropertyKey_AllowFrameReordering: *const c_void;
        static kVTCompressionPropertyKey_AllowTemporalCompression: *const c_void;
        static kVTCompressionPropertyKey_ExpectedFrameRate: *const c_void;
        static kVTCompressionPropertyKey_AverageBitRate: *const c_void;
        static kVTCompressionPropertyKey_ConstantBitRate: *const c_void;
        static kVTCompressionPropertyKey_Quality: *const c_void;
        static kVTCompressionPropertyKey_MaxKeyFrameInterval: *const c_void;
        static kVTCompressionPropertyKey_MaxFrameDelayCount: *const c_void;
        static kVTCompressionPropertyKey_ProfileLevel: *const c_void;
        static kVTCompressionPropertyKey_H264EntropyMode: *const c_void;
        static kVTCompressionPropertyKey_UsingHardwareAcceleratedVideoEncoder: *const c_void;

        static kVTProfileLevel_H264_Baseline_AutoLevel: *const c_void;
        static kVTProfileLevel_H264_Main_AutoLevel: *const c_void;
        static kVTProfileLevel_H264_High_AutoLevel: *const c_void;
        static kVTProfileLevel_H264_ConstrainedHigh_AutoLevel: *const c_void;
        static kVTProfileLevel_H264_High_5_2: *const c_void;

        static kVTH264EntropyMode_CABAC: *const c_void;
        static kVTH264EntropyMode_CAVLC: *const c_void;

        fn VTCompressionSessionCreate(
            allocator: *const c_void,
            width: i32,
            height: i32,
            codec_type: u32,
            encoder_specification: *const c_void,
            source_image_buffer_attributes: *const c_void,
            compressed_data_allocator: *const c_void,
            output_callback: VTCompressionOutputCallback,
            output_callback_ref_con: *mut c_void,
            compression_session_out: *mut *mut c_void,
        ) -> i32;

        fn VTSessionSetProperty(
            session: *mut c_void,
            property_key: *const c_void,
            property_value: *const c_void,
        ) -> i32;

        fn VTSessionCopyProperty(
            session: *mut c_void,
            property_key: *const c_void,
            allocator: *const c_void,
            property_value_out: *mut *mut c_void,
        ) -> i32;

        fn VTSessionCopySupportedPropertyDictionary(
            session: *mut c_void,
            supported_property_dictionary_out: *mut *mut c_void,
        ) -> i32;

        fn VTCompressionSessionPrepareToEncodeFrames(session: *mut c_void) -> i32;
        fn VTCompressionSessionGetPixelBufferPool(session: *mut c_void) -> *mut c_void;

        fn VTCompressionSessionEncodeFrame(
            session: *mut c_void,
            image_buffer: *mut c_void,
            presentation_time_stamp: CMTime,
            duration: CMTime,
            frame_properties: *const c_void,
            source_frame_ref_con: *mut c_void,
            info_flags_out: *mut u32,
        ) -> i32;

        fn VTCompressionSessionCompleteFrames(
            session: *mut c_void,
            complete_until_presentation_time_stamp: CMTime,
        ) -> i32;

        fn VTCompressionSessionInvalidate(session: *mut c_void);
    }

    const K_CF_NUMBER_SINT32_TYPE: isize = 3;
    const K_CF_NUMBER_FLOAT64_TYPE: isize = 6;
    const K_CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;
    const FOURCC_AVC1: u32 = u32::from_be_bytes(*b"avc1");
    const FOURCC_HVC1: u32 = u32::from_be_bytes(*b"hvc1");
    const FOURCC_420V: u32 = u32::from_be_bytes(*b"420v");
    const FOURCC_420F: u32 = u32::from_be_bytes(*b"420f");
    const FOURCC_BGRA: u32 = u32::from_be_bytes(*b"BGRA");

    struct CallbackState {
        encoded_count: AtomicUsize,
        sps_level_idc: AtomicUsize,
        sps_profile_idc: AtomicUsize,
    }

    unsafe extern "C" fn on_output(
        ref_con: *mut c_void,
        _frame_ref: *mut c_void,
        status: i32,
        _info_flags: u32,
        sample_buffer: *mut c_void,
    ) {
        if status == 0 && !sample_buffer.is_null() {
            let state = unsafe { &*(ref_con as *const CallbackState) };
            state.encoded_count.fetch_add(1, Ordering::Relaxed);
            if state.sps_level_idc.load(Ordering::Relaxed) == 0 {
                unsafe {
                    let desc = CMSampleBufferGetFormatDescription(sample_buffer);
                    if !desc.is_null() {
                        let mut ptr: *const u8 = null();
                        let mut size: usize = 0;
                        let mut count: usize = 0;
                        let mut nal_len: i32 = 0;
                        if CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
                            desc,
                            0,
                            &mut ptr,
                            &mut size,
                            &mut count,
                            &mut nal_len,
                        ) == 0
                            && size >= 4
                            && !ptr.is_null()
                        {
                            let sps = std::slice::from_raw_parts(ptr, size);
                            state
                                .sps_profile_idc
                                .store(sps[1] as usize, Ordering::Relaxed);
                            state
                                .sps_level_idc
                                .store(sps[3] as usize, Ordering::Relaxed);
                        }
                    }
                }
            }
        }
    }

    unsafe fn cf_string(s: &str) -> *mut c_void {
        let c = CString::new(s).unwrap();
        unsafe {
            CFStringCreateWithCString(kCFAllocatorDefault, c.as_ptr(), K_CF_STRING_ENCODING_UTF8)
        }
    }

    unsafe fn cf_description(obj: *const c_void) -> String {
        if obj.is_null() {
            return "<null>".to_owned();
        }
        unsafe {
            let desc = CFCopyDescription(obj);
            if desc.is_null() {
                return "<no desc>".to_owned();
            }
            let mut buf = vec![0i8; 65536];
            let ok = CFStringGetCString(
                desc,
                buf.as_mut_ptr(),
                buf.len() as isize,
                K_CF_STRING_ENCODING_UTF8,
            );
            CFRelease(desc);
            if ok {
                CStr::from_ptr(buf.as_ptr()).to_string_lossy().into_owned()
            } else {
                "<truncated>".to_owned()
            }
        }
    }

    unsafe fn cf_int(val: i32) -> *mut c_void {
        unsafe {
            CFNumberCreate(
                kCFAllocatorDefault,
                K_CF_NUMBER_SINT32_TYPE,
                &val as *const i32 as *const c_void,
            )
        }
    }

    unsafe fn cf_f64(val: f64) -> *mut c_void {
        unsafe {
            CFNumberCreate(
                kCFAllocatorDefault,
                K_CF_NUMBER_FLOAT64_TYPE,
                &val as *const f64 as *const c_void,
            )
        }
    }

    unsafe fn create_pool(width: i32, height: i32, pix_fmt: u32) -> *mut c_void {
        unsafe {
            let io_surface = CFDictionaryCreateMutable(
                kCFAllocatorDefault,
                0,
                &kCFTypeDictionaryKeyCallBacks,
                &kCFTypeDictionaryValueCallBacks,
            );
            let attrs = CFDictionaryCreateMutable(
                kCFAllocatorDefault,
                4,
                &kCFTypeDictionaryKeyCallBacks,
                &kCFTypeDictionaryValueCallBacks,
            );
            let fmt_num = cf_int(pix_fmt as i32);
            let w_num = cf_int(width);
            let h_num = cf_int(height);
            CFDictionarySetValue(attrs, kCVPixelBufferPixelFormatTypeKey, fmt_num);
            CFDictionarySetValue(attrs, kCVPixelBufferWidthKey, w_num);
            CFDictionarySetValue(attrs, kCVPixelBufferHeightKey, h_num);
            CFDictionarySetValue(attrs, kCVPixelBufferMetalCompatibilityKey, kCFBooleanTrue);
            CFDictionarySetValue(attrs, kCVPixelBufferIOSurfacePropertiesKey, io_surface);

            let mut pool: *mut c_void = null_mut();
            let status = CVPixelBufferPoolCreate(kCFAllocatorDefault, null(), attrs, &mut pool);
            assert_eq!(status, 0, "CVPixelBufferPoolCreate failed");
            CFRelease(fmt_num);
            CFRelease(w_num);
            CFRelease(h_num);
            CFRelease(attrs);
            CFRelease(io_surface);
            pool
        }
    }

    unsafe fn allocate_and_populate_pool_buffers(
        pool: *mut c_void,
        width: i32,
        height: i32,
        pix_fmt: u32,
        count: usize,
    ) -> Vec<*mut c_void> {
        let mut buffers = Vec::with_capacity(count);
        unsafe {
            for i in 0..count {
                let mut pb: *mut c_void = null_mut();
                let st = CVPixelBufferPoolCreatePixelBuffer(kCFAllocatorDefault, pool, &mut pb);
                assert_eq!(st, 0);
                CVPixelBufferLockBaseAddress(pb, 0);
                if pix_fmt == FOURCC_BGRA {
                    let base = CVPixelBufferGetBaseAddress(pb);
                    let bpr = CVPixelBufferGetBytesPerRow(pb);
                    for r in 0..height as usize {
                        let row =
                            std::slice::from_raw_parts_mut(base.add(r * bpr), width as usize * 4);
                        for (x, px) in row.chunks_exact_mut(4).enumerate() {
                            px[0] = ((x + r + i * 7) & 0xFF) as u8;
                            px[1] = ((x * 2 + r + i * 3) & 0xFF) as u8;
                            px[2] = ((x + r * 2 + i * 5) & 0xFF) as u8;
                            px[3] = 255;
                        }
                    }
                } else {
                    let y_base = CVPixelBufferGetBaseAddressOfPlane(pb, 0);
                    let y_bpr = CVPixelBufferGetBytesPerRowOfPlane(pb, 0);
                    for r in 0..height as usize {
                        let row =
                            std::slice::from_raw_parts_mut(y_base.add(r * y_bpr), width as usize);
                        for (x, p) in row.iter_mut().enumerate() {
                            *p = (16 + ((x + r + i * 11) % 219)) as u8;
                        }
                    }
                    let uv_base = CVPixelBufferGetBaseAddressOfPlane(pb, 1);
                    let uv_bpr = CVPixelBufferGetBytesPerRowOfPlane(pb, 1);
                    for r in 0..(height as usize / 2) {
                        let row =
                            std::slice::from_raw_parts_mut(uv_base.add(r * uv_bpr), width as usize);
                        for (x, p) in row.iter_mut().enumerate() {
                            *p = (64 + ((x + r + i * 5) % 128)) as u8;
                        }
                    }
                }
                CVPixelBufferUnlockBaseAddress(pb, 0);
                buffers.push(pb);
            }
        }
        buffers
    }

    pub fn inspect_h264_supported_properties() {
        unsafe {
            let spec = CFDictionaryCreateMutable(
                kCFAllocatorDefault,
                2,
                &kCFTypeDictionaryKeyCallBacks,
                &kCFTypeDictionaryValueCallBacks,
            );
            CFDictionarySetValue(
                spec,
                kVTVideoEncoderSpecification_RequireHardwareAcceleratedVideoEncoder,
                kCFBooleanTrue,
            );
            let state = Box::new(CallbackState {
                encoded_count: AtomicUsize::new(0),
                sps_level_idc: AtomicUsize::new(0),
                sps_profile_idc: AtomicUsize::new(0),
            });
            let mut session: *mut c_void = null_mut();
            let status = VTCompressionSessionCreate(
                kCFAllocatorDefault,
                3840,
                2160,
                FOURCC_AVC1,
                spec,
                null(),
                kCFAllocatorDefault,
                on_output,
                &*state as *const CallbackState as *mut c_void,
                &mut session,
            );
            assert_eq!(status, 0);

            let mut supp_dict: *mut c_void = null_mut();
            if VTSessionCopySupportedPropertyDictionary(session, &mut supp_dict) == 0 {
                let desc = cf_description(supp_dict);
                println!("=== Supported Properties on M4 Hardware H.264 Session ===");
                for line in desc.lines() {
                    if line.contains(" = ") && !line.trim().starts_with("Description") {
                        println!("  {}", line.trim());
                    }
                }
                CFRelease(supp_dict);
            }

            // Test setting explicit Level 6.0 / 6.1 / 6.2 strings vs High_AutoLevel vs High_5_2
            for level_str in [
                "H264_High_AutoLevel",
                "H264_High_5_2",
                "H264_High_6_0",
                "H264_High_6_1",
                "H264_High_6_2",
                "H264_Main_AutoLevel",
                "H264_Baseline_AutoLevel",
            ] {
                let s = cf_string(level_str);
                let st = VTSessionSetProperty(session, kVTCompressionPropertyKey_ProfileLevel, s);
                println!("  VTSessionSetProperty(ProfileLevel = {level_str:<24}) -> status = {st}");
                CFRelease(s);
            }

            VTCompressionSessionInvalidate(session);
            CFRelease(session);
            CFRelease(spec);
        }
    }

    pub struct VtConfig<'a> {
        pub label: &'a str,
        pub codec: u32,
        pub width: i32,
        pub height: i32,
        pub fps: i32,
        pub frames: usize,
        pub pix_fmt: u32,
        pub use_session_pool: bool,
        pub realtime: Option<bool>,
        pub prio_speed: Option<bool>,
        pub allow_b_frames: Option<bool>,
        pub allow_temporal: Option<bool>,
        pub cabac: Option<bool>,
        pub profile_level: Option<*const c_void>,
        pub expected_fps: Option<f64>,
        pub avg_bitrate: Option<i32>,
        pub cbr_bitrate: Option<i32>,
        pub quality: Option<f64>,
        pub max_keyframe_interval: Option<i32>,
        pub max_frame_delay: Option<i32>,
    }

    pub fn run_vt_config(cfg: &VtConfig<'_>) -> (f64, usize, usize) {
        unsafe {
            let spec = CFDictionaryCreateMutable(
                kCFAllocatorDefault,
                2,
                &kCFTypeDictionaryKeyCallBacks,
                &kCFTypeDictionaryValueCallBacks,
            );
            CFDictionarySetValue(
                spec,
                kVTVideoEncoderSpecification_EnableHardwareAcceleratedVideoEncoder,
                kCFBooleanTrue,
            );
            CFDictionarySetValue(
                spec,
                kVTVideoEncoderSpecification_RequireHardwareAcceleratedVideoEncoder,
                kCFBooleanTrue,
            );

            let state = Arc::new(CallbackState {
                encoded_count: AtomicUsize::new(0),
                sps_level_idc: AtomicUsize::new(0),
                sps_profile_idc: AtomicUsize::new(0),
            });

            let mut session: *mut c_void = null_mut();
            let status = VTCompressionSessionCreate(
                kCFAllocatorDefault,
                cfg.width,
                cfg.height,
                cfg.codec,
                spec,
                null(),
                kCFAllocatorDefault,
                on_output,
                Arc::as_ptr(&state) as *mut c_void,
                &mut session,
            );
            assert_eq!(
                status, 0,
                "VTCompressionSessionCreate failed for {}",
                cfg.label
            );

            if let Some(profile) = cfg.profile_level {
                let st =
                    VTSessionSetProperty(session, kVTCompressionPropertyKey_ProfileLevel, profile);
                if st != 0 {
                    eprintln!("  [warn] {}: ProfileLevel returned {st}", cfg.label);
                }
            }
            if let Some(rt) = cfg.realtime {
                VTSessionSetProperty(
                    session,
                    kVTCompressionPropertyKey_RealTime,
                    if rt { kCFBooleanTrue } else { kCFBooleanFalse },
                );
            }
            if let Some(ps) = cfg.prio_speed {
                VTSessionSetProperty(
                    session,
                    kVTCompressionPropertyKey_PrioritizeEncodingSpeedOverQuality,
                    if ps { kCFBooleanTrue } else { kCFBooleanFalse },
                );
            }
            if let Some(bf) = cfg.allow_b_frames {
                VTSessionSetProperty(
                    session,
                    kVTCompressionPropertyKey_AllowFrameReordering,
                    if bf { kCFBooleanTrue } else { kCFBooleanFalse },
                );
            }
            if let Some(tc) = cfg.allow_temporal {
                VTSessionSetProperty(
                    session,
                    kVTCompressionPropertyKey_AllowTemporalCompression,
                    if tc { kCFBooleanTrue } else { kCFBooleanFalse },
                );
            }
            if let Some(cabac) = cfg.cabac {
                VTSessionSetProperty(
                    session,
                    kVTCompressionPropertyKey_H264EntropyMode,
                    if cabac {
                        kVTH264EntropyMode_CABAC
                    } else {
                        kVTH264EntropyMode_CAVLC
                    },
                );
            }
            if let Some(efps) = cfg.expected_fps {
                let n = cf_f64(efps);
                VTSessionSetProperty(session, kVTCompressionPropertyKey_ExpectedFrameRate, n);
                CFRelease(n);
            }
            if let Some(br) = cfg.avg_bitrate {
                let n = cf_int(br);
                VTSessionSetProperty(session, kVTCompressionPropertyKey_AverageBitRate, n);
                CFRelease(n);
            }
            if let Some(cbr) = cfg.cbr_bitrate {
                let n = cf_int(cbr);
                VTSessionSetProperty(session, kVTCompressionPropertyKey_ConstantBitRate, n);
                CFRelease(n);
            }
            if let Some(q) = cfg.quality {
                let n = cf_f64(q);
                VTSessionSetProperty(session, kVTCompressionPropertyKey_Quality, n);
                CFRelease(n);
            }
            if let Some(ki) = cfg.max_keyframe_interval {
                let n = cf_int(ki);
                VTSessionSetProperty(session, kVTCompressionPropertyKey_MaxKeyFrameInterval, n);
                CFRelease(n);
            }
            if let Some(mfd) = cfg.max_frame_delay {
                let n = cf_int(mfd);
                VTSessionSetProperty(session, kVTCompressionPropertyKey_MaxFrameDelayCount, n);
                CFRelease(n);
            }

            VTCompressionSessionPrepareToEncodeFrames(session);

            // Verify hardware encoder is active
            let mut using_hw: *mut c_void = null_mut();
            if VTSessionCopyProperty(
                session,
                kVTCompressionPropertyKey_UsingHardwareAcceleratedVideoEncoder,
                kCFAllocatorDefault,
                &mut using_hw,
            ) == 0
            {
                assert_eq!(
                    using_hw as *const c_void, kCFBooleanTrue,
                    "Not using HW encoder!"
                );
                CFRelease(using_hw);
            }

            let ext_pool = if cfg.use_session_pool {
                null_mut()
            } else {
                create_pool(cfg.width, cfg.height, cfg.pix_fmt)
            };
            let active_pool = if cfg.use_session_pool {
                let p = VTCompressionSessionGetPixelBufferPool(session);
                if p.is_null() {
                    create_pool(cfg.width, cfg.height, cfg.pix_fmt)
                } else {
                    p
                }
            } else {
                ext_pool
            };

            let buffers = allocate_and_populate_pool_buffers(
                active_pool,
                cfg.width,
                cfg.height,
                cfg.pix_fmt,
                8,
            );

            // Warmup 15 frames
            let dur = CMTime {
                value: 1,
                timescale: cfg.fps,
                flags: 1,
                epoch: 0,
            };
            for i in 0..15 {
                let pts = CMTime {
                    value: i as i64,
                    timescale: cfg.fps,
                    flags: 1,
                    epoch: 0,
                };
                let mut flags = 0u32;
                VTCompressionSessionEncodeFrame(
                    session,
                    buffers[i % buffers.len()],
                    pts,
                    dur,
                    null(),
                    null_mut(),
                    &mut flags,
                );
            }
            let invalid_pts = CMTime {
                value: 0,
                timescale: 0,
                flags: 0,
                epoch: 0,
            };
            VTCompressionSessionCompleteFrames(session, invalid_pts);

            // Timed run
            let start = Instant::now();
            for i in 0..cfg.frames {
                let pts = CMTime {
                    value: (15 + i) as i64,
                    timescale: cfg.fps,
                    flags: 1,
                    epoch: 0,
                };
                let mut flags = 0u32;
                let st = VTCompressionSessionEncodeFrame(
                    session,
                    buffers[i % buffers.len()],
                    pts,
                    dur,
                    null(),
                    null_mut(),
                    &mut flags,
                );
                assert_eq!(st, 0, "VTCompressionSessionEncodeFrame failed: {st}");
            }
            VTCompressionSessionCompleteFrames(session, invalid_pts);
            let elapsed = start.elapsed().as_secs_f64();
            let fps_achieved = cfg.frames as f64 / elapsed;

            for pb in buffers {
                CVPixelBufferRelease(pb);
            }
            if !ext_pool.is_null() {
                CFRelease(ext_pool);
            }
            VTCompressionSessionInvalidate(session);
            CFRelease(session);
            CFRelease(spec);

            let profile_idc = state.sps_profile_idc.load(Ordering::Relaxed);
            let level_idc = state.sps_level_idc.load(Ordering::Relaxed);
            (fps_achieved, profile_idc, level_idc)
        }
    }

    pub fn run_all() {
        inspect_h264_supported_properties();

        println!(
            "\n=== 1. Direct VTCompressionSession H.264 3840x2160 Configuration Sweep (180 frames) ==="
        );
        println!(
            "{:<52} | {:>9} | {:>11} | {:>12} | {:>9}",
            "Configuration", "FPS", "ms/frame", "MPix/s", "SPS (P/L)"
        );
        println!("{}", "-".repeat(103));

        let base = |label: &'static str| VtConfig {
            label,
            codec: FOURCC_AVC1,
            width: 3840,
            height: 2160,
            fps: 120,
            frames: 180,
            pix_fmt: FOURCC_420V,
            use_session_pool: false,
            realtime: Some(true),
            prio_speed: Some(true),
            allow_b_frames: None,
            allow_temporal: None,
            cabac: None,
            profile_level: unsafe { Some(kVTProfileLevel_H264_High_AutoLevel) },
            expected_fps: Some(120.0),
            avg_bitrate: Some(8_000_000),
            cbr_bitrate: None,
            quality: None,
            max_keyframe_interval: Some(120),
            max_frame_delay: None,
        };

        let configs = unsafe {
            vec![
                base("1. High_AutoLevel, RT=true, PrioSpeed=true (Current)"),
                VtConfig {
                    profile_level: Some(kVTProfileLevel_H264_High_5_2),
                    ..base("2. Explicit H264_High_5_2, RT=true, PrioSpeed=true")
                },
                VtConfig {
                    profile_level: None,
                    ..base("3. ProfileLevel=unset, RT=true, PrioSpeed=true")
                },
                VtConfig {
                    realtime: Some(false),
                    prio_speed: Some(true),
                    ..base("4. Offline mode (RT=false, PrioSpeed=true)")
                },
                VtConfig {
                    realtime: Some(false),
                    prio_speed: Some(false),
                    ..base("5. Offline quality (RT=false, PrioSpeed=false)")
                },
                VtConfig {
                    realtime: Some(true),
                    prio_speed: Some(false),
                    ..base("6. RealTime only (RT=true, PrioSpeed=false)")
                },
                VtConfig {
                    allow_b_frames: Some(false),
                    ..base("7. No B-frames (AllowFrameReordering=false)")
                },
                VtConfig {
                    allow_b_frames: Some(false),
                    cabac: Some(false),
                    ..base("8. CAVLC Entropy + No B-frames")
                },
                VtConfig {
                    cabac: Some(false),
                    ..base("9. CAVLC Entropy (H264EntropyMode=CAVLC)")
                },
                VtConfig {
                    profile_level: Some(kVTProfileLevel_H264_Baseline_AutoLevel),
                    allow_b_frames: Some(false),
                    ..base("10. Baseline_AutoLevel (CAVLC, no B-frames)")
                },
                VtConfig {
                    profile_level: Some(kVTProfileLevel_H264_ConstrainedHigh_AutoLevel),
                    allow_b_frames: Some(false),
                    ..base("11. ConstrainedHigh_AutoLevel")
                },
                VtConfig {
                    profile_level: Some(kVTProfileLevel_H264_Main_AutoLevel),
                    ..base("12. Main_AutoLevel")
                },
                VtConfig {
                    expected_fps: Some(240.0),
                    ..base("13. ExpectedFrameRate = 240.0")
                },
                VtConfig {
                    expected_fps: Some(480.0),
                    ..base("14. ExpectedFrameRate = 480.0")
                },
                VtConfig {
                    expected_fps: None,
                    ..base("15. ExpectedFrameRate = unset")
                },
                VtConfig {
                    avg_bitrate: None,
                    quality: Some(0.5),
                    ..base("16. Constant Quality VBR (Quality=0.5, no bitrate)")
                },
                VtConfig {
                    avg_bitrate: None,
                    cbr_bitrate: Some(20_000_000),
                    ..base("17. ConstantBitRate = 20 Mbps")
                },
                VtConfig {
                    allow_temporal: Some(false),
                    max_keyframe_interval: Some(1),
                    ..base("18. All-Intra (AllowTemporalCompression=false)")
                },
                VtConfig {
                    max_frame_delay: Some(0),
                    allow_b_frames: Some(false),
                    ..base("19. Low-latency (MaxFrameDelayCount=0, no B)")
                },
                VtConfig {
                    pix_fmt: FOURCC_420F,
                    ..base("20. PixelFormat = 420f (Full-range NV12)")
                },
                VtConfig {
                    pix_fmt: FOURCC_BGRA,
                    ..base("21. PixelFormat = BGRA (32-bit BGRA IOSurface)")
                },
                VtConfig {
                    use_session_pool: true,
                    ..base("22. VTCompressionSessionGetPixelBufferPool internal")
                },
                VtConfig {
                    codec: FOURCC_HVC1,
                    profile_level: None,
                    ..base("23. HEVC (hvc1) RT=true, PrioSpeed=true, B=default")
                },
                VtConfig {
                    codec: FOURCC_HVC1,
                    profile_level: None,
                    allow_b_frames: Some(false),
                    ..base("24. HEVC (hvc1) RT=true, PrioSpeed=true, No B-frames")
                },
            ]
        };

        for cfg in &configs {
            let (fps, prof, lvl) = run_vt_config(cfg);
            let mpix_s = (cfg.width as f64 * cfg.height as f64 * fps) / 1_000_000.0;
            let sps_str = if prof > 0 {
                format!("{}/L{:.1}", prof, lvl as f64 / 10.0)
            } else {
                "N/A".to_owned()
            };
            println!(
                "{:<52} | {:>9.2} | {:>11.2} | {:>12.1} | {:>9}",
                cfg.label,
                fps,
                1000.0 / fps,
                mpix_s,
                sps_str
            );
        }

        println!("\n=== 2. Resolution Scaling Curve (H.264 vs HEVC on M4 Media Engine) ===");
        println!(
            "{:<30} | {:>10} | {:>10} | {:>14} | {:>14} | {:>9}",
            "Codec & Resolution", "Macroblks", "FPS", "Macroblks/s", "MPix/s", "SPS (P/L)"
        );
        println!("{}", "-".repeat(99));

        for &(codec, codec_name) in &[(FOURCC_AVC1, "H.264"), (FOURCC_HVC1, "HEVC")] {
            for &(w, h, frames) in &[
                (1280, 720, 360),
                (1920, 1080, 360),
                (2560, 1440, 240),
                (3200, 1800, 180),
                (3840, 2160, 180),
                (4096, 2160, 180),
            ] {
                let cfg = VtConfig {
                    label: codec_name,
                    codec,
                    width: w,
                    height: h,
                    fps: 120,
                    frames,
                    pix_fmt: FOURCC_420V,
                    use_session_pool: false,
                    realtime: Some(true),
                    prio_speed: Some(true),
                    allow_b_frames: Some(false),
                    allow_temporal: None,
                    cabac: None,
                    profile_level: None,
                    expected_fps: Some(240.0),
                    avg_bitrate: Some(8_000_000),
                    cbr_bitrate: None,
                    quality: None,
                    max_keyframe_interval: Some(120),
                    max_frame_delay: None,
                };
                let (fps, prof, lvl) = run_vt_config(&cfg);
                let mb_w = (w as usize).div_ceil(16);
                let mb_h = (h as usize).div_ceil(16);
                let mbs = mb_w * mb_h;
                let mbs_per_sec = mbs as f64 * fps;
                let mpix_s = (w as f64 * h as f64 * fps) / 1_000_000.0;
                let sps_str = if prof > 0 {
                    format!("{}/L{:.1}", prof, lvl as f64 / 10.0)
                } else {
                    "HEVC".to_owned()
                };
                println!(
                    "{:<30} | {:>10} | {:>10.2} | {:>14.0} | {:>14.1} | {:>9}",
                    format!("{codec_name} {w}x{h}"),
                    mbs,
                    fps,
                    mbs_per_sec,
                    mpix_s,
                    sps_str
                );
            }
        }
    }
}

fn main() {
    #[cfg(target_os = "macos")]
    macos_probe::run_all();
}
