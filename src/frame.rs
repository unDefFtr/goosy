use anyhow::Result;

/// A fully rendered top-left RGBA8888 premultiplied-sRGB frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RenderedFrame {
    width: u32,
    height: u32,
    row_bytes: usize,
    pixels: Vec<u8>,
}

impl RenderedFrame {
    pub fn new(width: u32, height: u32) -> Self {
        let row_bytes = width as usize * 4;
        let pixel_len = row_bytes * height as usize;
        Self {
            width,
            height,
            row_bytes,
            pixels: vec![0; pixel_len],
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
}

pub trait FrameSink {
    fn submit(&mut self, frame: RenderedFrame) -> Result<RenderedFrame>;
    fn finish(&mut self) -> Result<()>;
}
