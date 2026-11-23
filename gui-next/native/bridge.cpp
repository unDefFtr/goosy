#include "goosy-rinui/src/backend.cxxqt.h"

#include <QQmlApplicationEngine>
#include <QQuickImageProvider>
#include <mutex>
#include <utility>

namespace goosy_rinui {
struct PreviewState {
    std::mutex mutex;
    QImage style;
    QImage render;
};

namespace {
class PreviewProvider final : public QQuickImageProvider {
public:
    explicit PreviewProvider(std::shared_ptr<PreviewState> state)
        : QQuickImageProvider(QQuickImageProvider::Image), state(std::move(state)) {}

    QImage requestImage(const QString &id, QSize *size, const QSize &) override {
        const std::lock_guard lock(state->mutex);
        const QImage &image = id.startsWith(QStringLiteral("style/")) ? state->style : state->render;
        if (size)
            *size = image.size();
        return image;
    }

private:
    std::shared_ptr<PreviewState> state;
};
}

PreviewImages::PreviewImages(std::shared_ptr<PreviewState> state) : state(std::move(state)) {}

void PreviewImages::set(bool style, const QImage &image) const {
    const std::lock_guard lock(state->mutex);
    (style ? state->style : state->render) = image;
}

void PreviewImages::install(QQmlApplicationEngine &engine) const {
    engine.addImageProvider(QStringLiteral("goosy"), new PreviewProvider(state));
}

std::unique_ptr<Backend> make_backend() {
    return std::make_unique<Backend>();
}

std::unique_ptr<PreviewImages> make_preview_images() {
    return std::make_unique<PreviewImages>(std::make_shared<PreviewState>());
}
}
