#include "goosy-rinui/src/backend.cxxqt.h"

#include <QQmlApplicationEngine>
#include <QQuickImageProvider>
#include <mutex>
#include <string>
#include <unordered_map>
#include <utility>

namespace goosy_rinui {
struct PreviewState {
    std::mutex mutex;
    QImage style;
    QImage render;
    std::unordered_map<std::string, QImage> tasks;
};

namespace {
class PreviewProvider final : public QQuickImageProvider {
public:
    explicit PreviewProvider(std::shared_ptr<PreviewState> state)
        : QQuickImageProvider(QQuickImageProvider::Image), state(std::move(state)) {}

    QImage requestImage(const QString &id, QSize *size, const QSize &) override {
        const std::lock_guard lock(state->mutex);
        if (id.startsWith(QStringLiteral("task/"))) {
            const QString rest = id.mid(5);
            const qsizetype slash = rest.indexOf(QLatin1Char('/'));
            const std::string key = (slash >= 0 ? rest.left(slash) : rest).toStdString();
            const auto it = state->tasks.find(key);
            const QImage image = it == state->tasks.end() ? QImage() : it->second;
            if (size)
                *size = image.size();
            return image;
        }
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

void PreviewImages::set_task(const QString &id, const QImage &image) const {
    const std::lock_guard lock(state->mutex);
    const std::string key = id.toStdString();
    if (image.isNull()) {
        state->tasks.erase(key);
    } else {
        state->tasks[key] = image;
    }
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
