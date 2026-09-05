use anyhow::Result;
use std::any::Any;
use std::fmt;
use std::sync::Arc;

/// A fully rendered top-left RGBA8888 premultiplied-sRGB frame.
pub struct RenderedFrame {
    width: u32,
    height: u32,
    row_bytes: usize,
    pixels: Vec<u8>,
    pub(crate) gpu_payload: Option<Arc<dyn Any + Send + Sync>>,
}

impl Clone for RenderedFrame {
    fn clone(&self) -> Self {
        Self {
            width: self.width,
            height: self.height,
            row_bytes: self.row_bytes,
            pixels: self.pixels.clone(),
            gpu_payload: self.gpu_payload.clone(),
        }
    }
}

impl fmt::Debug for RenderedFrame {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RenderedFrame")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("row_bytes", &self.row_bytes)
            .field("pixels_len", &self.pixels.len())
            .field("has_gpu_payload", &self.gpu_payload.is_some())
            .finish()
    }
}

impl PartialEq for RenderedFrame {
    fn eq(&self, other: &Self) -> bool {
        self.width == other.width
            && self.height == other.height
            && self.row_bytes == other.row_bytes
            && self.pixels == other.pixels
    }
}

impl Eq for RenderedFrame {}

impl RenderedFrame {
    pub fn new(width: u32, height: u32) -> Self {
        let row_bytes = width as usize * 4;
        let pixel_len = row_bytes * height as usize;
        Self {
            width,
            height,
            row_bytes,
            pixels: vec![0; pixel_len],
            gpu_payload: None,
        }
    }

    pub fn width(&self) -> u32 {
        self.width
    }
    pub fn height(&self) -> u32 {
        self.height
    }
    pub fn row_bytes(&self) -> usize {
        self.row_bytes
    }
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }
    pub(crate) fn pixels_mut(&mut self) -> &mut Vec<u8> {
        &mut self.pixels
    }

    #[allow(dead_code)]
    pub(crate) fn gpu_payload(&self) -> Option<&Arc<dyn Any + Send + Sync>> {
        self.gpu_payload.as_ref()
    }

    #[allow(dead_code)]
    pub(crate) fn set_gpu_payload(&mut self, payload: Arc<dyn Any + Send + Sync>) {
        self.gpu_payload = Some(payload);
    }
    pub(crate) fn clear_gpu_payload(&mut self) {
        self.gpu_payload = None;
    }
}

pub trait FrameSink {
    fn submit(&mut self, frame: RenderedFrame) -> Result<RenderedFrame>;
    fn finish(&mut self) -> Result<()>;
}
