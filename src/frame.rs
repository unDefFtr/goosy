use anyhow::Result;

/// A fully rendered top-left RGBA8888 premultiplied-sRGB frame.
pub struct RenderedFrame {
    width: u32,
    height: u32,
    row_bytes: usize,
    pixels: Vec<u8>,
    #[cfg(target_os = "macos")]
    gpu_frame: Option<crate::surface::MacosGpuFrame>,
}

impl Clone for RenderedFrame {
    fn clone(&self) -> Self {
        Self {
            width: self.width,
            height: self.height,
            row_bytes: self.row_bytes,
            pixels: self.pixels.clone(),
            #[cfg(target_os = "macos")]
            gpu_frame: None,
        }
    }
}

impl std::fmt::Debug for RenderedFrame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RenderedFrame")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("row_bytes", &self.row_bytes)
            .field("pixels_len", &self.pixels.len())
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
            #[cfg(target_os = "macos")]
            gpu_frame: None,
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

    #[cfg(target_os = "macos")]
    pub(crate) fn set_gpu_frame(&mut self, gpu_frame: Option<crate::surface::MacosGpuFrame>) {
        self.gpu_frame = gpu_frame;
    }

    #[cfg(target_os = "macos")]
    pub(crate) fn take_gpu_frame(&mut self) -> Option<crate::surface::MacosGpuFrame> {
        self.gpu_frame.take()
    }
}

pub trait FrameSink {
    fn submit(&mut self, frame: RenderedFrame) -> Result<RenderedFrame>;
    fn finish(&mut self) -> Result<()>;
}
