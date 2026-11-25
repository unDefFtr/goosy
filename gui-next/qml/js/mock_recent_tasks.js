.pragma library

// ============================================================================
// MOCK DATA FOR HOME WORKBENCH PREVIEW & TESTING
// ----------------------------------------------------------------------------
// Production Home.qml binds directly to `Backend.taskJson.recentTasks`
// (sorted by last activity time descending in `src/gui_next/model.rs`).
// Set `Home.mockPreset` to "empty" | "single" | "allStates" | "many" to
// preview or test all task states, empty state, and long-list scrolling
// without starting real render workers.
// ============================================================================

function _svgThumb(c1, c2, label) {
    var svg = "<svg xmlns='http://www.w3.org/2000/svg' width='192' height='108' viewBox='0 0 192 108'>"
        + "<defs><linearGradient id='g' x1='0' y1='0' x2='1' y2='1'>"
        + "<stop offset='0%' stop-color='" + c1 + "'/>"
        + "<stop offset='100%' stop-color='" + c2 + "'/>"
        + "</linearGradient></defs>"
        + "<rect width='192' height='108' fill='url(#g)'/>"
        + "<rect x='16' y='28' width='40' height='40' rx='6' fill='#ffffff' fill-opacity='0.24'/>"
        + "<rect x='68' y='34' width='88' height='8' rx='4' fill='#ffffff' fill-opacity='0.82'/>"
        + "<rect x='68' y='50' width='64' height='6' rx='3' fill='#ffffff' fill-opacity='0.50'/>"
        + "<rect x='68' y='62' width='76' height='6' rx='3' fill='#ffffff' fill-opacity='0.34'/>"
        + "</svg>"
    return "data:image/svg+xml;utf8," + encodeURIComponent(svg)
}

function allStatesTasks() {
    var now = Date.now()
    return [
        {
            id: "mock-101",
            title: "夜航星 (Night Voyager) — 4K HDR 歌词版",
            state: "running",
            done: 4512,
            total: 7200,
            elapsed: 38.4,
            fps: 117.5,
            output: "D:/Renders/night-voyager-4k120.mp4",
            detail: "",
            thumbnail: _svgThumb("#1b3a6b", "#4e2a84"),
            updatedAt: now - 1000,
            lastActivitySeq: 107,
            config: { width: 3840, height: 2160, fps: 120, encoder: "NVENC", codec: "H.265" }
        },
        {
            id: "mock-102",
            title: "晴天 · 双语逐字动态歌词（超长标题用于验证单行省略号截断效果是否正常显示）",
            state: "completed",
            done: 14400,
            total: 14400,
            elapsed: 112.0,
            fps: 128.6,
            output: "D:/Renders/qingtian-1080p60.mp4",
            detail: "",
            thumbnail: _svgThumb("#0e5a6d", "#2c8c7c"),
            updatedAt: now - 60000,
            lastActivitySeq: 106,
            config: { width: 1920, height: 1080, fps: 60, encoder: "QSV", codec: "H.264" }
        },
        {
            id: "mock-103",
            title: "Lemon — Kenshi Yonezu",
            state: "paused",
            done: 2880,
            total: 7200,
            elapsed: 45.2,
            fps: 63.7,
            output: "D:/Renders/lemon-2k60.mp4",
            detail: "",
            thumbnail: _svgThumb("#5b3a29", "#b07d3c"),
            updatedAt: now - 180000,
            lastActivitySeq: 105,
            config: { width: 2560, height: 1440, fps: 60, encoder: "NVENC", codec: "H.264" }
        },
        {
            id: "mock-104",
            title: "群青 (YOASOBI)",
            state: "starting",
            done: 0,
            total: 0,
            elapsed: 0.4,
            fps: 0,
            output: "D:/Renders/gunjou-4k60.mp4",
            detail: "",
            thumbnail: "",
            updatedAt: now - 240000,
            lastActivitySeq: 104,
            config: { width: 3840, height: 2160, fps: 60, encoder: "NVENC", codec: "H.265" }
        },
        {
            id: "mock-105",
            title: "打上花火 — Live Remix",
            state: "queued",
            done: 0,
            total: 0,
            elapsed: 0,
            fps: 0,
            output: "D:/Renders/hanabi-1080p60.mp4",
            detail: "",
            thumbnail: "",
            updatedAt: now - 360000,
            lastActivitySeq: 103,
            config: { width: 1920, height: 1080, fps: 60, encoder: "FFmpeg", codec: "H.264" }
        },
        {
            id: "mock-106",
            title: "红莲华 (LiSA) — 采样测试",
            state: "failed",
            done: 120,
            total: 900,
            elapsed: 3.1,
            fps: 38.7,
            output: "D:/Renders/gurenge-sample.mp4",
            detail: "编码器初始化失败：输出设备不支持当前分辨率",
            thumbnail: "",
            updatedAt: now - 720000,
            lastActivitySeq: 102,
            config: { width: 3840, height: 2160, fps: 120, encoder: "NVENC", codec: "AV1" }
        },
        {
            id: "mock-107",
            title: "起风了 — 纯享版",
            state: "cancelled",
            done: 640,
            total: 10800,
            elapsed: 11.8,
            fps: 54.2,
            output: "D:/Renders/qifengle.mp4",
            detail: "",
            thumbnail: _svgThumb("#2d3748", "#4a5568"),
            updatedAt: now - 1440000,
            lastActivitySeq: 101,
            config: { width: 1920, height: 1080, fps: 30, encoder: "FFmpeg", codec: "H.264" }
        }
    ]
}

function singleTask() {
    return [allStatesTasks()[1]]
}

function manyTasks(count) {
    var total = Math.min(20, Math.max(1, Number(count) || 20))
    var base = allStatesTasks()
    var result = []
    for (var i = 0; i < total; ++i) {
        var tpl = base[i % base.length]
        result.push({
            id: "mock-many-" + (i + 1),
            title: "#" + (i + 1) + "  " + tpl.title,
            state: tpl.state,
            done: tpl.done,
            total: tpl.total,
            elapsed: tpl.elapsed,
            fps: tpl.fps,
            output: tpl.output,
            detail: tpl.detail,
            thumbnail: (i % 3 === 0) ? "" : tpl.thumbnail,
            updatedAt: tpl.updatedAt - i * 30000,
            lastActivitySeq: 1000 - i,
            config: tpl.config
        })
    }
    return result
}

function preset(name) {
    switch (String(name || "")) {
    case "empty": return []
    case "single": return singleTask()
    case "allStates": return allStatesTasks()
    case "many": return manyTasks(20)
    default: return null
    }
}
