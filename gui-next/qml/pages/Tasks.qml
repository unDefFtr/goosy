import QtQuick
import QtQuick.Controls as Controls
import QtQuick.Layouts
import RinUI
import "../components"
import "../js/ui.js" as Ui

Controls.Page {
    id: page
    objectName: "tasksPage"
    padding: 24
    background: Item {}
    property var backend: Backend
    property var task: JSON.parse(backend.taskJson || "{}")
    readonly property var tasks: page.task.tasks || []
    readonly property string state: String(page.task.state || "idle")
    readonly property real displayedFrames: Ui.displayedFrames(page.task)
    readonly property var selectedActions: page.task.actions || []
    readonly property bool recoverable: page.state === "paused" || page.state === "interrupted"
    readonly property string selectedId: String(page.task.selectedId || "")
    onSelectedIdChanged: {
        detailScroll.contentY = 0;
        taskLogs.expanded = false;
        logFlick.followLatest = true;
        taskPreview.mode = 0;
    }

    ListModel {
        id: taskRows
    }

    function syncTaskRows() {
        if (!queue || !taskRows) return;
        var snapshot = page.task.tasks || [];
        var selectedIndex = -1;
        Ui.reconcileTaskRows(taskRows, snapshot, function(task, index) {
            if (task.id === page.task.selectedId) selectedIndex = index;
            return {
                taskId: String(task.id),
                title: String(task.title || ""),
                taskState: String(task.state),
                done: Ui.displayedFrames(task),
                total: Number(task.total) || 0,
                output: String(task.output || ""),
                detail: String(task.detail || "")
            };
        });
        const selection = selectedIndex >= 0 ? snapshot[selectedIndex].id : "";
        if (queue.syncedSelection !== selection) {
            queue.syncedSelection = selection;
            queue.forceLayout();
            queue.currentIndex = selectedIndex;
            if (selectedIndex >= 0)
                queue.positionViewAtIndex(selectedIndex, ListView.Contain);
        } else if (queue.currentIndex !== selectedIndex) {
            // Qt batches structural changes. Let it shift the current row first.
            queue.forceLayout();
            if (queue.currentIndex !== selectedIndex) {
                const viewportY = queue.contentY;
                queue.currentIndex = selectedIndex;
                queue.contentY = viewportY;
            }
        }
    }

    function rowForId(id) {
        return id ? page.tasks.find(row => row.id === id) : null;
    }

    function requestAction(action, id) {
        const row = rowForId(id);
        if (!row || row.actions.indexOf(action) < 0) return;
        if (action === "delete" || action === "cancel") {
            confirmDialog.targetId = id;
            confirmDialog.operation = action;
            confirmDialog.open();
        } else {
            page.backend.command(action + ":" + id);
        }
    }

    function stateCount(states) {
        return page.tasks.filter(function (task) {
            return states.indexOf(task.state) >= 0;
        }).length;
    }

    onTaskChanged: syncTaskRows()
    Component.onCompleted: syncTaskRows()

    ColumnLayout {
        anchors.fill: parent
        spacing: 16
        RowLayout {
            Layout.fillWidth: true
            Text {
                text: qsTr("Tasks")
                typography: Typography.Title
            }
            Item {
                Layout.fillWidth: true
            }
            Text {
                text: qsTr("Concurrency limit: %1").arg(page.task.maxConcurrent || 1)
                color: Theme.currentTheme.colors.textSecondaryColor
            }
        }
        InfoBar {
            objectName: "taskActionError"
            Layout.fillWidth: true
            visible: !!page.task.error
            Text {
                objectName: "taskActionErrorSummary"
                width: Math.max(0, parent.parent.width)
                text: Ui.messageText(page.task.error)
                textFormat: Text.PlainText
                wrapMode: Text.WrapAnywhere
            }
            severity: Severity.Error
            closable: false
        }
        Text {
            objectName: "taskActionErrorDetail"
            Layout.fillWidth: true
            visible: text.length > 0
            text: page.task.error ? page.task.error.detail || "" : ""
            textFormat: Text.PlainText
            typography: Typography.Caption
            wrapMode: Text.WrapAnywhere
        }
        RowLayout {
            Layout.fillWidth: true
            Layout.fillHeight: true
            spacing: 20

            ColumnLayout {
                id: master
                objectName: "taskMaster"
                Layout.preferredWidth: page.availableWidth * 0.30
                Layout.minimumWidth: 280
                Layout.maximumWidth: page.availableWidth * 0.32
                Layout.fillHeight: true
                spacing: 10

                Text {
                    objectName: "taskCountSummary"
                    Layout.fillWidth: true
                    text: {
                        var parts = [page.tasks.length === 1 ? qsTr("1 task") : qsTr("%1 tasks").arg(page.tasks.length)];
                        var running = page.stateCount(["starting", "running"]);
                        var paused = page.stateCount(["paused"]);
                        var queued = page.stateCount(["queued"]);
                        var interrupted = page.stateCount(["interrupted"]);
                        if (running)
                            parts.push(qsTr("%1 running").arg(running));
                        if (paused)
                            parts.push(qsTr("%1 paused").arg(paused));
                        if (interrupted)
                            parts.push(qsTr("%1 interrupted").arg(interrupted));
                        if (queued)
                            parts.push(qsTr("%1 queued").arg(queued));
                        return parts.join(" · ");
                    }
                    typography: Typography.Body
                    color: Theme.currentTheme.colors.textSecondaryColor
                    elide: Text.ElideRight
                }

                ListView {
                    id: queue
                    objectName: "taskList"
                    Layout.fillWidth: true
                    Layout.fillHeight: true
                    clip: true
                    boundsBehavior: Flickable.StopAtBounds
                    model: taskRows
                    currentIndex: -1
                    // ListView tracks the current delegate through inserts and removals.
                    property string syncedSelection: ""
                    onItemClicked: function (index) {
                        if (index >= 0 && index < taskRows.count)
                            page.backend.command("select:" + taskRows.get(index).taskId);
                    }
                    Keys.onUpPressed: function(event) {
                        keyboardNavigation = true
                        if (currentIndex > 0) page.backend.command("select:" + taskRows.get(currentIndex - 1).taskId)
                        event.accepted = true
                    }
                    Keys.onDownPressed: function(event) {
                        keyboardNavigation = true
                        if (currentIndex + 1 < taskRows.count) page.backend.command("select:" + taskRows.get(currentIndex + 1).taskId)
                        event.accepted = true
                    }
                    delegate: ListViewDelegate {
                        id: taskCard
                        required property string taskId
                        required property string title
                        required property string taskState
                        required property real done
                        required property real total
                        required property string output
                        required property string detail
                        required property int index
                        readonly property bool isCompleted: taskState === "completed" || taskState === "done"
                        readonly property bool isFailed: taskState === "failed"
                        readonly property real percentWidth: Math.ceil(Math.max(percentMetrics.width, percentMetrics.advanceWidth))
                        width: queue.width
                        topPadding: 10
                        bottomPadding: 10
                        leftPadding: 16
                        rightPadding: 12
                        height: contentItem.implicitHeight + topPadding + bottomPadding
                        highlighted: taskId === String(page.task.selectedId || "")
                        keyboardNavigation: queue.keyboardNavigation && highlighted
                        focusPolicy: Qt.StrongFocus
                        Accessible.name: qsTr("%1, %2").arg(Ui.taskTitle(title)).arg(Ui.stateLabel(taskState))

                        TapHandler {
                            acceptedButtons: Qt.RightButton
                            onTapped: function(eventPoint) {
                                taskMenu.openFor(taskCard, eventPoint.position);
                            }
                        }

                        TextMetrics {
                            id: percentMetrics
                            font: percentLabel.font
                            text: Ui.progressPercent(1, 1)
                        }

                        ToolTip {
                            id: stateTip
                            visible: taskCard.hovered && !pathTip.visible
                            text: Ui.stateLabel(taskCard.taskState)
                        }

                        contentItem: ColumnLayout {
                            spacing: 6

                            RowLayout {
                                Layout.fillWidth: true
                                spacing: 6

                                Icon {
                                    size: 16
                                    name: Ui.stateIcon(taskCard.taskState)
                                    color: Ui.stateColor(taskCard.taskState, Theme.currentTheme.colors)
                                    Layout.alignment: Qt.AlignVCenter
                                }

                                Text {
                                    Layout.fillWidth: true
                                    Layout.alignment: Qt.AlignVCenter
                                    text: Ui.taskTitle(taskCard.title)
                                    textFormat: Text.PlainText
                                    typography: Typography.BodyStrong
                                    elide: Text.ElideRight
                                    maximumLineCount: 1
                                    wrapMode: Text.NoWrap
                                    verticalAlignment: Text.AlignVCenter
                                }

                                Text {
                                    id: percentLabel
                                    Layout.alignment: Qt.AlignVCenter
                                    Layout.preferredWidth: taskCard.percentWidth
                                    Layout.minimumWidth: taskCard.percentWidth
                                    Layout.maximumWidth: taskCard.percentWidth
                                    text: Ui.progressPercent(taskCard.done, taskCard.total)
                                    typography: Typography.Caption
                                    color: Theme.currentTheme.colors.textSecondaryColor
                                    horizontalAlignment: Text.AlignRight
                                    verticalAlignment: Text.AlignVCenter
                                    maximumLineCount: 1
                                    wrapMode: Text.NoWrap
                                }
                            }

                            ProgressBar {
                                Layout.fillWidth: true
                                from: 0
                                to: 1
                                value: Ui.progressValue(taskCard.done, taskCard.total)
                                state: taskCard.isFailed ? ProgressBar.Error : ["paused", "interrupted"].indexOf(taskCard.taskState) >= 0 ? ProgressBar.Paused : ProgressBar.Running
                                opacity: taskCard.isCompleted ? 0 : 1
                            }

                            Text {
                                Layout.fillWidth: true
                                text: taskCard.isFailed
                                    ? (Ui.taskSummary(taskCard.taskState, taskCard.detail) || Ui.stateLabel(taskCard.taskState))
                                    : Ui.stateLabel(taskCard.taskState) + " · " + taskCard.output.split(/[\\/]/).pop()
                                textFormat: Text.PlainText
                                typography: Typography.Caption
                                color: taskCard.isFailed
                                    ? Theme.currentTheme.colors.systemCriticalColor
                                    : Theme.currentTheme.colors.textSecondaryColor
                                elide: taskCard.isFailed ? Text.ElideRight : Text.ElideMiddle
                                maximumLineCount: 1
                                wrapMode: Text.NoWrap
                                HoverHandler {
                                    id: outputHover
                                }
                                ToolTip {
                                    id: pathTip
                                    objectName: "taskListPathTip"
                                    visible: outputHover.hovered && pathTip.text.length > 0
                                    text: taskCard.isFailed ? Ui.taskSummary(taskCard.taskState, taskCard.detail) : taskCard.output
                                }
                            }
                        }
                    }
                    Text {
                        anchors.centerIn: parent
                        visible: queue.count === 0
                        text: qsTr("No tasks yet")
                        color: Theme.currentTheme.colors.textSecondaryColor
                    }
                }
            }

            Rectangle {
                Layout.fillHeight: true
                implicitWidth: 1
                color: Theme.currentTheme.colors.dividerBorderColor
            }

            Flickable {
                id: detailScroll
                objectName: "taskDetailScroll"
                Layout.fillWidth: true
                Layout.minimumWidth: 500
                Layout.fillHeight: true
                visible: !!page.task.id
                contentWidth: width
                contentHeight: detail.implicitHeight
                boundsBehavior: Flickable.StopAtBounds
                clip: true
                Controls.ScrollBar.vertical: ScrollBar {}
                ColumnLayout {
                    id: detail
                    objectName: "taskDetail"
                    width: detailScroll.width - 14
                    spacing: 10

                    RowLayout {
                        Layout.fillWidth: true
                        Text {
                            Layout.fillWidth: true
                            text: page.task.id ? Ui.taskTitle(page.task.title) : ""
                            textFormat: Text.PlainText
                            typography: Typography.Subtitle
                            elide: Text.ElideRight
                        }
                        Button {
                            objectName: "pauseButton"
                            text: qsTr("Pause")
                            icon.name: "ic_fluent_pause_20_regular"
                            highlighted: true
                            enabled: page.selectedActions.indexOf("pause") >= 0
                            visible: enabled || page.task.pending === "pause"
                            onClicked: page.requestAction("pause", page.selectedId)
                        }
                        Button {
                            objectName: "resumeButton"
                            text: page.state === "interrupted" ? qsTr("Resume rendering") : qsTr("Resume")
                            icon.name: "ic_fluent_play_20_regular"
                            highlighted: true
                            enabled: page.selectedActions.indexOf("resume") >= 0
                            visible: enabled || page.task.pending === "resume"
                            onClicked: page.requestAction("resume", page.selectedId)
                        }
                        Button {
                            objectName: "cancelButton"
                            text: qsTr("Cancel")
                            icon.name: "ic_fluent_dismiss_20_regular"
                            enabled: page.selectedActions.indexOf("cancel") >= 0
                            visible: enabled || page.task.pending === "cancel"
                            onClicked: page.requestAction("cancel", page.selectedId)
                        }
                    }

                    RowLayout {
                        Layout.fillWidth: true
                        spacing: 12
                        Text {
                            text: Ui.stateLabel(page.state)
                            color: Ui.stateColor(page.state, Theme.currentTheme.colors)
                            typography: Typography.BodyStrong
                        }
                        Text {
                            visible: !!page.task.pending
                            text: ({
                                    pause: qsTr("Pausing…"),
                                    resume: qsTr("Resuming…"),
                                    cancel: qsTr("Cancelling…")
                                })[page.task.pending] || ""
                            typography: Typography.Caption
                            color: Theme.currentTheme.colors.textSecondaryColor
                        }
                        Item {
                            Layout.fillWidth: true
                        }
                        Text {
                            visible: Number(page.task.total) > 0
                            text: Ui.formatCount(page.displayedFrames, page.task.total) + " · " + Ui.progressPercent(page.displayedFrames, page.task.total) + " · " + Ui.formatFps(page.task.fps)
                            typography: Typography.Body
                        }
                        Text {
                            text: qsTr("Elapsed: %1").arg(Ui.formatElapsed(page.task.elapsed))
                            typography: Typography.Caption
                            color: Theme.currentTheme.colors.textSecondaryColor
                        }
                    }

                    ProgressBar {
                        Layout.fillWidth: true
                        visible: Number(page.task.total) > 0
                        from: 0
                        to: 1
                        value: Ui.progressValue(page.displayedFrames, page.task.total)
                        state: page.state === "failed" ? ProgressBar.Error : page.recoverable ? ProgressBar.Paused : ProgressBar.Running
                    }

                    Text {
                        Layout.fillWidth: true
                        visible: !!page.task.detail
                        text: Ui.taskSummary(page.state, page.task.detail)
                        color: page.state === "failed" ? Theme.currentTheme.colors.systemCriticalColor : Theme.currentTheme.colors.textSecondaryColor
                        elide: Text.ElideRight
                        maximumLineCount: 1
                    }

                    ColumnLayout {
                        Layout.fillWidth: true
                        spacing: 4
                        Text {
                            id: outputName
                            objectName: "taskOutputFilename"
                            Layout.fillWidth: true
                            text: String(page.task.output || "").split(/[\\/]/).pop()
                            textFormat: Text.PlainText
                            typography: Typography.Body
                            elide: Text.ElideMiddle
                            maximumLineCount: 1
                            wrapMode: Text.NoWrap
                            HoverHandler { id: filenameHover }
                            ToolTip {
                                objectName: "taskOutputPathTip"
                                visible: filenameHover.hovered
                                text: page.task.output || ""
                            }
                        }
                        Flow {
                            Layout.fillWidth: true
                            spacing: 12
                            Text {
                                text: qsTr("%1 × %2 · %3 fps").arg((page.task.config || {}).width || "—")
                                    .arg((page.task.config || {}).height || "—").arg((page.task.config || {}).fps || "—")
                                typography: Typography.Caption
                                color: Theme.currentTheme.colors.textSecondaryColor
                            }
                            Text {
                                text: Ui.encoderLabel((page.task.config || {}).encoder || "")
                                typography: Typography.Caption
                                color: Theme.currentTheme.colors.textSecondaryColor
                            }
                            Text {
                                text: (page.task.config || {}).codec || ""
                                typography: Typography.Caption
                                color: Theme.currentTheme.colors.textSecondaryColor
                            }
                            Text {
                                visible: Number(page.task.outputSize) > 0
                                text: qsTr("File size: %1").arg(Ui.formatBytes(page.task.outputSize))
                                typography: Typography.Caption
                                color: Theme.currentTheme.colors.textSecondaryColor
                            }
                        }
                        Text {
                            Layout.fillWidth: true
                            text: qsTr("Created %1 · Updated %2").arg(Ui.formatTimestamp(page.task.createdAt)).arg(Ui.formatTimestamp(page.task.updatedAt))
                            typography: Typography.Caption
                            wrapMode: Text.WordWrap
                            color: Theme.currentTheme.colors.textSecondaryColor
                        }
                    }

                    TaskPreview {
                        id: taskPreview
                        objectName: "selectedTaskPreview"
                        Layout.fillWidth: true
                        maximumImageHeight: Math.max(180, detailScroll.height - 265)
                        task: page.task
                        renderSource: page.backend.renderSource
                        styleSource: page.backend.styleSource
                        styleError: page.task.styleError || null
                    }

                    Expander {
                        Layout.fillWidth: true
                        implicitWidth: 0
                        text: qsTr("Files and error details")
                        expanded: false
                        Text {
                            Layout.fillWidth: true
                            text: qsTr("Task ID: %1\nOutput: %2\nAudio: %3").arg(page.task.id || "")
                                .arg(page.task.output || "").arg((page.task.config || {}).audio || "")
                                + (page.task.detail ? "\n" + page.task.detail : "")
                            wrapMode: Text.WrapAnywhere
                            textFormat: Text.PlainText
                            typography: Typography.Caption
                        }
                    }

                    Expander {
                        id: taskOptions
                        Layout.fillWidth: true
                        implicitWidth: 0
                        text: qsTr("Render configuration")
                        expanded: false
                        Text {
                            Layout.fillWidth: true
                            text: taskOptions.expanded && page.task.options ? JSON.stringify(page.task.options, null, 2) : ""
                            textFormat: Text.PlainText
                            wrapMode: Text.WrapAnywhere
                            typography: Typography.Caption
                        }
                    }

                    Expander {
                        id: taskLogs
                        objectName: "taskLogs"
                        Layout.fillWidth: true
                        implicitWidth: 0
                        text: qsTr("Log")
                        onExpandedChanged: {
                            if (expanded) {
                                logFlick.followLatest = true;
                                Qt.callLater(function () {
                                    logFlick.contentY = Math.max(0, logFlick.contentHeight - logFlick.height);
                                });
                            }
                        }
                        Flickable {
                            id: logFlick
                            objectName: "taskLogScroll"
                            Layout.fillWidth: true
                            Layout.preferredHeight: 120
                            contentWidth: width
                            contentHeight: logText.implicitHeight
                            clip: true
                            boundsBehavior: Flickable.StopAtBounds
                            property bool followLatest: true
                            onMovementEnded: followLatest = atYEnd
                            onContentYChanged: if (moving)
                                followLatest = atYEnd
                            onContentHeightChanged: if (followLatest)
                                contentY = Math.max(0, contentHeight - height)
                            Controls.ScrollBar.vertical: ScrollBar {}
                            Text {
                                id: logText
                                objectName: "selectedTaskLogText"
                                width: logFlick.width - 12
                                text: String(page.task.log || qsTr("No log entries yet"))
                                textFormat: Text.PlainText
                                wrapMode: Text.WrapAnywhere
                                typography: Typography.Caption
                                color: Theme.currentTheme.colors.textSecondaryColor
                            }
                        }
                    }
                }
            }

            Text {
                Layout.fillWidth: true
                Layout.fillHeight: true
                visible: !page.task.id
                text: qsTr("Select a task to view its details")
                color: Theme.currentTheme.colors.textSecondaryColor
                horizontalAlignment: Text.AlignHCenter
                verticalAlignment: Text.AlignVCenter
            }
        }
    }

    Menu {
        id: taskMenu
        objectName: "taskContextMenu"
        parent: page.Controls.Overlay.overlay
        popupType: Controls.Popup.Item
        position: Position.None
        margins: 4
        leftPadding: 0
        rightPadding: 0
        property string targetId: ""
        readonly property var targetRow: page.rowForId(targetId)
        readonly property var allowed: targetRow ? targetRow.actions : []
        property point pointer
        property rect screenBounds
        readonly property real rightEdge: Math.min(parent ? parent.width : 0, screenBounds.x + screenBounds.width) - margins
        readonly property real bottomEdge: Math.min(parent ? parent.height : 0, screenBounds.y + screenBounds.height) - margins
        width: Math.min(implicitWidth, Math.max(0, rightEdge - Math.max(0, screenBounds.x) - margins))
        height: Math.min(implicitHeight, Math.max(0, bottomEdge - Math.max(0, screenBounds.y) - margins))
        x: Math.max(margins + Math.max(0, screenBounds.x), Math.min(pointer.x, rightEdge - width))
        y: Math.max(margins + Math.max(0, screenBounds.y), Math.min(pointer.y, bottomEdge - height))
        // RinUI's default enter animation repositions anchored drop-down menus.
        enter: Transition { NumberAnimation { property: "opacity"; from: 0; to: 1; duration: Utils.appearanceSpeed } }
        onAllowedChanged: if (visible && !allowed.length) close()
        onClosed: targetId = ""

        function openFor(card, point) {
            targetId = card.taskId;
            if (!parent || !allowed.length) return;
            pointer = card.mapToItem(parent, point.x, point.y);
            const globalPoint = card.mapToGlobal(point.x, point.y);
            const screen = Qt.application.screens.find(s => globalPoint.x >= s.virtualX && globalPoint.x < s.virtualX + s.width
                && globalPoint.y >= s.virtualY && globalPoint.y < s.virtualY + s.height);
            if (screen) {
                const origin = parent.mapFromGlobal(screen.virtualX, screen.virtualY);
                screenBounds = Qt.rect(origin.x, origin.y, screen.width, screen.height);
            } else {
                screenBounds = Qt.rect(0, 0, parent.width, parent.height);
            }
            open();
        }

        component TaskAction: MenuItem {
            required property string operation
            // Center the RinUI menu column, which is laid out with x: -5.
            transform: Translate { x: 5 }
            objectName: "taskMenu_" + operation
            visible: taskMenu.allowed.indexOf(operation) >= 0
            enabled: visible
            onTriggered: page.requestAction(operation, taskMenu.targetId)
        }
        TaskAction { operation: "pause"; text: qsTr("Pause"); icon.name: "ic_fluent_pause_20_regular" }
        TaskAction { operation: "resume"; text: qsTr("Resume"); icon.name: "ic_fluent_play_20_regular" }
        TaskAction { operation: "cancel"; text: qsTr("Cancel"); icon.name: "ic_fluent_dismiss_20_regular" }
        TaskAction { operation: "redo"; text: qsTr("Redo"); icon.name: "ic_fluent_arrow_repeat_all_20_regular" }
        TaskAction { operation: "openOutput"; text: qsTr("Open output folder"); icon.name: "ic_fluent_folder_open_20_regular" }
        TaskAction { operation: "delete"; text: qsTr("Delete"); icon.name: "ic_fluent_delete_20_regular" }
    }

    Dialog {
        id: confirmDialog
        readonly property bool deleting: operation === "delete"
        objectName: deleting ? "deleteTaskConfirmDialog" : "cancelTaskConfirmDialog"
        property string targetId: ""
        property string operation: ""
        readonly property var targetRow: page.rowForId(targetId)
        readonly property bool allowed: !!targetRow && (targetRow.actions || []).indexOf(operation) >= 0
        title: deleting ? qsTr("Delete task") : qsTr("Cancel task")
        modal: true
        closePolicy: Controls.Popup.CloseOnEscape
        onOpened: dismissCancelButton.forceActiveFocus()
        onAllowedChanged: if (visible && !allowed) close()
        onAccepted: if (allowed) page.backend.command(operation + ":" + targetId)
        onClosed: targetId = ""

        Text {
            Layout.fillWidth: true
            text: (confirmDialog.deleting
                ? qsTr("Delete “%1”? This removes the task history, thumbnail, checkpoints, video segments, and temporary files. Your exported video and source files are kept. If the task is running, cancel it and wait for rendering to stop first.")
                : qsTr("Cancel “%1”? Rendering cannot be resumed after cancellation. The task history will be kept."))
                .arg(confirmDialog.targetRow ? Ui.taskTitle(confirmDialog.targetRow.title) : "")
            textFormat: Text.PlainText
            typography: Typography.Body
            color: Theme.currentTheme.colors.textColor
            wrapMode: Text.WordWrap
        }

        footer: DialogButtonBox {
            id: cancelDialogFooter
            Button {
                id: confirmCancelButton
                objectName: confirmDialog.deleting ? "confirmDeleteTaskButton" : "confirmCancelTaskButton"
                Layout.fillWidth: true
                Layout.preferredWidth: 1
                text: confirmDialog.title
                highlighted: true
                primaryColor: Theme.currentTheme.colors.systemCriticalColor
                focusPolicy: Qt.StrongFocus
                onClicked: confirmDialog.accept()
                Keys.onReturnPressed: confirmCancelButton.click()
                Keys.onEnterPressed: confirmCancelButton.click()
            }
            Button {
                id: dismissCancelButton
                objectName: confirmDialog.deleting ? "dismissDeleteTaskButton" : "dismissCancelTaskButton"
                Layout.fillWidth: true
                Layout.preferredWidth: 1
                text: qsTr("Go back")
                focusPolicy: Qt.StrongFocus
                onClicked: confirmDialog.reject()
                Keys.onReturnPressed: dismissCancelButton.click()
                Keys.onEnterPressed: dismissCancelButton.click()
            }
        }
    }
}
