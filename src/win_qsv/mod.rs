//! Windows DirectX (D3D12 + D3D11) + Intel Quick Sync Video (QSV / oneVPL)
//! zero-copy / less-copy GPU rendering and hardware encoding pipeline.

pub mod d3d11_pipeline;
pub mod encoder;
pub mod mfx_ffi;

use std::sync::atomic::{AtomicU64, Ordering};

pub use d3d11_pipeline::{
    D3d12SharedRgbaRing, Nv12SurfaceLease, Nv12SurfacePool, SharedD3d11Context,
};
pub use encoder::{
    QsvCapabilities, QsvEncodeConfig, QsvLowPowerMode, WinQsvAsyncEncoder, probe_qsv_capabilities,
};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ColorMatrix {
    Bt601,
    #[default]
    Bt709,
    Bt2020,
}

impl ColorMatrix {
    pub fn from_env() -> Self {
        match std::env::var("GOOSY_QSV_COLOR_MATRIX")
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "bt601" | "601" => Self::Bt601,
            "bt2020" | "2020" => Self::Bt2020,
            _ => Self::Bt709,
        }
    }

    /// ITU-T H.273 / VUI color primaries, transfer characteristics, matrix coefficients.
    pub fn vui_codes(self) -> (u16, u16, u16) {
        match self {
            Self::Bt601 => (6, 6, 6),   // SMPTE 170M
            Self::Bt709 => (1, 1, 1),   // BT.709
            Self::Bt2020 => (9, 14, 9), // BT.2020
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ColorRange {
    #[default]
    Limited,
    Full,
}

impl ColorRange {
    pub fn from_env() -> Self {
        match std::env::var("GOOSY_QSV_COLOR_RANGE")
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "full" | "pc" => Self::Full,
            _ => Self::Limited,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Nv12ConverterMode {
    /// Auto-selects direct NV12 UAV Compute Shader when supported, else Video Processor.
    #[default]
    Auto,
    /// D3D11 Compute Shader writing directly into NV12 R8_UNORM + R8G8_UNORM UAVs.
    ComputeDirect,
    /// D3D11 Compute Shader writing into two intermediate R8 + RG8 UAVs, then RTV blit to NV12.
    ComputeIntermediate,
    /// D3D11 Video Processor (`ID3D11VideoContext::VideoProcessorBlt`).
    VideoProcessor,
}

impl Nv12ConverterMode {
    pub fn from_env() -> Self {
        match std::env::var("GOOSY_QSV_CONVERTER")
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "compute" | "cs" | "compute-direct" => Self::ComputeDirect,
            "compute-intermediate" | "cs-staged" => Self::ComputeIntermediate,
            "vp" | "videoprocessor" | "video-processor" => Self::VideoProcessor,
            _ => Self::Auto,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::ComputeDirect => "d3d11-compute-direct-nv12-uav",
            Self::ComputeIntermediate => "d3d11-compute-intermediate-uav",
            Self::VideoProcessor => "d3d11-video-processor",
        }
    }
}

/// GPU-resident NV12 frame produced on the shared D3D11 device for direct zero-copy
/// handoff to Intel QSV (`MFXVideoENCODE_EncodeFrameAsync`).
pub struct WinGpuFrame {
    pub lease: Nv12SurfaceLease,
    pub width: u32,
    pub height: u32,
    pub aligned_width: u32,
    pub aligned_height: u32,
    pub color_matrix: ColorMatrix,
    pub color_range: ColorRange,
    pub converter_used: Nv12ConverterMode,
}

// Pool and zero-copy telemetry counters.
static POOL_ACQUIRED: AtomicU64 = AtomicU64::new(0);
static POOL_RELEASED: AtomicU64 = AtomicU64::new(0);
static POOL_CAPACITY: AtomicU64 = AtomicU64::new(0);
static GPU_FRAMES_ENCODED: AtomicU64 = AtomicU64::new(0);
static CPU_FALLBACK_FRAMES: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug, Default)]
pub struct WinQsvTelemetry {
    pub acquired_count: u64,
    pub released_count: u64,
    pub in_flight_count: u64,
    pub pool_capacity: u64,
    pub gpu_frames_encoded: u64,
    pub cpu_fallback_frames: u64,
}

pub fn win_qsv_telemetry() -> WinQsvTelemetry {
    let acquired = POOL_ACQUIRED.load(Ordering::Relaxed);
    let released = POOL_RELEASED.load(Ordering::Relaxed);
    WinQsvTelemetry {
        acquired_count: acquired,
        released_count: released,
        in_flight_count: acquired.saturating_sub(released),
        pool_capacity: POOL_CAPACITY.load(Ordering::Relaxed),
        gpu_frames_encoded: GPU_FRAMES_ENCODED.load(Ordering::Relaxed),
        cpu_fallback_frames: CPU_FALLBACK_FRAMES.load(Ordering::Relaxed),
    }
}

pub fn reset_win_qsv_telemetry() {
    POOL_ACQUIRED.store(0, Ordering::Relaxed);
    POOL_RELEASED.store(0, Ordering::Relaxed);
    GPU_FRAMES_ENCODED.store(0, Ordering::Relaxed);
    CPU_FALLBACK_FRAMES.store(0, Ordering::Relaxed);
}

pub(crate) fn record_pool_capacity(cap: usize) {
    POOL_CAPACITY.store(cap as u64, Ordering::Relaxed);
}
pub(crate) fn record_pool_acquire() {
    POOL_ACQUIRED.fetch_add(1, Ordering::Relaxed);
}
pub(crate) fn record_pool_release() {
    POOL_RELEASED.fetch_add(1, Ordering::Relaxed);
}
pub(crate) fn record_gpu_frame_encoded() {
    GPU_FRAMES_ENCODED.fetch_add(1, Ordering::Relaxed);
}
pub(crate) fn record_cpu_fallback_frame() {
    CPU_FALLBACK_FRAMES.fetch_add(1, Ordering::Relaxed);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color_matrix_vui_codes_match_itu_h273() {
        assert_eq!(ColorMatrix::Bt601.vui_codes(), (6, 6, 6));
        assert_eq!(ColorMatrix::Bt709.vui_codes(), (1, 1, 1));
        assert_eq!(ColorMatrix::Bt2020.vui_codes(), (9, 14, 9));
    }

    #[test]
    fn qsv_probe_rejects_odd_or_tiny_dimensions() {
        assert!(probe_qsv_capabilities(crate::VideoCodec::H264, 1921, 1080, 60).is_err());
        assert!(probe_qsv_capabilities(crate::VideoCodec::H264, 32, 32, 60).is_err());
    }

    #[test]
    fn telemetry_tracks_pool_balance() {
        reset_win_qsv_telemetry();
        record_pool_capacity(12);
        record_pool_acquire();
        record_pool_acquire();
        record_pool_release();
        let t = win_qsv_telemetry();
        assert_eq!(t.pool_capacity, 12);
        assert_eq!(t.acquired_count, 2);
        assert_eq!(t.released_count, 1);
        assert_eq!(t.in_flight_count, 1);
        record_pool_release();
        assert_eq!(win_qsv_telemetry().in_flight_count, 0);
    }
}
