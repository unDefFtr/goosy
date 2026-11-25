use crate::{FrameUpdate, Snapshot, ViewModel};
use cxx::UniquePtr;
use rinui_host::cxx_qt_lib::{QImage, QImageFormat, QString, QUrl};
use std::{path::PathBuf, pin::Pin};

#[cxx_qt::bridge(namespace = "goosy_rinui")]
pub mod ffi {
    #[namespace = ""]
    unsafe extern "C++" {
        include!("cxx-qt-lib/qstring.h");
        type QString = rinui_host::cxx_qt_lib::QString;
        include!("cxx-qt-lib/qurl.h");
        type QUrl = rinui_host::cxx_qt_lib::QUrl;
        include!("cxx-qt-lib/qimage.h");
        type QImage = rinui_host::cxx_qt_lib::QImage;
        include!("cxx-qt-lib/qqmlapplicationengine.h");
        type QQmlApplicationEngine = rinui_host::cxx_qt_lib::QQmlApplicationEngine;
    }

    unsafe extern "C++" {
        include!("bridge.h");
        type PreviewImages;
        fn open_directory(path: &QString) -> bool;
        fn make_backend() -> UniquePtr<Backend>;
        fn make_preview_images() -> UniquePtr<PreviewImages>;
        fn install(self: &PreviewImages, engine: Pin<&mut QQmlApplicationEngine>);
        fn set(self: &PreviewImages, style: bool, image: &QImage);
        fn set_task(self: &PreviewImages, id: &QString, image: &QImage);
    }

    extern "RustQt" {
        #[qobject]
        #[qproperty(QString, defaults_json, cxx_name = "defaultsJson", READ, CONSTANT)]
        #[qproperty(QString, project_json, cxx_name = "projectJson", READ, NOTIFY)]
        #[qproperty(QString, task_json, cxx_name = "taskJson", READ, NOTIFY)]
        #[qproperty(QString, style_source, cxx_name = "styleSource", READ, NOTIFY)]
        #[qproperty(QString, render_source, cxx_name = "renderSource", READ, NOTIFY)]
        type Backend = super::BackendRust;

        #[qinvokable]
        fn tick(self: Pin<&mut Backend>);
        #[qinvokable]
        #[cxx_name = "selectAsset"]
        fn select_asset(self: Pin<&mut Backend>, kind: &QString, url: &QUrl);
        #[qinvokable]
        #[cxx_name = "setText"]
        fn set_text(self: Pin<&mut Backend>, key: &QString, value: &QString);
        #[qinvokable]
        #[cxx_name = "setNumber"]
        fn set_number(self: Pin<&mut Backend>, key: &QString, value: i32);
        #[qinvokable]
        #[cxx_name = "setFlag"]
        fn set_flag(self: Pin<&mut Backend>, key: &QString, value: bool);
        #[qinvokable]
        fn command(self: Pin<&mut Backend>, action: &QString);
    }
}

#[derive(Default)]
pub struct BackendRust {
    defaults_json: QString,
    project_json: QString,
    task_json: QString,
    style_source: QString,
    render_source: QString,
    revision: u64,
    model: Option<Box<dyn ViewModel>>,
    images: Option<UniquePtr<ffi::PreviewImages>>,
}

use cxx_qt::CxxQtType;

impl ffi::Backend {
    pub(crate) fn attach(
        mut self: Pin<&mut Self>,
        model: Box<dyn ViewModel>,
        images: UniquePtr<ffi::PreviewImages>,
    ) {
        let mut rust = self.as_mut().rust_mut();
        rust.defaults_json = QString::from(model.defaults_json().as_str());
        rust.model = Some(model);
        rust.images = Some(images);
        drop(rust);
        self.tick();
    }

    pub fn tick(mut self: Pin<&mut Self>) {
        self.as_mut().update(|_| {});
    }

    pub fn select_asset(mut self: Pin<&mut Self>, kind: &QString, url: &QUrl) {
        let path = url
            .to_local_file()
            .map(|path| PathBuf::from(path.to_string()));
        if path.is_some() || url.is_empty() {
            self.as_mut()
                .update(|model| model.select_asset(&kind.to_string(), path));
        }
    }

    pub fn set_text(mut self: Pin<&mut Self>, key: &QString, value: &QString) {
        self.as_mut()
            .update(|model| model.set_text(&key.to_string(), value.to_string()));
    }

    pub fn set_number(mut self: Pin<&mut Self>, key: &QString, value: i32) {
        self.as_mut()
            .update(|model| model.set_number(&key.to_string(), value));
    }

    pub fn set_flag(mut self: Pin<&mut Self>, key: &QString, value: bool) {
        self.as_mut()
            .update(|model| model.set_flag(&key.to_string(), value));
    }

    pub fn command(mut self: Pin<&mut Self>, action: &QString) {
        self.as_mut()
            .update(|model| model.command(&action.to_string()));
    }

    fn update(mut self: Pin<&mut Self>, action: impl FnOnce(&mut dyn ViewModel)) {
        let Some(Snapshot {
            project,
            task,
            style,
            render,
            task_thumbnails,
        }) = (|| {
            let mut rust = self.as_mut().rust_mut();
            let model = rust.model.as_deref_mut()?;
            action(model);
            Some(model.tick())
        })()
        else {
            return;
        };
        self.as_mut().update_task_thumbnails(task_thumbnails);
        if let Some(project) = project {
            self.as_mut().rust_mut().project_json = QString::from(project.as_str());
            self.as_mut().project_json_changed();
        }
        if let Some(task) = task {
            self.as_mut().rust_mut().task_json = QString::from(task.as_str());
            self.as_mut().task_json_changed();
        }
        self.as_mut().update_image(true, style);
        self.update_image(false, render);
    }

    fn frame_to_qimage(update: FrameUpdate) -> Option<QImage> {
        match update {
            FrameUpdate::Keep => None,
            FrameUpdate::Clear => Some(QImage::default()),
            FrameUpdate::Replace(frame) => {
                let (Ok(width), Ok(height)) =
                    (i32::try_from(frame.width), i32::try_from(frame.height))
                else {
                    return None;
                };
                if width <= 0
                    || height <= 0
                    || (width as usize)
                        .checked_mul(4)
                        .and_then(|stride| stride.checked_mul(height as usize))
                        != Some(frame.pixels.len())
                {
                    return None;
                }
                // SAFETY: packed RGBA8888, 4-byte-aligned scanlines. Qt owns the Vec until
                // the last implicitly shared QImage goes away, including image provider copies.
                Some(unsafe {
                    QImage::from_raw_bytes(
                        frame.pixels,
                        width,
                        height,
                        QImageFormat::Format_RGBA8888_Premultiplied,
                    )
                })
            }
        }
    }

    fn update_task_thumbnails(
        mut self: Pin<&mut Self>,
        updates: Vec<(String, FrameUpdate)>,
    ) {
        if updates.is_empty() {
            return;
        }
        let rust = self.as_mut().rust_mut();
        let Some(images) = &rust.images else { return };
        for (id, update) in updates {
            let Some(image) = Self::frame_to_qimage(update) else {
                continue;
            };
            images.set_task(&QString::from(id.as_str()), &image);
        }
    }

    fn update_image(mut self: Pin<&mut Self>, style: bool, update: FrameUpdate) {
        let Some(image) = Self::frame_to_qimage(update) else {
            return;
        };
        let mut rust = self.as_mut().rust_mut();
        let Some(images) = &rust.images else { return };
        images.set(style, &image);
        rust.revision += 1;
        let source = if image.is_null() {
            QString::default()
        } else {
            let kind = if style { "style" } else { "render" };
            QString::from(format!("image://goosy/{kind}/{}", rust.revision).as_str())
        };
        if style {
            rust.style_source = source;
            drop(rust);
            self.style_source_changed();
        } else {
            rust.render_source = source;
            drop(rust);
            self.render_source_changed();
        }
    }
}
