.pragma library

// Remove departed UUIDs before moving survivors into the order supplied by Rust.
function reconcileTaskRows(model, items, project) {
    var pruned = false
    for (var i = 0; i < items.length; ++i) {
        var values = project(items[i], i)
        if (!pruned && i < model.count && model.get(i).taskId !== values.taskId) {
            var wanted = new Set(items.map(task => String(task.id)))
            for (var j = model.count - 1; j >= i; --j) {
                if (!wanted.has(model.get(j).taskId))
                    model.remove(j)
            }
            pruned = true
        }
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
    if (h >= 12 && h < 18) return qsTranslate("GoosyUi", "Good afternoon.")
    if (h >= 18 && h <= 23) return qsTranslate("GoosyUi", "Good evening.")
    return qsTranslate("GoosyUi", "Good morning.")
}

function isActive(state) {
    return ["starting", "running", "paused"].indexOf(String(state)) >= 0
}

function stateLabel(state) {
    switch (String(state)) {
    case "queued": return qsTranslate("GoosyUi", "Queued")
    case "starting": return qsTranslate("GoosyUi", "Starting")
    case "running": return qsTranslate("GoosyUi", "Rendering")
    case "paused": return qsTranslate("GoosyUi", "Paused")
    case "interrupted": return qsTranslate("GoosyUi", "Interrupted")
    case "done":
    case "completed": return qsTranslate("GoosyUi", "Completed")
    case "cancelled": return qsTranslate("GoosyUi", "Cancelled")
    case "failed": return qsTranslate("GoosyUi", "Failed")
    case "deleting": return qsTranslate("GoosyUi", "Pending deletion")
    default: return qsTranslate("GoosyUi", "Idle")
    }
}

function taskTitle(title) {
    return title || qsTranslate("GoosyUi", "Untitled task")
}

// Existing history stores raw diagnostics. Only exact, known messages have translations.
function taskSummary(state, detail) {
    if (!detail) return ""
    switch (detail) {
    case "任务已由用户取消": return qsTranslate("GoosyUi", "Cancelled by the user")
    case "正在等待前一 Worker 提交分段并释放工作目录": return qsTranslate("GoosyUi", "Waiting for the previous worker to finish its segment and release the workspace")
    case "工作目录创建中断，缺少初始清单": return qsTranslate("GoosyUi", "Workspace creation was interrupted before the initial manifest was written")
    case "渲染监控线程意外退出": return qsTranslate("GoosyUi", "The render monitor stopped unexpectedly")
    case "渲染 Worker 未在宽限期内退出，已请求终止进程树": return qsTranslate("GoosyUi", "The render worker timed out and was asked to stop")
    default: return state === "failed" ? qsTranslate("GoosyUi", "View error details") : qsTranslate("GoosyUi", "View task details")
    }
}

function onOff(enabled) {
    return enabled ? qsTranslate("GoosyUi", "On") : qsTranslate("GoosyUi", "Off")
}

function modeLabel(index) {
    switch (index) {
    case 1: return qsTranslate("GoosyUi", "Range sample")
    case 2: return qsTranslate("GoosyUi", "Random sample")
    default: return qsTranslate("GoosyUi", "Full render")
    }
}

function encoderLabel(value) {
    switch (value) {
    case "pipe": return qsTranslate("GoosyUi", "External FFmpeg")
    case "ffmpeg-next": return qsTranslate("GoosyUi", "ffmpeg-next (automatic)")
    case "ffmpeg-next-nvenc-zero-copy": return qsTranslate("GoosyUi", "Vulkan/CUDA/NVENC zero-copy")
    default: return value || ""
    }
}

function messageText(message) {
    if (!message) return ""
    switch (message.code) {
    case "matchingAssets": return qsTranslate("GoosyUi", "Looking for matching files…")
    case "assetsMatched": return qsTranslate("GoosyUi", "Found matching files")
    case "usingEmbeddedLyrics": return qsTranslate("GoosyUi", "Using the lyrics embedded in the audio")
    case "noMatchingLyrics": return qsTranslate("GoosyUi", "No matching or embedded lyrics found. Choose a lyrics file.")
    case "audioMetadataFailed": return qsTranslate("GoosyUi", "Could not read the audio metadata")
    case "assetWorkerStopped": return qsTranslate("GoosyUi", "The file matching service stopped")
    case "styleWorkerStopped": return qsTranslate("GoosyUi", "The style preview service stopped")
    case "styleAssetsFailed": return qsTranslate("GoosyUi", "Could not load the images for the style preview")
    case "styleRenderFailed": return qsTranslate("GoosyUi", "Could not render the style preview")
    case "zeroCopyPreview": return qsTranslate("GoosyUi", "CPU frame previews are unavailable with zero-copy encoding")
    case "thumbnailSaveFailed": return qsTranslate("GoosyUi", "Could not save the thumbnail")
    case "assetsBusy": return qsTranslate("GoosyUi", "Files are still being matched. Try again in a moment.")
    case "audioRequired": return qsTranslate("GoosyUi", "Select an audio file")
    case "audioReadFailed": return qsTranslate("GoosyUi", "Could not read the audio. Choose another audio file.")
    case "lyricsReadFailed": return qsTranslate("GoosyUi", "Could not read the lyrics. Choose a valid lyrics file.")
    case "embeddedLyricsMissing": return qsTranslate("GoosyUi", "The audio has no embedded lyrics. Choose a lyrics file.")
    case "emptyLyrics": return qsTranslate("GoosyUi", "The lyrics are empty. Choose another lyrics file.")
    case "noSampleRange": return qsTranslate("GoosyUi", "No lyrics are available for sampling. Choose another lyrics file.")
    case "samplePastLyricsEnd": return qsTranslate("GoosyUi", "The sample starts at or after the end of the lyrics. Choose an earlier start time.")
    case "samplePastAudioEnd": return qsTranslate("GoosyUi", "The sample starts at or after the end of the audio. Choose an earlier start time.")
    case "imageReadFailed": return qsTranslate("GoosyUi", "Could not read the image. Choose another image.")
    case "unsupportedCodec": return qsTranslate("GoosyUi", "This encoder does not support the selected codec. Choose another codec.")
    case "oddDimensions": return qsTranslate("GoosyUi", "Set both the width and height to even numbers.")
    case "outputFileRequired": return qsTranslate("GoosyUi", "Choose an output file")
    case "outputExtensionRequired": return qsTranslate("GoosyUi", "Add a container extension to the output filename.")
    case "outputDirectoryRequired": return qsTranslate("GoosyUi", "Choose an output file in a directory.")
    case "outputDirectoryUnavailable": return qsTranslate("GoosyUi", "Could not access the output directory. Choose another directory.")
    case "outputPrivateDirectory": return qsTranslate("GoosyUi", "Choose an output directory outside Goosy’s private staging directory: %1").arg(message.args[0])
    case "outputExists": return qsTranslate("GoosyUi", "This output file already exists. Choose another filename: %1").arg(message.args[0])
    case "outputReserved": return qsTranslate("GoosyUi", "Another Goosy task has reserved this output. Choose another filename: %1").arg(message.args[0])
    case "outputCheckFailed": return qsTranslate("GoosyUi", "Could not check the output path")
    case "sampleOutputFailed": return qsTranslate("GoosyUi", "Could not generate the sample filename. Choose a valid output file.")
    case "assetUnreadable": return qsTranslate("GoosyUi", "The file is missing or unreadable. Locate it again: %1").arg(message.args[0])
    case "assetFingerprintMissing": return qsTranslate("GoosyUi", "No saved fingerprint is available. Locate the original or confirm this file: %1").arg(message.args[0])
    case "assetContentChanged": return qsTranslate("GoosyUi", "The file differs from the saved fingerprint. Locate the original or confirm this file: %1").arg(message.args[0])
    case "taskCreateFailed": return qsTranslate("GoosyUi", "Could not create the task")
    case "concurrencyFailed": return qsTranslate("GoosyUi", "Could not change the concurrent task limit")
    case "assetAcceptFailed": return qsTranslate("GoosyUi", "Could not accept the current file")
    case "taskRedoFailed": return qsTranslate("GoosyUi", "Could not reopen the task for rendering")
    case "taskPauseFailed": return qsTranslate("GoosyUi", "Could not pause the task")
    case "taskResumeFailed": return qsTranslate("GoosyUi", "Could not resume the task")
    case "taskCancelFailed": return qsTranslate("GoosyUi", "Could not cancel the task")
    case "taskDeleteFailed": return qsTranslate("GoosyUi", "Could not delete the task")
    case "outputDirectoryLookupFailed": return qsTranslate("GoosyUi", "Could not locate the output directory")
    case "outputDirectoryOpenFailed": return qsTranslate("GoosyUi", "The file manager could not open the directory: %1").arg(message.args[0])
    case "languageSwitchFailed": return qsTranslate("GoosyUi", "Could not switch languages.")
    case "languageSaveFailed": return qsTranslate("GoosyUi", "The language change could not be saved.")
    default: return qsTranslate("GoosyUi", "View details for more information")
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
