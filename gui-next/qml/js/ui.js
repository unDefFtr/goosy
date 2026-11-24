.pragma library

function isActive(state) {
    return ["starting", "running", "paused"].indexOf(String(state)) >= 0
}

function stateLabel(state) {
    switch (String(state)) {
    case "queued": return "排队中"
    case "starting": return "启动中"
    case "running": return "渲染中"
    case "paused": return "已暂停"
    case "completed": return "已完成"
    case "cancelled": return "已取消"
    case "failed": return "失败"
    default: return "空闲"
    }
}

function stateIcon(state) {
    switch (String(state)) {
    case "starting": return "ic_fluent_arrow_sync_20_regular"
    case "running": return "ic_fluent_play_20_regular"
    case "paused": return "ic_fluent_pause_20_regular"
    case "completed": return "ic_fluent_checkmark_circle_20_regular"
    case "cancelled": return "ic_fluent_dismiss_circle_20_regular"
    case "failed": return "ic_fluent_error_circle_20_regular"
    default: return "ic_fluent_clock_20_regular"
    }
}

function stateColor(state, colors) {
    switch (String(state)) {
    case "failed": return colors.systemCriticalColor
    case "cancelled":
    case "paused": return colors.systemCautionColor
    case "completed": return colors.systemSuccessColor
    case "starting":
    case "running": return colors.primaryColor
    default: return colors.textSecondaryColor
    }
}

function progressValue(done, total) {
    var d = Number(done) || 0
    var t = Number(total) || 0
    if (t <= 0) return 0
    return Math.max(0, Math.min(1, d / t))
}

function progressPercent(done, total) {
    var t = Number(total) || 0
    if (t <= 0) return "—"
    return (Math.round(progressValue(done, total) * 1000) / 10).toFixed(1) + "%"
}

function formatFps(done, elapsed) {
    var frames = Number(done) || 0
    var seconds = Number(elapsed) || 0
    var fps = seconds > 0 ? frames / seconds : 0
    return frames > 0 && isFinite(fps) && fps > 0 ? fps.toFixed(1) + " FPS" : "— FPS"
}

function formatCount(done, total) {
    var d = Number(done) || 0
    var t = Number(total) || 0
    if (t > 0) return d + " / " + t
    return d > 0 ? String(d) : "—"
}

function formatElapsed(seconds) {
    var total = Math.max(0, Math.round(Number(seconds) || 0))
    var hours = Math.floor(total / 3600)
    var minutes = Math.floor((total % 3600) / 60)
    var secs = total % 60
    var pad = function(value) { return value < 10 ? "0" + value : String(value) }
    if (hours > 0) return hours + ":" + pad(minutes) + ":" + pad(secs)
    return minutes + ":" + pad(secs)
}

function formatFrame(frame) {
    var value = Number(frame) || 0
    return value > 0 ? String(value) : "—"
}
