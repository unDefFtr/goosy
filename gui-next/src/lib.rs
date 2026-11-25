mod backend;

use rinui_host::{AppIdentity, HostConfig, HostError, QmlSource, ResourceMode, RinUIHost};

use std::path::PathBuf;

/// Request the desktop's file manager for a validated local directory.
pub fn open_directory(file_url: &str) -> bool {
    backend::ffi::open_directory(&file_url.into())
}

pub struct RgbaFrame {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

#[derive(Default)]
pub enum FrameUpdate {
    #[default]
    Keep,
    Clear,
    Replace(RgbaFrame),
}

#[derive(Default)]
pub struct Snapshot {
    pub project: Option<String>,
    pub task: Option<String>,
    pub style: FrameUpdate,
    pub render: FrameUpdate,
    pub task_thumbnails: Vec<(String, FrameUpdate)>,
}

pub trait ViewModel {
    fn defaults_json(&self) -> String;
    fn select_asset(&mut self, kind: &str, path: Option<PathBuf>);
    fn set_text(&mut self, key: &str, value: String);
    fn set_number(&mut self, key: &str, value: i32);
    fn set_flag(&mut self, key: &str, value: bool);
    fn command(&mut self, action: &str);
    fn tick(&mut self) -> Snapshot;
}

pub fn run(model: Box<dyn ViewModel>) -> Result<i32, HostError> {
    cxx_qt::init_crate!(goosy_rinui);
    let mut host = RinUIHost::new(HostConfig {
        identity: AppIdentity::new("Goosy", "Goosy GUI Next"),
        resource_mode: ResourceMode::Embedded,
        ..HostConfig::default()
    })?;
    let images = backend::ffi::make_preview_images();
    host.configure_engine(|engine| {
        // SAFETY: only register an engine-owned image provider on the GUI thread.
        unsafe { engine.with_raw_engine_mut(|engine| images.install(engine)) };
    })?;
    let mut backend = backend::ffi::make_backend();
    backend.pin_mut().attach(model, images);
    host.expose_qobject("Backend", backend)?;
    host.load(&QmlSource::Url("qrc:/goosy/Goosy.qml".to_owned()))?;
    Ok(host.exec())
}
