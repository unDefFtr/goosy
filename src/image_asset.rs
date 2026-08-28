use anyhow::{Context, Result, bail};
use skia_safe::image::CachingHint;
use skia_safe::{Data, Image};
use std::path::Path;

#[derive(Clone)]
pub struct ImageAsset {
    image: Image,
}

impl ImageAsset {
    pub fn from_path(path: &Path) -> Result<Self> {
        let bytes =
            std::fs::read(path).with_context(|| format!("read image {}", path.display()))?;
        Self::from_bytes(&bytes).with_context(|| format!("load image {}", path.display()))
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        if bytes.is_empty() {
            bail!("image data is empty");
        }
        let encoded =
            Image::from_encoded(Data::new_copy(bytes)).context("decode image encoding")?;
        let image = encoded
            .make_raster_image(None, CachingHint::Disallow)
            .context("materialize decoded image as raster pixels")?;
        Ok(Self { image })
    }

    pub(crate) fn image(&self) -> &Image {
        &self.image
    }
}

#[derive(Clone)]
pub struct SceneImageAssets {
    pub background: Option<ImageAsset>,
    pub cover: Option<ImageAsset>,
}

pub fn load_scene_image_assets(
    background: Option<&Path>,
    cover: Option<&Path>,
    embedded_cover: Option<&[u8]>,
) -> Result<SceneImageAssets> {
    if let Some(background_path) = background {
        let background_asset = ImageAsset::from_path(background_path)?;
        let cover_asset = match cover {
            Some(cover_path) if cover_path == background_path => Some(background_asset.clone()),
            Some(cover_path) => Some(ImageAsset::from_path(cover_path)?),
            None => embedded_cover.map(ImageAsset::from_bytes).transpose()?,
        };
        return Ok(SceneImageAssets {
            background: Some(background_asset),
            cover: cover_asset,
        });
    }

    if let Some(cover_path) = cover {
        let asset = ImageAsset::from_path(cover_path)?;
        return Ok(SceneImageAssets {
            background: Some(asset.clone()),
            cover: Some(asset),
        });
    }

    if let Some(bytes) = embedded_cover {
        let asset = ImageAsset::from_bytes(bytes)?;
        return Ok(SceneImageAssets {
            background: Some(asset.clone()),
            cover: Some(asset),
        });
    }

    Ok(SceneImageAssets {
        background: None,
        cover: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use skia_safe::{EncodedImageFormat, ImageInfo, Pixmap};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn test_png(color: [u8; 4]) -> (Vec<u8>, Vec<u8>) {
        let info = ImageInfo::new_n32_premul((2, 2), None);
        let mut pixels = color.repeat(4);
        let expected = pixels.clone();
        let pixmap = Pixmap::new(&info, &mut pixels, 8).unwrap();
        (
            pixmap.encode(EncodedImageFormat::PNG, 100).unwrap(),
            expected,
        )
    }

    fn test_path(name: &str) -> std::path::PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "goosy-image-asset-{name}-{}-{nonce}.png",
            std::process::id()
        ))
    }

    #[test]
    fn image_asset_is_eager_raster_with_unchanged_pixels() {
        let (png, expected) = test_png([40, 80, 220, 255]);
        let asset = ImageAsset::from_bytes(&png).unwrap();

        assert!(!asset.image().is_lazy_generated());
        assert!(asset.image().peek_pixels().is_some());
        let pixmap = asset.image().peek_pixels().unwrap();
        assert_eq!(pixmap.bytes().unwrap(), expected);
    }

    #[test]
    fn embedded_cover_is_shared_by_background_and_cover() {
        let (png, _) = test_png([40, 80, 220, 255]);
        let assets = load_scene_image_assets(None, None, Some(&png)).unwrap();

        assert_eq!(
            assets.background.unwrap().image().unique_id(),
            assets.cover.unwrap().image().unique_id()
        );
    }

    #[test]
    fn identical_paths_are_loaded_once_and_shared() {
        let (png, _) = test_png([40, 80, 220, 255]);
        let path = test_path("shared");
        std::fs::write(&path, png).unwrap();

        let assets = load_scene_image_assets(Some(&path), Some(&path), None).unwrap();
        std::fs::remove_file(path).unwrap();

        assert_eq!(
            assets.background.unwrap().image().unique_id(),
            assets.cover.unwrap().image().unique_id()
        );
    }

    #[test]
    fn explicit_different_background_is_a_distinct_asset() {
        let (background_png, _) = test_png([220, 40, 80, 255]);
        let (cover_png, _) = test_png([40, 80, 220, 255]);
        let background_path = test_path("background");
        let cover_path = test_path("cover");
        std::fs::write(&background_path, background_png).unwrap();
        std::fs::write(&cover_path, cover_png).unwrap();

        let assets =
            load_scene_image_assets(Some(&background_path), Some(&cover_path), None).unwrap();
        std::fs::remove_file(background_path).unwrap();
        std::fs::remove_file(cover_path).unwrap();

        assert_ne!(
            assets.background.unwrap().image().unique_id(),
            assets.cover.unwrap().image().unique_id()
        );
    }
}
