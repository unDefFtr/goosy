#pragma once

#include <QImage>
#include <QString>
#include <memory>

class QQmlApplicationEngine;

namespace goosy_rinui {
class Backend;
struct PreviewState;

class PreviewImages {
public:
    explicit PreviewImages(std::shared_ptr<PreviewState> state);
    void set(bool style, const QImage &image) const;
    void set_task(const QString &id, const QImage &image) const;
    void install(QQmlApplicationEngine &engine) const;

private:
    std::shared_ptr<PreviewState> state;
};

bool open_directory(const QString &file_url);
std::unique_ptr<Backend> make_backend();
std::unique_ptr<PreviewImages> make_preview_images();
}
