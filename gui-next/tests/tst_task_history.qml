import QtQuick
import QtTest
import "../qml/js/ui.js" as Ui

TestCase {
    name: "TaskHistory"

    ListModel { id: rows }

    function sync(ids) {
        Ui.reconcileTaskRows(rows, ids, function(id, index) {
            return { taskId: id, rank: index }
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
