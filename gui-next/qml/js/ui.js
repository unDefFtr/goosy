.pragma library

// Rust supplies the order. Move existing delegates so focus and selection follow each UUID.
function reconcileTaskRows(model, items, project) {
    for (var i = 0; i < items.length; ++i) {
        var values = project(items[i], i)
        var index = i
        while (index < model.count && model.get(index).taskId !== values.taskId)
            ++index
        if (index === model.count) {
            model.insert(i, values)
            continue
        }
        if (index !== i)
            model.move(index, i, 1)
        var row = model.get(i)
        for (var key in values) {
            if (row[key] !== values[key])
                model.setProperty(i, key, values[key])
        }
    }
    if (model.count > items.length)
        model.remove(items.length, model.count - items.length)
}

function formatTimestamp(ms) {
    return Number(ms) > 0 ? Qt.formatDateTime(new Date(Number(ms)), "yyyy-MM-dd HH:mm:ss") : "—"
}

function formatBytes(bytes) {
    var size = Number(bytes) || 0
    if (size < 1024) return size + " B"
    var units = ["KiB", "MiB", "GiB", "TiB"]
    var i = 0
    size /= 1024
    while (size >= 1024 && i < units.length - 1) {
        size /= 1024
        ++i
    }
    return size.toFixed(1) + " " + units[i]
}

function displayedFrames(task) {
    var live = task.state === "running" || task.state === "starting"
    return Number(live ? task.done : task.committed) || 0
}

function greetingText(hour) {
    var h = (hour === undefined || hour === null || hour < 0)
        ? new Date().getHours()
        : Number(hour)
    if (!isFinite(h) || h < 0 || h > 23) return "Good morning."
    if (h >= 12 && h < 18) return "Good afternoon."
    if (h >= 18) return "Good evening."
    return "Good morning."
}

function isActive(state) {
    return ["starting", "running", "paused"].indexOf(String(state)) >= 0
}

function stateLabel(state) {
    switch (String(state)) {
    case "queued": return "排队中"
    case "starting": return "启动中"
    case "running": return "渲染中"
    case "paused": return "已暂停"
    case "interrupted": return "已中断"
    case "done":
    case "completed": return "已完成"
    case "cancelled": return "已取消"
    case "failed": return "失败"
    case "deleting": return "待完成删除"
    default: return "空闲"
    }
}

function stateIcon(state) {
    switch (String(state)) {
    case "queued": return "ic_fluent_clock_20_regular"
    case "starting":
    case "running": return "ic_fluent_play_20_regular"
    case "interrupted": return "ic_fluent_clock_20_regular"
    case "paused": return "ic_fluent_pause_20_regular"
    case "done":
    case "completed": return "ic_fluent_checkmark_circle_20_regular"
    case "failed": return "ic_fluent_error_circle_20_regular"
    case "cancelled": return "ic_fluent_dismiss_circle_20_regular"
    case "deleting": return "ic_fluent_delete_20_regular"
    default: return "ic_fluent_circle_20_regular"
    }
}

function stateColor(state, colors) {
    switch (String(state)) {
    case "failed": return colors.systemCriticalColor
    case "cancelled":
    case "deleting":
    case "interrupted":
    case "paused": return colors.systemCautionColor
    case "done":
    case "completed": return colors.systemSuccessColor
    case "starting":
    case "running": return colors.primaryColor
    default: return colors.textSecondaryColor
    }
}

function stateBackgroundColor(state, colors) {
    switch (String(state)) {
    case "failed": return colors.systemCriticalBackgroundColor
    case "cancelled":
    case "deleting":
    case "interrupted":
    case "paused": return colors.systemCautionBackgroundColor
    case "done":
    case "completed": return colors.systemSuccessBackgroundColor
    case "starting":
    case "running": return colors.systemAttentionBackgroundColor
    default: return colors.systemNeutralBackgroundColor
    }
}

function formatOutputSpec(width, height, fps) {
    var w = Number(width) || 0
    var h = Number(height) || 0
    var f = Number(fps) || 0
    var res = (w > 0 && h > 0) ? (w + "×" + h) : ""
    if (res && f > 0) return res + " @ " + f + "fps"
    if (res) return res
    if (f > 0) return f + "fps"
    return ""
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

function formatFps(fps) {
    var value = Number(fps) || 0
    return isFinite(value) && value > 0 ? value.toFixed(1) + " FPS" : "— FPS"
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
