import QtQuick
import QtQuick.Controls as Controls
import QtQuick.Layouts
import RinUI
import "qrc:/rinui/RinUI/animations" as RinAnimations
import "../components"
import "../js/ui.js" as Ui
import "../js/mock_recent_tasks.js" as MockRecentTasks

Controls.Page {
    id: page
    objectName: "homePage"
    title: qsTr("Home")
    background: Item {}
    property var project: JSON.parse(Backend.projectJson || "{}")
    property var task: JSON.parse(Backend.taskJson || "{}")
    readonly property var wizard: project.wizard || {}
    readonly property var cfg: project.settings || {}
    readonly property int step: wizard.step || 0
    readonly property double redoRevision: wizard.redoRevision || 0
    readonly property var titles: [qsTr("Media"), qsTr("Style"), qsTr("Output"), qsTr("Review")]
    readonly property bool navigating: stepTransition.running
    property int navigationDirection: 1
    property string navigationAction: ""
    property int navigationTarget: 0

    // Responsive breakpoint matching Fluent compact/medium threshold (content width < 720px stacks vertically).
    readonly property real narrowBreakpoint: 720
    readonly property bool isNarrow: page.width < narrowBreakpoint

    // Optional preview hooks (empty / -1 use live Backend data and system local hour).
    property string mockPreset: ""
    property int greetingHour: -1
    property int localHour: new Date().getHours()
    readonly property string greeting: Ui.greetingText(greetingHour >= 0 ? greetingHour : localHour)

    readonly property var recentSource: {
        var mock = MockRecentTasks.preset(page.mockPreset)
        if (mock !== null)
            return mock
        return page.task.recentTasks || []
    }

    ListModel {
        id: recentRows
    }

    function syncRecentRows() {
        if (!recentRows) return
        Ui.reconcileTaskRows(recentRows, page.recentSource || [], function(item) {
            var itemCfg = item.config || {}
            return {
                taskId: String(item.id || ""),
                title: String(item.title || ""),
                taskState: String(item.state || "idle"),
                done: Ui.displayedFrames(item),
                total: Number(item.total) || 0,
                outputWidth: Number(itemCfg.width) || 0,
                outputHeight: Number(itemCfg.height) || 0,
                outputFps: Number(itemCfg.fps) || 0,
                codec: String(itemCfg.codec || ""),
                thumbnail: String(item.thumbnail || ""),
                detail: String(item.detail || "")
            }
        })
    }

    function openTasksPage() {
        var win = Controls.ApplicationWindow.window
        if (win && win.navigationView) {
            win.navigationView.push(Qt.resolvedUrl("Tasks.qml"))
        }
    }

    function openTaskDetail(taskId) {
        if (taskId && !page.mockPreset) {
            Backend.command("select:" + taskId)
        }
        openTasksPage()
    }

    onRecentSourceChanged: syncRecentRows()
    Component.onCompleted: syncRecentRows()

    Timer {
        interval: 60000
        repeat: true
        running: true
        onTriggered: page.localHour = new Date().getHours()
    }

    onRedoRevisionChanged: {
        if (stepTransition && stepTransition.running) {
            stepTransition.stop()
            form.x = 0
            form.opacity = 1
        }
    }

    function navigate(target, action) {
        if (navigating || (target === step && !action)) return
        navigationDirection = target > step ? 1 : -1
        navigationTarget = target
        navigationAction = action || ""
        stepButtons.itemAt(step).forceActiveFocus(Qt.OtherFocusReason)
        stepTransition.start()
    }

    SequentialAnimation {
        id: stepTransition
        objectName: "wizardStepTransition"
        ParallelAnimation {
            NumberAnimation { target: form; property: "opacity"; to: 0; duration: Utils.appearanceSpeed; easing.type: Easing.InOutQuad }
            NumberAnimation {
                target: form; property: "x"; to: -page.navigationDirection * formScroll.width / 12
                duration: Utils.appearanceSpeed
                easing.type: Easing.Bezier; easing.bezierCurve: RinAnimations.BezierCurve.softDismiss
            }
        }
        ScriptAction {
            script: {
                var previous = page.step
                if (page.navigationAction) Backend.command(page.navigationAction)
                else Backend.setNumber("wizardStep", page.navigationTarget)
                if (page.step !== previous) page.navigationDirection = page.step > previous ? 1 : -1
                formScroll.contentY = 0
                form.x = page.navigationDirection * formScroll.width / 12
            }
        }
        ParallelAnimation {
            NumberAnimation { target: form; property: "opacity"; to: 1; duration: Utils.appearanceSpeed; easing.type: Easing.InOutQuad }
            NumberAnimation {
                target: form; property: "x"; to: 0; duration: Utils.animationSpeedMiddle
                easing.type: Easing.Bezier; easing.bezierCurve: RinAnimations.BezierCurve.pointToPoint
            }
        }
    }
    padding: 48

    Flickable {
        id: workbenchScroll
        objectName: "homeWorkbenchScroll"
        anchors.fill: parent
        visible: !page.wizard.active
        clip: true
        boundsBehavior: Flickable.StopAtBounds
        contentWidth: width
        contentHeight: page.isNarrow ? (workbenchGrid.implicitHeight + 6) : height
        interactive: page.isNarrow
        Controls.ScrollBar.vertical: ScrollBar {
            id: workbenchVScroll
            policy: page.isNarrow ? Controls.ScrollBar.AsNeeded : Controls.ScrollBar.AlwaysOff
        }

        GridLayout {
            id: workbenchGrid
            objectName: "homeWorkbenchGrid"
            x: 3
            y: 3
            width: Math.max(0, workbenchScroll.width - 6 - (page.isNarrow && workbenchVScroll.visible ? 12 : 0))
            height: page.isNarrow ? implicitHeight : Math.max(0, workbenchScroll.height - 6)
            columns: page.isNarrow ? 1 : 2
            columnSpacing: 48
            rowSpacing: 28

            ColumnLayout {
                id: leftColumn
                objectName: "homeLeftColumn"
                Layout.fillWidth: true
                Layout.fillHeight: !page.isNarrow
                Layout.alignment: page.isNarrow ? Qt.AlignTop : Qt.AlignVCenter
                Layout.minimumWidth: 0
                Layout.preferredWidth: page.isNarrow
                    ? workbenchGrid.width
                    : Math.max(0, (workbenchGrid.width - workbenchGrid.columnSpacing) * 0.4)
                spacing: 0

                Item {
                    Layout.fillHeight: true
                    visible: !page.isNarrow
                }

                Text {
                    id: greetingTitle
                    objectName: "homeGreetingTitle"
                    Layout.fillWidth: true
                    text: page.greeting
                    typography: Typography.TitleLarge
                    color: Theme.currentTheme.colors.textColor
                    wrapMode: Text.WordWrap
                }

                Text {
                    id: greetingSubtitle
                    objectName: "homeGreetingSubtitle"
                    Layout.fillWidth: true
                    Layout.topMargin: 6
                    text: qsTr("Ready to make your next video?")
                    typography: Typography.Body
                    color: Theme.currentTheme.colors.textSecondaryColor
                    wrapMode: Text.WordWrap
                }

                Text {
                    objectName: "quickStartHeading"
                    Layout.fillWidth: true
                    Layout.topMargin: 28
                    text: qsTr("Quick start")
                    typography: Typography.BodyStrong
                    color: Theme.currentTheme.colors.textColor
                }

                QuickStartCard {
                    id: quickStartCard
                    objectName: "beginWizardButton"
                    Layout.fillWidth: true
                    Layout.topMargin: 10
                    Controls.ToolTip.visible: hovered
                    Controls.ToolTip.delay: 600
                    Controls.ToolTip.text: qsTr("Drafts are kept for this session only")
                    onClicked: Backend.command("wizardBegin")
                }

                Item {
                    Layout.fillHeight: true
                    visible: !page.isNarrow
                }
            }

            ColumnLayout {
                id: rightColumn
                objectName: "homeRightColumn"
                Layout.fillWidth: true
                Layout.fillHeight: !page.isNarrow
                Layout.alignment: Qt.AlignTop
                Layout.minimumWidth: 0
                Layout.preferredWidth: page.isNarrow
                    ? workbenchGrid.width
                    : Math.max(0, (workbenchGrid.width - workbenchGrid.columnSpacing) * 0.6)
                spacing: 0

                Item {
                    Layout.fillHeight: true
                    visible: !page.isNarrow && recentRows.count > 0
                }

                ListView {
                    id: recentTaskList
                    objectName: "recentTaskList"
                    Layout.fillWidth: true
                    Layout.fillHeight: false
                    Layout.preferredHeight: {
                        var total = Math.max(0, contentHeight + topMargin + bottomMargin)
                        if (page.isNarrow)
                            return total
                        return Math.min(Math.max(0, workbenchScroll.height - 6), total)
                    }
                    visible: recentRows.count > 0
                    clip: true
                    spacing: 8
                    topMargin: 3
                    bottomMargin: 3
                    boundsBehavior: Flickable.StopAtBounds
                    interactive: !page.isNarrow
                    cacheBuffer: Math.max(2000, count * 120)
                    activeFocusOnTab: false
                    verticalScrollBar.policy: page.isNarrow ? Controls.ScrollBar.AlwaysOff : Controls.ScrollBar.AsNeeded
                    model: recentRows

                    delegate: RecentTaskItem {
                        id: recentDelegate
                        required property var model
                        required property int index

                        taskId: model.taskId
                        title: model.title
                        taskState: model.taskState
                        done: model.done
                        total: model.total
                        outputWidth: model.outputWidth
                        outputHeight: model.outputHeight
                        outputFps: model.outputFps
                        codec: model.codec
                        thumbnail: model.thumbnail
                        detail: model.detail

                        x: 3
                        width: Math.max(0, recentTaskList.width - 6 - (!page.isNarrow && recentTaskList.verticalScrollBar.visible ? 12 : 0))
                        onClicked: page.openTaskDetail(recentDelegate.taskId)

                        Keys.onUpPressed: function(event) {
                            if (index > 0) {
                                recentTaskList.positionViewAtIndex(index - 1, ListView.Contain)
                                var prev = recentTaskList.itemAtIndex(index - 1)
                                if (prev)
                                    prev.forceActiveFocus(Qt.TabFocusReason)
                            }
                            event.accepted = true
                        }
                        Keys.onDownPressed: function(event) {
                            if (index + 1 < recentTaskList.count) {
                                recentTaskList.positionViewAtIndex(index + 1, ListView.Contain)
                                var next = recentTaskList.itemAtIndex(index + 1)
                                if (next)
                                    next.forceActiveFocus(Qt.TabFocusReason)
                            }
                            event.accepted = true
                        }
                    }
                }

                Item {
                    Layout.fillHeight: true
                    visible: !page.isNarrow && recentRows.count > 0
                }

                Item {
                    id: emptyState
                    objectName: "recentTasksEmptyState"
                    Layout.fillWidth: true
                    Layout.fillHeight: !page.isNarrow
                    Layout.minimumHeight: 240
                    visible: recentRows.count === 0

                    ColumnLayout {
                        anchors.centerIn: parent
                        width: Math.min(parent.width - 24, 340)
                        spacing: 10

                        Icon {
                            Layout.alignment: Qt.AlignHCenter
                            name: "ic_fluent_movies_and_tv_20_regular"
                            size: 36
                            color: Theme.currentTheme.colors.textSecondaryColor
                            opacity: 0.7
                        }

                        Text {
                            Layout.fillWidth: true
                            horizontalAlignment: Text.AlignHCenter
                            text: qsTr("No render tasks yet")
                            typography: Typography.BodyStrong
                            color: Theme.currentTheme.colors.textColor
                        }

                        Text {
                            Layout.fillWidth: true
                            horizontalAlignment: Text.AlignHCenter
                            text: qsTr("Create your first task to see its progress and history here.")
                            typography: Typography.Caption
                            color: Theme.currentTheme.colors.textSecondaryColor
                            wrapMode: Text.WordWrap
                        }

                        Button {
                            id: emptyCreateButton
                            objectName: "emptyStateCreateButton"
                            Layout.alignment: Qt.AlignHCenter
                            Layout.topMargin: 6
                            highlighted: false
                            text: qsTr("Get started")
                            icon.name: "ic_fluent_add_20_regular"
                            focusPolicy: Qt.StrongFocus
                            onClicked: Backend.command("wizardBegin")
                            Keys.onReturnPressed: function(event) {
                                click()
                                event.accepted = true
                            }
                            Keys.onEnterPressed: function(event) {
                                click()
                                event.accepted = true
                            }
                        }
                    }
                }
            }
        }
    }

    RowLayout {
        anchors.fill: parent
        visible: page.wizard.active === true
        spacing: 28
        ColumnLayout {
            Layout.fillWidth: true
            Layout.preferredWidth: 55
            Layout.minimumWidth: 480
            Layout.fillHeight: true
            spacing: 10
            Text {
                objectName: "createTaskTitle"
                text: page.wizard.redo ? qsTr("Redo task") : qsTr("Create task")
                typography: Typography.Title
                Layout.bottomMargin: 6
            }
            RowLayout {
                Layout.fillWidth: true
                Repeater {
                    id: stepButtons
                    model: page.titles
                    Button {
                        required property string modelData
                        required property int index
                        objectName: "wizardStep" + index
                        Layout.fillWidth: true
                        flat: index !== page.step
                        highlighted: index === page.step
                        text: (index + 1) + "  " + modelData
                        icon.name: ((page.wizard.completed || 0) & (1 << index)) ? "ic_fluent_checkmark_20_regular" : ""
                        focusPolicy: Qt.StrongFocus
                        Accessible.description: index === page.step ? qsTr("Current step")
                            : (((page.wizard.completed || 0) & (1 << index)) ? qsTr("Completed") : qsTr("Not visited yet"))
                        onClicked: page.navigate(index)
                        Keys.onLeftPressed: function(event) { stepButtons.itemAt(Math.max(0, index - 1)).forceActiveFocus(Qt.TabFocusReason); event.accepted = true }
                        Keys.onRightPressed: function(event) { stepButtons.itemAt(Math.min(3, index + 1)).forceActiveFocus(Qt.TabFocusReason); event.accepted = true }
                        Keys.onPressed: function(event) {
                            if (event.key === Qt.Key_Home || event.key === Qt.Key_End) {
                                stepButtons.itemAt(event.key === Qt.Key_Home ? 0 : 3).forceActiveFocus(Qt.TabFocusReason)
                                event.accepted = true
                            } else if (event.key === Qt.Key_Return || event.key === Qt.Key_Enter) {
                                page.navigate(index)
                                event.accepted = true
                            }
                        }
                    }
                }
            }
            Flickable {
                id: formScroll
                objectName: "wizardFormScroll"
                Layout.fillWidth: true
                Layout.fillHeight: true
                contentWidth: width
                contentHeight: form.implicitHeight
                clip: true
                boundsBehavior: Flickable.StopAtBounds
                interactive: !page.navigating
                Controls.ScrollBar.vertical: ScrollBar {}
                ColumnLayout {
                    id: form
                    objectName: "wizardAnimatedForm"
                    width: formScroll.width - 16
                    spacing: 12
                    SettingsPanel {
                        Layout.fillWidth: true
                        visible: page.step < 3
                        step: page.step
                        defaults: JSON.parse(Backend.defaultsJson || "{}")
                        project: page.project
                    }
                    ColumnLayout {
                        Layout.fillWidth: true
                        visible: page.step === 3
                        spacing: 14
                        Repeater {
                            model: [
                                {
                                    heading: qsTr("Media"), target: 0,
                                    body: qsTr("Audio: %1\nLyrics: %2\nCover: %3\nBackground: %4\nSong title: %5")
                                        .arg(page.project.audio || qsTr("None selected"))
                                        .arg(page.project.lyricsPath || qsTr("Embedded lyrics"))
                                        .arg(page.project.coverPath || (page.cfg.embeddedCover ? qsTr("Embedded cover") : qsTr("No external cover")))
                                        .arg(page.project.background || qsTr("Automatic background"))
                                        .arg(page.project.title || qsTr("Title from audio metadata"))
                                },
                                {
                                    heading: qsTr("Style"), target: 1,
                                    body: qsTr("Font size: %1% · Line height: %2% · Line spacing: %3%\nTranslation: %4 · Backing vocals: %5")
                                        .arg(page.cfg.fontScale).arg(page.cfg.lineHeight).arg(page.cfg.lineSpacing)
                                        .arg(Ui.onOff(page.cfg.translation)).arg(Ui.onOff(page.cfg.backgroundVocal)),
                                    details: qsTr("Translation font size: %1% · Backing vocal font size: %2% · Side margins: %3%\nTranslation spacing: %4% · Backing vocal spacing: %5% · Blur σ step: %6\nEmbedded cover: %7 · Debug outlines: %8")
                                        .arg(page.cfg.translationFontScale).arg(page.cfg.backgroundFontScale).arg(page.cfg.horizontalPadding)
                                        .arg(page.cfg.translationGap).arg(page.cfg.backgroundGap).arg(page.cfg.blurStep)
                                        .arg(Ui.onOff(page.cfg.embeddedCover)).arg(Ui.onOff(page.cfg.debugOverlays))
                                },
                                {
                                    heading: qsTr("Output"), target: 2,
                                    body: qsTr("%1 × %2 · %3 fps\n%4 · %5\n%6\n%7 · Audio: %8")
                                        .arg(page.cfg.width).arg(page.cfg.height).arg(page.cfg.fps)
                                        .arg(Ui.encoderLabel((page.cfg.encoders || [])[page.cfg.encoder]))
                                        .arg((page.cfg.codecs || [])[page.cfg.codec] || "")
                                        .arg(page.wizard.effectiveOutput || page.project.output || qsTr("No path selected"))
                                        .arg(Ui.modeLabel(page.wizard.mode || 0))
                                        .arg(Ui.onOff(page.cfg.includeAudio && (page.wizard.mode === 0 || (page.wizard.redo && page.wizard.mode === 1)))),
                                    details: qsTr("Sample start: %1 ms · Duration: %2\nPreview interval (frames): %3 · NVENC p%4")
                                        .arg(page.cfg.sampleStart)
                                        .arg(page.cfg.sampleDuration == null ? qsTr("Until the end") : qsTr("%1 ms").arg(page.cfg.sampleDuration))
                                        .arg(page.cfg.previewInterval).arg(Number(page.cfg.nvencPreset) + 1)
                                }
                            ]
                            SectionCard {
                                required property var modelData
                                Layout.fillWidth: true
                                title: modelData.heading
                                Text { Layout.fillWidth: true; text: modelData.body; textFormat: Text.PlainText; wrapMode: Text.WrapAnywhere }
                                Expander {
                                    Layout.fillWidth: true
                                    implicitWidth: 0
                                    visible: !!modelData.details
                                    text: qsTr("Detailed settings")
                                    Text { Layout.fillWidth: true; text: modelData.details || ""; textFormat: Text.PlainText; wrapMode: Text.WordWrap }
                                }
                                Button { text: qsTr("Edit"); onClicked: page.navigate(modelData.target) }
                            }
                        }
                        Repeater {
                            model: page.wizard.issues || []
                            ColumnLayout {
                                required property var modelData
                                Layout.fillWidth: true
                                RowLayout {
                                    Layout.fillWidth: true
                                    Text {
                                        Layout.fillWidth: true
                                        text: Ui.messageText(modelData.message)
                                        textFormat: Text.PlainText
                                        color: Theme.currentTheme.colors.systemCriticalColor
                                        wrapMode: Text.WordWrap
                                    }
                                    Button {
                                        visible: modelData.canAccept === true
                                        text: qsTr("Use current file")
                                        onClicked: Backend.command("acceptAsset:" + modelData.field)
                                    }
                                    Button {
                                        text: modelData.canAccept !== undefined ? qsTr("Locate file…") : qsTr("Edit")
                                        onClicked: page.navigate(modelData.step)
                                    }
                                }
                                Expander {
                                    Layout.fillWidth: true
                                    implicitWidth: 0
                                    visible: !!(modelData.message && modelData.message.detail)
                                    text: qsTr("Error details")
                                    Text { Layout.fillWidth: true; text: modelData.message ? modelData.message.detail : ""; textFormat: Text.PlainText; wrapMode: Text.WrapAnywhere }
                                }
                            }
                        }
                        Text {
                            Layout.fillWidth: true
                            visible: page.wizard.checked && (page.wizard.issues || []).length === 0
                            text: qsTr("Checks passed")
                            color: Theme.currentTheme.colors.systemSuccessColor
                            wrapMode: Text.WordWrap
                        }
                        Button { text: qsTr("Check settings"); onClicked: Backend.command("wizardCheck") }
                    }
                }
                MouseArea {
                    parent: formScroll
                    anchors.fill: parent
                    z: 1
                    visible: page.navigating
                    acceptedButtons: Qt.AllButtons
                    onWheel: function(event) { event.accepted = true }
                }
            }
            Rectangle { Layout.fillWidth: true; implicitHeight: 1; color: Theme.currentTheme.colors.dividerBorderColor }
            RowLayout {
                Layout.fillWidth: true
                Button {
                    objectName: "wizardBackButton"
                    text: page.step === 0 ? qsTr("Back to Home") : qsTr("Back")
                    enabled: !page.navigating
                    onClicked: {
                        if (page.step === 0) Backend.command("wizardHome")
                        else page.navigate(page.step - 1, "wizardBack")
                    }
                }
                Item { Layout.fillWidth: true }
                Button {
                    objectName: "wizardNextButton"
                    text: page.step === 3 ? qsTr("Create task") : qsTr("Next")
                    highlighted: true
                    enabled: !page.navigating
                    onClicked: {
                        if (page.step === 3) Backend.command("wizardCreate")
                        else page.navigate(page.step + 1, "wizardNext")
                    }
                }
            }
        }
        Rectangle { Layout.fillHeight: true; implicitWidth: 1; color: Theme.currentTheme.colors.dividerBorderColor }
        ColumnLayout {
            Layout.fillWidth: true
            Layout.preferredWidth: 45
            Layout.minimumWidth: 360
            Layout.fillHeight: true
            spacing: 14
            Layout.topMargin: 6
            PreviewPanel {
                id: persistentPreview
                objectName: "wizardPersistentPreview"
                Layout.fillWidth: true
                styleSource: Backend.styleSource
                renderSource: Backend.renderSource
                styleError: page.task.styleError || null
                previewNote: page.task.previewNote || null
                task: page.task
                mode: 1
            }
            Text {
                Layout.fillWidth: true
                text: qsTr("%1 × %2 · %3 fps\n%4 · %5").arg(page.cfg.width || "").arg(page.cfg.height || "").arg(page.cfg.fps || "").arg(Ui.encoderLabel((page.cfg.encoders || [])[page.cfg.encoder])).arg((page.cfg.codecs || [])[page.cfg.codec] || "")
                wrapMode: Text.WordWrap
                color: Theme.currentTheme.colors.textSecondaryColor
            }
            Item { Layout.fillHeight: true }
        }
    }
    onStepChanged: formScroll.contentY = 0
}
