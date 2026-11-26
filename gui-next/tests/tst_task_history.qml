import QtQuick
import QtTest
import "../qml/js/ui.js" as Ui

TestCase {
    name: "TaskHistory"

    ListModel { id: rows }

    function sync(ids) {
        Ui.reconcileTaskRows(rows, ids.map(id => ({ id: id })), function(task, index) {
            return { taskId: task.id, rank: index }
        })
    }

    function assertOrder(ids) {
        compare(rows.count, ids.length)
        for (var i = 0; i < ids.length; ++i) {
            compare(rows.get(i).taskId, ids[i])
            compare(rows.get(i).rank, i)
        }
    }

    function test_modelFollowsRustOrder() {
        sync(["a", "b", "c"])
        assertOrder(["a", "b", "c"])
        sync(["c", "d", "a"])
        assertOrder(["c", "d", "a"])
        sync(["d", "a"])
        assertOrder(["d", "a"])
        sync([])
        assertOrder([])
    }

    function test_compactLocalTimestamp() {
        var afternoon = new Date(2026, 0, 5, 16, 3, 9)
        compare(Ui.formatTimestamp(afternoon.getTime()), "2026-01-05 16:03:09")
        compare(Ui.formatTimestamp(0), "—")
    }

    function test_historyPresentation() {
        compare(Ui.taskTitle("Night 夜航"), "Night 夜航")
        compare(Ui.taskTitle(""), qsTranslate("GoosyUi", "Untitled task"))
        compare(Ui.stateLabel("paused"), qsTranslate("GoosyUi", "Paused"))
        compare(Ui.stateLabel("interrupted"), qsTranslate("GoosyUi", "Interrupted"))
        compare(Ui.stateLabel("failed"), qsTranslate("GoosyUi", "Failed"))
        compare(Ui.stateLabel("done"), Ui.stateLabel("completed"))
        compare(Ui.taskSummary("cancelled", "任务已由用户取消"), qsTranslate("GoosyUi", "Cancelled by the user"))
        compare(Ui.taskSummary("failed", "decoder: <invalid stream>"), qsTranslate("GoosyUi", "View error details"))
        compare(Ui.taskSummary("interrupted", "decoder: <invalid stream>"), qsTranslate("GoosyUi", "View task details"))
        compare(Ui.taskSummary("queued", ""), "")
    }

    function test_durableAndLiveProgress() {
        var task = { state: "running", done: 47, committed: 30 }
        compare(Ui.displayedFrames(task), 47)
        task.state = "paused"
        compare(Ui.displayedFrames(task), 30)
        task.state = "interrupted"
        compare(Ui.displayedFrames(task), 30)
        verify(Ui.stateLabel(task.state) !== Ui.stateLabel("idle"))
        task.committed = 0
        compare(Ui.displayedFrames(task), 0)
    }
}
