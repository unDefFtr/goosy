import QtQuick
import QtQuick.Layouts
import RinUI

ColumnLayout {
    id: root
    property var task: ({})
    property string renderSource: ""
    property string styleSource: ""
    property string styleError: ""
    property real maximumImageHeight: 360
    property int mode: 0
    readonly property bool actual: mode === 0
    readonly property string source: actual ? renderSource : styleSource
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
                text: qsTr("任务画面")
                icon.name: "ic_fluent_filmstrip_20_regular"
            }
            SegmentedItem {
                text: qsTr("当前样式")
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
            visible: !root.source
            text: {
                if (!root.actual)
                    return root.styleError ? qsTr("样式预览不可用") : qsTr("加载样式预览…");
                if (root.task.previewNote)
                    return root.task.previewNote;
                if (root.task.state === "queued")
                    return qsTr("排队中，暂无画面");
                if (root.task.state === "starting" || root.task.state === "running")
                    return qsTr("等待渲染画面…");
                return qsTr("无可用任务画面");
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
        text: qsTr("第 %1 帧").arg(root.task.previewFrame || 0)
        typography: Typography.Caption
        color: Theme.currentTheme.colors.textSecondaryColor
        horizontalAlignment: Text.AlignLeft
    }
    Text {
        Layout.fillWidth: true
        visible: !root.actual && !!root.styleError
        text: root.styleError
        typography: Typography.Caption
        wrapMode: Text.WrapAnywhere
        color: Theme.currentTheme.colors.systemCriticalColor
    }
}
