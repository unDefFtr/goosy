import QtQuick
import QtQuick.Controls as Controls
import QtQuick.Layouts
import RinUI
import "../js/ui.js" as Ui

Controls.AbstractButton {
    id: root

    property string taskId: ""
    property string title: ""
    property string taskState: "idle"
    property real done: 0
    property real total: 0
    property int outputWidth: 0
    property int outputHeight: 0
    property int outputFps: 0
    property string codec: ""
    property string thumbnail: ""
    property string detail: ""
    property bool keyboardFocused: false

    readonly property bool isActiveState: Ui.isActive(root.taskState)
    readonly property bool isRunningState: root.taskState === "running" || root.taskState === "starting"
    readonly property bool showProgress: root.isActiveState && root.total > 0
    readonly property string outputSpec: Ui.formatOutputSpec(root.outputWidth, root.outputHeight, root.outputFps)
    readonly property string statusText: root.isRunningState && root.total > 0
        ? (Ui.stateLabel(root.taskState) + " " + Ui.progressPercent(root.done, root.total))
        : Ui.stateLabel(root.taskState)

    focusPolicy: Qt.StrongFocus
    hoverEnabled: true
    leftPadding: 12
    rightPadding: 14
    topPadding: 10
    bottomPadding: 10

    Accessible.role: Accessible.ListItem
    Accessible.name: (root.title || qsTr("未命名任务")) + "，" + root.statusText
    Accessible.description: [root.codec, root.outputSpec].filter(function(item) { return item.length > 0 }).join(" · ")

    implicitWidth: Math.max(contentItem.implicitWidth + leftPadding + rightPadding, 200)
    implicitHeight: contentItem.implicitHeight + topPadding + bottomPadding

    Keys.onReturnPressed: function(event) {
        root.clicked()
        event.accepted = true
    }
    Keys.onEnterPressed: function(event) {
        root.clicked()
        event.accepted = true
    }

    HoverHandler {
        cursorShape: Qt.PointingHandCursor
    }

    ToolTip {
        visible: root.hovered && root.taskState === "failed" && root.detail.length > 0
        text: root.detail
    }

    background: Rectangle {
        id: itemBg
        radius: Theme.currentTheme.appearance.buttonRadius
        color: root.pressed
            ? Theme.currentTheme.colors.controlTertiaryColor
            : root.hovered
                ? Theme.currentTheme.colors.controlSecondaryColor
                : Theme.currentTheme.colors.cardColor
        border.width: Theme.currentTheme.appearance.borderWidth
        border.color: root.hovered
            ? Theme.currentTheme.colors.controlBorderColor
            : Theme.currentTheme.colors.cardBorderColor

        Behavior on color {
            ColorAnimation { duration: Utils.appearanceSpeed; easing.type: Easing.OutQuad }
        }
        Behavior on border.color {
            ColorAnimation { duration: Utils.appearanceSpeed; easing.type: Easing.OutQuad }
        }

        FocusIndicator {
            control: root
            radius: itemBg.radius
            keyboardFocus: root.keyboardFocused
        }
    }

    contentItem: RowLayout {
        spacing: 12

        Rectangle {
            id: thumbBox
            objectName: "recentTaskThumbnail"
            Layout.preferredWidth: 96
            Layout.preferredHeight: 54
            Layout.minimumWidth: 96
            Layout.maximumWidth: 96
            Layout.minimumHeight: 54
            Layout.maximumHeight: 54
            Layout.alignment: Qt.AlignVCenter
            radius: Theme.currentTheme.appearance.smallRadius
            color: Theme.currentTheme.colors.controlAltSecondaryColor
            border.width: Theme.currentTheme.appearance.borderWidth
            border.color: Theme.currentTheme.colors.cardBorderColor
            clip: true

            Image {
                id: thumbImage
                anchors.fill: parent
                anchors.margins: Theme.currentTheme.appearance.borderWidth
                source: root.thumbnail
                fillMode: Image.PreserveAspectCrop
                asynchronous: true
                cache: false
                visible: status === Image.Ready
            }

            Icon {
                anchors.centerIn: parent
                visible: thumbImage.status !== Image.Ready
                name: "ic_fluent_filmstrip_20_regular"
                size: 20
                color: Theme.currentTheme.colors.textSecondaryColor
                opacity: 0.65
            }

            Rectangle {
                anchors.fill: parent
                radius: thumbBox.radius
                color: "transparent"
                border.width: Theme.currentTheme.appearance.borderWidth
                border.color: Theme.currentTheme.colors.cardBorderColor
            }
        }

        ColumnLayout {
            Layout.fillWidth: true
            Layout.minimumWidth: 0
            Layout.alignment: Qt.AlignVCenter
            spacing: 6

            RowLayout {
                Layout.fillWidth: true
                Layout.minimumWidth: 0
                spacing: 8

                Text {
                    Layout.fillWidth: true
                    Layout.minimumWidth: 0
                    Layout.alignment: Qt.AlignVCenter
                    text: root.title || qsTr("未命名任务")
                    typography: Typography.BodyStrong
                    color: Theme.currentTheme.colors.textColor
                    elide: Text.ElideRight
                    maximumLineCount: 1
                    wrapMode: Text.NoWrap
                }

                Rectangle {
                    id: statusBadge
                    Layout.alignment: Qt.AlignVCenter
                    implicitWidth: statusRow.implicitWidth + 14
                    implicitHeight: statusRow.implicitHeight + 6
                    radius: Theme.currentTheme.appearance.smallRadius
                    color: Ui.stateBackgroundColor(root.taskState, Theme.currentTheme.colors)
                    border.width: Theme.currentTheme.appearance.borderWidth
                    border.color: Qt.alpha(Ui.stateColor(root.taskState, Theme.currentTheme.colors), 0.28)

                    RowLayout {
                        id: statusRow
                        anchors.centerIn: parent
                        spacing: 4

                        Icon {
                            size: 13
                            name: Ui.stateIcon(root.taskState)
                            color: Ui.stateColor(root.taskState, Theme.currentTheme.colors)
                            Layout.alignment: Qt.AlignVCenter
                        }

                        Text {
                            text: root.statusText
                            typography: Typography.Caption
                            color: Ui.stateColor(root.taskState, Theme.currentTheme.colors)
                            maximumLineCount: 1
                            wrapMode: Text.NoWrap
                            Layout.alignment: Qt.AlignVCenter
                        }
                    }
                }
            }

            RowLayout {
                Layout.fillWidth: true
                Layout.minimumWidth: 0
                spacing: 8

                Rectangle {
                    visible: root.codec.length > 0
                    Layout.alignment: Qt.AlignVCenter
                    implicitWidth: codecLabel.implicitWidth + 10
                    implicitHeight: codecLabel.implicitHeight + 4
                    radius: Theme.currentTheme.appearance.smallRadius
                    color: Theme.currentTheme.colors.controlColor
                    border.width: Theme.currentTheme.appearance.borderWidth
                    border.color: Theme.currentTheme.colors.controlBorderColor

                    Text {
                        id: codecLabel
                        anchors.centerIn: parent
                        text: root.codec
                        typography: Typography.Caption
                        color: Theme.currentTheme.colors.textSecondaryColor
                        maximumLineCount: 1
                        wrapMode: Text.NoWrap
                    }
                }

                Text {
                    Layout.fillWidth: true
                    Layout.minimumWidth: 0
                    Layout.alignment: Qt.AlignVCenter
                    visible: root.outputSpec.length > 0
                    text: root.outputSpec
                    typography: Typography.Caption
                    color: Theme.currentTheme.colors.textSecondaryColor
                    elide: Text.ElideRight
                    maximumLineCount: 1
                    wrapMode: Text.NoWrap
                }
            }

            ProgressBar {
                Layout.fillWidth: true
                visible: root.showProgress
                from: 0
                to: 1
                value: Ui.progressValue(root.done, root.total)
                state: root.taskState === "paused" ? ProgressBar.Paused : ProgressBar.Running
            }
        }
    }
}
