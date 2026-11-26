import QtQuick
import QtTest
import "../qml/pages" as Pages

TestCase {
    id: suite
    name: "TaskMenu"
    width: 960
    height: 600
    visible: true
    when: windowShown

    readonly property string selected: "9f98ed66-f699-45cd-a171-4861ce4c4663"
    readonly property string target: "9258716b-3253-41a3-89a8-369d12b07432"
    readonly property var operations: ["pause", "resume", "cancel", "redo", "openOutput", "delete"]
    property var page
    readonly property var operationLabels: ({
        pause: qsTranslate("Tasks", "Pause"), resume: qsTranslate("Tasks", "Resume"),
        cancel: qsTranslate("Tasks", "Cancel"), redo: qsTranslate("Tasks", "Redo"),
        openOutput: qsTranslate("Tasks", "Open output folder"), delete: qsTranslate("Tasks", "Delete")
    })
    property var list
    property var menu

    QtObject {
        id: backendFixture
        property string taskJson: "{}"
        property string renderSource: ""
        property string styleSource: ""
        property var commands: []
        function command(value) { commands.push(value) }
    }

    Component {
        id: pageComponent
        Pages.Tasks { width: suite.width; height: suite.height; backend: backendFixture }
    }

    function row(id, state, actions) {
        return { id: id, title: id, state: state, actions: actions }
    }

    function publish(rows, error, selectedId) {
        const selection = rows.find(r => r.id === (selectedId || selected)) || {}
        backendFixture.taskJson = JSON.stringify(Object.assign({}, selection, {
            selectedId: selection.id || "", tasks: rows, error: error || null
        }))
        list.forceLayout()
    }

    SignalSpy {
        id: rowMoves
        target: suite.list ? suite.list.model : null
        signalName: "rowsMoved"
    }

    function selectedRow() {
        return row(selected, "completed", ["redo", "openOutput", "delete"])
    }

    function setTarget(state, actions) {
        publish([selectedRow(), row(target, state, actions)])
    }

    function init() {
        backendFixture.commands = []
        backendFixture.taskJson = "{}"
        page = createTemporaryObject(pageComponent, suite)
        verify(page)
        list = findChild(page, "taskList")
        menu = findChild(page, "taskContextMenu")
        verify(list && menu)
    }

    function openTarget() {
        tryVerify(() => list.itemAtIndex(1) !== null)
        const card = list.itemAtIndex(1)
        mouseClick(card, card.width / 2, card.height / 2, Qt.RightButton)
        tryCompare(menu, "opened", true)
        compare(list.currentIndex, 0)
        compare(backendFixture.commands.length, 0)
    }

    function actionItem(action) {
        for (let i = 0; i < menu.count; ++i) {
            if (menu.itemAt(i).operation === action) return menu.itemAt(i)
        }
        fail("Missing menu item " + action)
    }

    function clickAction(action) {
        const item = actionItem(action)
        verify(item.visible && item.enabled)
        mouseClick(item)
        tryCompare(menu, "visible", false)
    }

    function test_stateVisibility_data() {
        const history = ["redo", "openOutput", "delete"]
        return [
            { tag: "running", actions: ["pause", "cancel"].concat(history) },
            { tag: "paused", actions: ["resume", "cancel"].concat(history) },
            { tag: "interrupted", actions: ["resume"].concat(history) },
            { tag: "completed", actions: history },
            { tag: "failed", actions: history },
            { tag: "cancelled", actions: history },
            { tag: "queued", actions: ["cancel"].concat(history) }
        ]
    }

    function test_stateVisibility(data) {
        setTarget(data.tag, data.actions)
        openTarget()
        for (const op of operations) {
            compare(actionItem(op).visible, data.actions.includes(op), op)
            compare(actionItem(op).text, operationLabels[op], op)
        }
        compare(menu.count, operations.length)
    }

    function test_leftClickSelects() {
        setTarget("completed", ["redo"])
        tryVerify(() => list.itemAtIndex(1) !== null)
        mouseClick(list.itemAtIndex(1))
        compare(menu.visible, false)
        compare(backendFixture.commands, ["select:" + target])
    }

    function test_scrolledHistorySurvivesRefresh() {
        const rows = []
        for (let i = 0; i < 48; ++i)
            rows.push(row(i === 3 ? selected : "task-" + i, "queued", ["cancel"]))
        publish(rows)
        tryCompare(list.updateAnimation, "running", false)
        list.positionViewAtIndex(24, ListView.Beginning)
        list.contentY += 7
        const viewed = list.itemAtIndex(24)
        const viewedRow = rows[24]
        verify(viewed)
        const offset = viewed.mapToItem(list, 0, 0).y
        verify(offset < 0 && offset > -viewed.height)
        verify(rowMoves.valid)
        rowMoves.clear()

        function checkViewport() {
            const selectedIndex = rows.findIndex(r => r.id === selected)
            compare(list.currentIndex, selectedIndex)
            compare(list.currentItem.taskId, selected)
            compare(list.currentItem.highlighted, true)
            compare(list.itemAtIndex(rows.indexOf(viewedRow)), viewed)
            fuzzyCompare(viewed.mapToItem(list, 0, 0).y, offset, 1)
            compare(rowMoves.count, 0)
        }

        viewedRow.state = "running"
        viewedRow.done = 23
        viewedRow.total = 90
        rows[3].state = "paused"
        publish(rows)
        checkViewport()
        compare(viewed.taskState, "running")
        compare(viewed.done, 23)
        compare(page.state, "paused")

        rows.unshift(row("new-task", "queued", ["cancel"]))
        publish(rows)
        checkViewport()
        rows.splice(1, 2)
        publish(rows)
        checkViewport()

        // Removing the selection can select its successor at the same numeric index.
        const index = list.currentIndex
        rows.splice(index, 1)
        publish(rows, null, rows[index].id)
        compare(list.currentIndex, index)
        compare(list.currentItem.taskId, rows[index].id)
        compare(list.currentItem.highlighted, true)
        const y = list.currentItem.mapToItem(list, 0, 0).y
        verify(y >= -1 && y + list.currentItem.height <= list.height + 1)
    }

    function test_liveActionsFollowUuid() {
        setTarget("running", ["pause", "cancel", "redo"])
        openTarget()
        publish([row(target, "paused", ["resume", "redo"]), selectedRow()])
        tryCompare(actionItem("resume"), "visible", true)
        compare(actionItem("pause").visible, false)
        compare(actionItem("cancel").visible, false)
        clickAction("resume")
        compare(backendFixture.commands, ["resume:" + target])
    }

    function test_taskRemovedClosesMenu() {
        setTarget("completed", ["redo", "delete"])
        openTarget()
        publish([selectedRow()])
        tryCompare(menu, "visible", false)
        compare(backendFixture.commands.length, 0)
    }

    function test_targetedCommands_data() {
        return [{ tag: "pause" }, { tag: "redo" }, { tag: "openOutput" }]
    }

    function test_targetedCommands(data) {
        setTarget("running", [data.tag])
        openTarget()
        clickAction(data.tag)
        compare(backendFixture.commands, [data.tag + ":" + target])
    }

    function test_confirmOperation_data() {
        return [{ tag: "delete" }, { tag: "cancel" }]
    }

    function test_confirmOperation(data) {
        setTarget("paused", ["resume", "cancel", "redo", "openOutput", "delete"])
        const prefix = data.tag === "delete" ? "Delete" : "Cancel"
        for (const accept of [false, true]) {
            openTarget()
            clickAction(data.tag)
            const dialog = findChild(page, data.tag + "TaskConfirmDialog")
            verify(dialog)
            tryCompare(dialog, "opened", true)
            compare(backendFixture.commands.length, 0)
            const button = findChild(dialog, (accept ? "confirm" : "dismiss") + prefix + "TaskButton")
            verify(button)
            mouseClick(button)
            tryCompare(dialog, "visible", false)
            compare(backendFixture.commands, accept ? [data.tag + ":" + target] : [])
        }
    }

    function test_staleDeleteConfirmation() {
        setTarget("completed", ["delete"])
        openTarget()
        clickAction("delete")
        const dialog = findChild(page, "deleteTaskConfirmDialog")
        tryCompare(dialog, "opened", true)
        publish([selectedRow()])
        tryCompare(dialog, "visible", false)
        compare(backendFixture.commands.length, 0)
    }

    function test_detailFollowsPublishedActions() {
        publish([row(selected, "interrupted", ["resume", "redo", "delete"])])
        verify(findChild(page, "resumeButton").visible)
        verify(!findChild(page, "cancelButton").visible)
        verify(!findChild(page, "pauseButton").visible)
    }

    function test_errorFeedback() {
        const reason = "open <missing> directory"
        publish([selectedRow()], { code: "outputDirectoryLookupFailed", args: [], detail: reason })
        const error = findChild(page, "taskActionError")
        verify(error.visible)
        compare(findChild(error, "taskActionErrorSummary").text, qsTranslate("GoosyUi", "Could not locate the output directory"))
        const detail = findChild(page, "taskActionErrorDetail")
        verify(detail.visible)
        compare(detail.textFormat, Text.PlainText)
        compare(detail.text, reason)
    }

    function test_taskCounts() {
        const summary = findChild(page, "taskCountSummary")
        publish([])
        compare(summary.text, qsTranslate("Tasks", "%1 tasks").arg(0))
        publish([selectedRow()])
        compare(summary.text, qsTranslate("Tasks", "1 task"))
        setTarget("interrupted", ["resume"])
        compare(summary.text, qsTranslate("Tasks", "%1 tasks").arg(2) + " · " + qsTranslate("Tasks", "%1 interrupted").arg(1))
    }

    function test_popupAtWindowEdge_data() {
        return [{ tag: "compact-bottom-right", w: 680, h: 420, right: true, bottom: true },
                { tag: "wide-top-right", w: 960, h: 600, right: true, bottom: false },
                { tag: "compact-bottom-left", w: 680, h: 420, right: false, bottom: true },
                { tag: "completed", w: 960, h: 600, right: true, bottom: true, completed: true }]
    }

    function test_popupAtWindowEdge(data) {
        const window = page.Window.window
        const originalSize = Qt.size(window.width, window.height)
        window.width = data.w
        window.height = data.h
        try {
            tryCompare(menu.parent, "width", data.w)
            tryCompare(menu.parent, "height", data.h)
            const actions = data.completed ? ["redo", "openOutput", "delete"]
                                           : ["pause", "cancel", "redo", "openOutput", "delete"]
            setTarget(data.completed ? "completed" : "running", actions)
            tryVerify(() => list.itemAtIndex(1) !== null)
            const card = list.itemAtIndex(1)
            const center = card.mapToItem(menu.parent, card.width / 2, card.height / 2)
            page.x += (data.right ? data.w - 12 : 12) - center.x
            page.y += (data.bottom ? data.h - 12 : 12) - center.y
            openTarget()
            verify(menu.width > 0 && menu.height > 0)
            const widest = Math.max(...actions.map(op => actionItem(op).implicitWidth))
            verify(menu.width >= widest, "The longest action must fit")
            verify(menu.width <= Math.ceil(widest + menu.leftPadding + menu.rightPadding),
                   "The popup should fit its actions without unused horizontal space")
            const frame = menu.background
            const topRow = actionItem(actions[0]).background
            const bottomRow = actionItem("delete").background
            const topGap = topRow.mapToItem(frame, 0, 0).y
            verify(topGap > 0)
            fuzzyCompare(frame.height - bottomRow.mapToItem(frame, 0, bottomRow.height).y, topGap, 0.5)
            for (const op of actions) {
                const highlight = actionItem(op).background
                const origin = highlight.mapToItem(frame, 0, 0)
                fuzzyCompare(origin.x, topGap, 0.5)
                fuzzyCompare(frame.width - origin.x - highlight.width, topGap, 0.5,
                             `${op}: frame=${frame.width}, menu=${menu.width}, viewport=${menu.contentItem.width}, column=${highlight.parent.parent.width}, row=${highlight.parent.width}, padding=${menu.leftPadding}/${menu.rightPadding}`)
            }
            verify(menu.x >= 0 && menu.x + menu.width <= data.w)
            verify(menu.y >= 0 && menu.y + menu.height <= data.h)
            clickAction("openOutput")
            compare(backendFixture.commands, ["openOutput:" + target])
        } finally {
            window.width = originalSize.width
            window.height = originalSize.height
        }
    }
}
