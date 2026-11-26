mod backend;

use rinui_host::{
    AppIdentity, AppJsonStore, ConfigLocation, HostConfig, HostError, HostHandle, QmlSource,
    ResourceMode, RinUIHost, TranslationCatalog, TranslationManager,
};
use serde::{Deserialize, Serialize};

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
    fn set_preview_text(&mut self, text: [String; 5]);
    fn command(&mut self, action: &str);
    fn tick(&mut self) -> Snapshot;
}

#[derive(Clone, Default, Deserialize, Serialize)]
struct LanguageConfig {
    language: Option<String>,
}

fn supported_language(locale: &str) -> Option<&'static str> {
    match locale {
        "en_US" => Some("en_US"),
        "zh_CN" => Some("zh_CN"),
        _ => None,
    }
}

fn resolve_language(preference: Option<&'static str>) -> &'static str {
    preference.unwrap_or_else(|| {
        let system = TranslationManager::system_locale();
        if system.split(['_', '-']).next() == Some("zh") {
            "zh_CN"
        } else {
            "en_US"
        }
    })
}

const CATALOG_ID: &str = "goosy";
const CATALOG_PREFIX: &str = ":/goosy/languages";

fn apply_language(host: &HostHandle, language: &str) -> Result<(), HostError> {
    let result = host.set_language(language)?;
    if language == "zh_CN"
        && !result.app_catalogs.iter().any(|catalog| {
            catalog.catalog_id == CATALOG_ID
                && catalog.loaded
                && catalog.selected_locale.as_deref() == Some(language)
        })
    {
        return Err(HostError::TranslationLoadFailed {
            path: format!("{CATALOG_PREFIX}/{language}.qm"),
        });
    }
    Ok(())
}

pub fn run(model: Box<dyn ViewModel>) -> Result<i32, HostError> {
    cxx_qt::init_crate!(goosy_rinui);
    let identity = AppIdentity::new("Goosy", "Goosy GUI Next");
    let mut host = RinUIHost::new(HostConfig {
        identity: identity.clone(),
        resource_mode: ResourceMode::Embedded,
        ..HostConfig::default()
    })?;
    let config_store = host.config_store();
    let config = config_store.borrow();
    // RinUI stores its config at <app local data>/RinUI/config/rin_ui.json.
    let app_data = config.path().and_then(|path| path.ancestors().nth(3));
    let (store, _) = AppJsonStore::load_or_default(
        &ConfigLocation::Standard,
        &identity,
        app_data,
        "language.json",
        LanguageConfig::default(),
    )?;
    drop(config);
    host.register_translation_catalog(TranslationCatalog::QrcPrefix {
        id: CATALOG_ID.into(),
        prefix: CATALOG_PREFIX.into(),
    })?;
    let preference = store.get().language.as_deref().and_then(supported_language);
    let language = resolve_language(preference);
    let handle = host.handle();
    apply_language(&handle, language)?;
    let images = backend::ffi::make_preview_images();
    host.configure_engine(|engine| {
        // SAFETY: only register an engine-owned image provider on the GUI thread.
        unsafe { engine.with_raw_engine_mut(|engine| images.install(engine)) };
    })?;
    let mut backend = backend::ffi::make_backend();
    backend
        .pin_mut()
        .attach(model, images, handle, store, language, preference);
    host.expose_qobject("Backend", backend)?;
    host.load(&QmlSource::Url("qrc:/goosy/Goosy.qml".to_owned()))?;
    Ok(host.exec())
}
