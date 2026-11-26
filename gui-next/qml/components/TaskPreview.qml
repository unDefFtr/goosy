import QtQuick
import QtQuick.Layouts
import RinUI
import "../js/ui.js" as Ui

ColumnLayout {
    id: root
    property var task: ({})
    property string renderSource: ""
    property string styleSource: ""
    property var styleError: null
    property real maximumImageHeight: 360
    property int mode: 0
    readonly property bool actual: mode === 0
    readonly property string source: actual ? (renderSource || task.savedPreview || "") : styleSource
    readonly property real aspect: {
        var c = (task && task.config) || {};
        return (c.width > 0 && c.height > 0) ? c.width / c.height : 16 / 9;
    }
    readonly property real imageWidth: Math.min(width, maximumImageHeight * aspect)
    spacing: 8
    onModeChanged: if (modes.currentIndex !== mode)
        modes.currentIndex = mode

    RowLayout {
        Layout.fillWidth: true
        Segmented {
            id: modes
            objectName: "taskPreviewModes"
            currentIndex: root.mode
            onCurrentIndexChanged: root.mode = currentIndex
            SegmentedItem {
                text: qsTr("Task preview")
                icon.name: "ic_fluent_filmstrip_20_regular"
            }
            SegmentedItem {
                text: qsTr("Current style")
                icon.name: "ic_fluent_image_20_regular"
            }
        }
        Item {
            Layout.fillWidth: true
        }
    }

    Frame {
        id: viewport
        objectName: "taskPreviewViewport"
        Layout.preferredWidth: root.imageWidth
        Layout.preferredHeight: root.imageWidth / root.aspect
        Layout.alignment: Qt.AlignHCenter
        hoverable: false
        padding: 0
        Image {
            id: frame
            objectName: "selectedTaskPreviewImage"
            anchors.fill: parent
            source: root.source
            cache: false
            asynchronous: false
            fillMode: Image.PreserveAspectFit
        }
        Text {
            anchors.centerIn: parent
            width: parent.width - 32
            visible: !root.source || frame.status === Image.Error
            textFormat: Text.PlainText
            text: {
                if (!root.actual)
                    return root.styleError ? Ui.messageText(root.styleError) : qsTr("Loading style preview…");
                if (frame.status === Image.Error)
                    return qsTr("Could not load the task preview");
                if (root.task.previewNote)
                    return Ui.messageText(root.task.previewNote);
                if (root.task.state === "queued")
                    return qsTr("Queued; no preview yet");
                if (root.task.state === "starting" || root.task.state === "running")
                    return qsTr("Waiting for a rendered frame…");
                return qsTr("No task preview available");
            }
            horizontalAlignment: Text.AlignHCenter
            wrapMode: Text.WordWrap
            color: Theme.currentTheme.colors.textSecondaryColor
        }
    }

    Text {
        Layout.preferredWidth: root.imageWidth
        Layout.alignment: Qt.AlignHCenter
        visible: root.actual && Number(root.task.previewFrame) > 0 && !!root.renderSource
        text: qsTr("Frame %1").arg(root.task.previewFrame || 0)
        typography: Typography.Caption
        color: Theme.currentTheme.colors.textSecondaryColor
        horizontalAlignment: Text.AlignLeft
    }
    Text {
        Layout.fillWidth: true
        readonly property var message: root.actual ? root.task.previewNote : root.styleError
        visible: text.length > 0
        text: message ? [(root.source || (root.actual && frame.status === Image.Error)) ? Ui.messageText(message) : "",
                         message.detail].filter(Boolean).join("\n") : ""
        textFormat: Text.PlainText
        typography: Typography.Caption
        wrapMode: Text.WrapAnywhere
        color: root.actual ? Theme.currentTheme.colors.textSecondaryColor : Theme.currentTheme.colors.systemCriticalColor
    }
}
