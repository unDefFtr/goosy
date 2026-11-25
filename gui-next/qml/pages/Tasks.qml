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
    property var task: JSON.parse(Backend.taskJson || "{}")
    readonly property var tasks: page.task.tasks || []
    readonly property string state: String(page.task.state || "idle")
    readonly property bool canPause: page.state === "running" && !page.task.pending
    readonly property bool canResume: page.state === "paused" && !page.task.pending
    readonly property bool canCancel: ["queued", "starting", "running", "paused"].indexOf(page.state) >= 0 && page.task.pending !== "cancel"
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

    function indexFor(id) {
        for (var i = 0; i < taskRows.count; ++i)
            if (taskRows.get(i).taskId === String(id))
                return i;
        return -1;
    }

    function syncTaskRows() {
        var snapshot = page.task.tasks || [];
        for (var i = 0; i < snapshot.length; ++i) {
            var task = snapshot[i];
            var values = {
                taskId: String(task.id),
                title: String(task.title || ""),
                taskState: String(task.state),
                done: Number(task.done) || 0,
                total: Number(task.total) || 0,
                elapsed: Number(task.elapsed) || 0,
                output: String(task.output || ""),
                detail: String(task.detail || "")
            };
            var rowIndex = indexFor(values.taskId);
            if (rowIndex < 0) {
                taskRows.append(values);
                continue;
            }
            for (var key in values)
                if (taskRows.get(rowIndex)[key] !== values[key])
                    taskRows.setProperty(rowIndex, key, values[key]);
        }
        for (var index = taskRows.count - 1; index >= 0; --index) {
            if (!snapshot.some(function (task) {
                return String(task.id) === taskRows.get(index).taskId;
            }))
                taskRows.remove(index);
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
                text: qsTr("并发上限 %1").arg(page.task.maxConcurrent || 1)
                color: Theme.currentTheme.colors.textSecondaryColor
            }
        }
        InfoBar {
            objectName: "taskActionError"
            Layout.fillWidth: true
            visible: !!page.task.error
            text: page.task.error || ""
            severity: Severity.Error
            closable: false
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
                    Layout.fillWidth: true
                    text: {
                        var parts = [qsTr("%1 个任务").arg(page.tasks.length)];
                        var running = page.stateCount(["starting", "running"]);
                        var paused = page.stateCount(["paused"]);
                        var queued = page.stateCount(["queued"]);
                        if (running)
                            parts.push(qsTr("%1 运行中").arg(running));
                        if (paused)
                            parts.push(qsTr("%1 已暂停").arg(paused));
                        if (queued)
                            parts.push(qsTr("%1 排队中").arg(queued));
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
                    currentIndex: {
                        for (var i = 0; i < taskRows.count; ++i)
                            if (taskRows.get(i).taskId === String(page.task.selectedId || ""))
                                return i;
                        return -1;
                    }
                    onCurrentIndexChanged: {
                        if (currentIndex >= 0)
                            positionViewAtIndex(currentIndex, ListView.Contain);
                    }
                    onItemClicked: function (index) {
                        if (index >= 0 && index < taskRows.count)
                            Backend.command("select:" + taskRows.get(index).taskId);
                    }
                    Keys.onUpPressed: function(event) {
                        keyboardNavigation = true
                        if (currentIndex > 0) Backend.command("select:" + taskRows.get(currentIndex - 1).taskId)
                        event.accepted = true
                    }
                    Keys.onDownPressed: function(event) {
                        keyboardNavigation = true
                        if (currentIndex + 1 < taskRows.count) Backend.command("select:" + taskRows.get(currentIndex + 1).taskId)
                        event.accepted = true
                    }
                    delegate: ListViewDelegate {
                        id: taskCard
                        required property string taskId
                        required property string title
                        required property string taskState
                        required property real done
                        required property real total
                        required property real elapsed
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
                        Accessible.name: title + "，" + Ui.stateLabel(taskState)

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
                                    text: taskCard.title
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
                                state: taskCard.isFailed ? ProgressBar.Error : taskCard.taskState === "paused" ? ProgressBar.Paused : ProgressBar.Running
                                opacity: taskCard.isCompleted ? 0 : 1
                            }

                            Text {
                                Layout.fillWidth: true
                                text: taskCard.isFailed
                                    ? (taskCard.detail ? taskCard.detail.replace(/[\r\n]+/g, " ").trim() : qsTr("渲染失败"))
                                    : taskCard.output.split(/[\\/]/).pop()
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
                                    text: taskCard.isFailed ? (taskCard.detail || taskCard.output) : taskCard.output
                                }
                            }
                        }
                    }
                    Text {
                        anchors.centerIn: parent
                        visible: queue.count === 0
                        text: qsTr("暂无任务")
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
                            text: page.task.title || (page.task.id ? qsTr("任务 #%1").arg(page.task.id) : "")
                            typography: Typography.Subtitle
                            elide: Text.ElideRight
                        }
                        Button {
                            objectName: "pauseButton"
                            text: qsTr("暂停")
                            icon.name: "ic_fluent_pause_20_regular"
                            highlighted: true
                            visible: page.state === "running"
                            enabled: page.canPause
                            onClicked: Backend.command("pause:" + page.task.id)
                        }
                        Button {
                            objectName: "resumeButton"
                            text: qsTr("继续")
                            icon.name: "ic_fluent_play_20_regular"
                            highlighted: true
                            visible: page.state === "paused"
                            enabled: page.canResume
                            onClicked: Backend.command("resume:" + page.task.id)
                        }
                        Button {
                            objectName: "cancelButton"
                            text: qsTr("取消")
                            icon.name: "ic_fluent_dismiss_20_regular"
                            visible: ["queued", "starting", "running", "paused"].indexOf(page.state) >= 0
                            enabled: page.canCancel
                            onClicked: {
                                cancelConfirmDialog.targetTaskId = String(page.task.id || "");
                                cancelConfirmDialog.open();
                            }
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
                                    pause: qsTr("正在暂停…"),
                                    resume: qsTr("正在继续…"),
                                    cancel: qsTr("正在取消…")
                                })[page.task.pending] || ""
                            typography: Typography.Caption
                            color: Theme.currentTheme.colors.textSecondaryColor
                        }
                        Item {
                            Layout.fillWidth: true
                        }
                        Text {
                            visible: Number(page.task.total) > 0
                            text: Ui.formatCount(page.task.done, page.task.total) + " · " + Ui.progressPercent(page.task.done, page.task.total) + " · " + Ui.formatFps(page.task.fps)
                            typography: Typography.Body
                        }
                        Text {
                            text: qsTr("已用 %1").arg(Ui.formatElapsed(page.task.elapsed))
                            typography: Typography.Caption
                            color: Theme.currentTheme.colors.textSecondaryColor
                        }
                    }

                    ProgressBar {
                        Layout.fillWidth: true
                        visible: Number(page.task.total) > 0
                        from: 0
                        to: 1
                        value: Ui.progressValue(page.task.done, page.task.total)
                        state: page.state === "failed" ? ProgressBar.Error : page.state === "paused" ? ProgressBar.Paused : ProgressBar.Running
                    }

                    Text {
                        Layout.fillWidth: true
                        visible: !!page.task.detail
                        text: page.task.detail || ""
                        color: page.state === "failed" ? Theme.currentTheme.colors.systemCriticalColor : Theme.currentTheme.colors.textSecondaryColor
                        elide: Text.ElideRight
                        maximumLineCount: 1
                        HoverHandler {
                            id: failureHover
                        }
                        ToolTip {
                            objectName: "taskFailureTip"
                            visible: failureHover.hovered
                            text: page.task.detail || ""
                        }
                    }

                    ColumnLayout {
                        Layout.fillWidth: true
                        spacing: 4
                        Text {
                            id: outputName
                            objectName: "taskOutputFilename"
                            Layout.fillWidth: true
                            text: String(page.task.output || "").split(/[\\/]/).pop()
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
                                text: (page.task.config || {}).encoder || ""
                                typography: Typography.Caption
                                color: Theme.currentTheme.colors.textSecondaryColor
                            }
                            Text {
                                text: (page.task.config || {}).codec || ""
                                typography: Typography.Caption
                                color: Theme.currentTheme.colors.textSecondaryColor
                            }
                        }
                    }

                    TaskPreview {
                        id: taskPreview
                        objectName: "selectedTaskPreview"
                        Layout.fillWidth: true
                        maximumImageHeight: Math.max(180, detailScroll.height - 265)
                        task: page.task
                        renderSource: Backend.renderSource
                        styleSource: Backend.styleSource
                        styleError: page.task.styleError || ""
                    }

                    Expander {
                        Layout.fillWidth: true
                        implicitWidth: 0
                        text: qsTr("文件与错误详情")
                        expanded: false
                        Text {
                            Layout.fillWidth: true
                            text: qsTr("输出：%1\n音频：%2").arg(page.task.output || "").arg((page.task.config || {}).audio || "") + (page.task.detail ? "\n" + page.task.detail : "")
                            wrapMode: Text.WrapAnywhere
                            textFormat: Text.PlainText
                            typography: Typography.Caption
                        }
                    }

                    Expander {
                        id: taskLogs
                        objectName: "taskLogs"
                        Layout.fillWidth: true
                        implicitWidth: 0
                        text: qsTr("日志")
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
                                text: String(page.task.log || qsTr("暂无日志"))
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
                text: qsTr("选择一个任务查看详情")
                color: Theme.currentTheme.colors.textSecondaryColor
                horizontalAlignment: Text.AlignHCenter
                verticalAlignment: Text.AlignVCenter
            }
        }
    }

    Dialog {
        id: cancelConfirmDialog
        objectName: "cancelTaskConfirmDialog"
        property string targetTaskId: ""
        title: qsTr("取消任务")
        modal: true
        closePolicy: Controls.Popup.CloseOnEscape
        onOpened: dismissCancelButton.forceActiveFocus()
        onAccepted: {
            if (page.canCancel && String(page.task.id || "") === cancelConfirmDialog.targetTaskId)
                Backend.command("cancel:" + cancelConfirmDialog.targetTaskId);
        }

        Text {
            Layout.fillWidth: true
            text: qsTr("确定取消该任务吗？已渲染的进度将被丢弃。")
            typography: Typography.Body
            color: Theme.currentTheme.colors.textColor
            wrapMode: Text.WordWrap
        }

        footer: DialogButtonBox {
            id: cancelDialogFooter
            Button {
                id: confirmCancelButton
                objectName: "confirmCancelTaskButton"
                Layout.fillWidth: true
                Layout.preferredWidth: 1
                text: qsTr("取消任务")
                highlighted: true
                primaryColor: Theme.currentTheme.colors.systemCriticalColor
                focusPolicy: Qt.StrongFocus
                onClicked: cancelConfirmDialog.accept()
                Keys.onReturnPressed: confirmCancelButton.click()
                Keys.onEnterPressed: confirmCancelButton.click()
            }
            Button {
                id: dismissCancelButton
                objectName: "dismissCancelTaskButton"
                Layout.fillWidth: true
                Layout.preferredWidth: 1
                text: qsTr("返回")
                focusPolicy: Qt.StrongFocus
                onClicked: cancelConfirmDialog.reject()
                Keys.onReturnPressed: dismissCancelButton.click()
                Keys.onEnterPressed: dismissCancelButton.click()
            }
        }

        Connections {
            target: page
            function onStateChanged() {
                if (cancelConfirmDialog.opened && !page.canCancel)
                    cancelConfirmDialog.close();
            }
            function onCanCancelChanged() {
                if (cancelConfirmDialog.opened && !page.canCancel)
                    cancelConfirmDialog.close();
            }
            function onSelectedIdChanged() {
                if (cancelConfirmDialog.opened && String(page.task.id || "") !== cancelConfirmDialog.targetTaskId)
                    cancelConfirmDialog.close();
            }
        }
    }
}
