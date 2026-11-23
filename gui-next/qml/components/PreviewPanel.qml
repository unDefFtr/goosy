import QtQuick
import QtQuick.Layouts
import RinUI

SectionCard {
    id: root

    property string renderSource: ""
    property string styleSource: ""
    property string styleError: ""
    property string previewNote: ""
    property var task: ({})
    property bool active: false
    property int mode: 0

    readonly property bool renderAvailable: root.renderSource.length > 0
    readonly property bool styleAvailable: root.styleSource.length > 0
    readonly property bool showRender: root.renderAvailable
        && (root.mode === 0 || !root.styleAvailable)
    readonly property bool showStyle: root.styleAvailable && !root.showRender
    readonly property bool hasImage: root.showRender || root.showStyle

    showHeading: false
    iconName: "ic_fluent_image_20_regular"

    onActiveChanged: {
        if (root.active) {
            root.mode = 0
        }
    }

    RowLayout {
        Layout.fillWidth: true
        spacing: 12

        Text {
            Layout.fillWidth: true
            typography: Typography.BodyLarge
            text: root.showRender ? qsTr("预览 · 任务 #%1").arg(root.task.id || "") : qsTr("预览")
            elide: Text.ElideRight
        }

        Text {
            typography: Typography.Caption
            color: Theme.currentTheme.colors.textSecondaryColor
            text: root.showRender && (Number(root.task.previewFrame) || 0) > 0
                ? qsTr("第 %1 帧").arg(root.task.previewFrame)
                : ""
            visible: text.length > 0
        }

        Segmented {
            id: modeTabs
            objectName: "previewModeTabs"
            currentIndex: root.mode
            onCurrentIndexChanged: root.mode = currentIndex

            SegmentedItem {
                objectName: "renderPreviewTab"
                text: qsTr("任务画面")
                enabled: root.renderAvailable
            }

            SegmentedItem {
                objectName: "stylePreviewTab"
                text: qsTr("当前样式")
                enabled: root.styleAvailable
            }
        }
    }

    Connections {
        target: root
        function onModeChanged() {
            if (modeTabs && modeTabs.currentIndex !== root.mode) {
                modeTabs.currentIndex = root.mode
            }
        }
    }

    Rectangle {
        Layout.fillWidth: true
        Layout.preferredHeight: Math.round(width * 9 / 16)
        radius: Theme.currentTheme.appearance.smallRadius
        color: "#151515"
        clip: true

        Image {
            id: renderPreviewImage
            objectName: "renderPreviewImage"
            anchors.fill: parent
            source: root.renderSource
            visible: root.showRender
            fillMode: Image.PreserveAspectFit
            asynchronous: false
            cache: false
        }

        Image {
            id: stylePreviewImage
            objectName: "stylePreviewImage"
            anchors.fill: parent
            source: root.styleSource
            visible: root.showStyle
            fillMode: Image.PreserveAspectFit
            asynchronous: false
            cache: false
        }

        Text {
            objectName: "previewPlaceholder"
            anchors.centerIn: parent
            width: parent.width - 32
            horizontalAlignment: Text.AlignHCenter
            typography: Typography.Body
            color: root.styleError.length > 0
                ? Theme.currentTheme.colors.systemCriticalColor
                : Theme.currentTheme.colors.textSecondaryColor
            text: {
                if (root.hasImage) return ""
                if (root.styleError.length > 0) return qsTr("预览不可用")
                if (root.previewNote.length > 0) return root.previewNote
                return root.active ? qsTr("等待画面…") : qsTr("加载预览…")
            }
            visible: text.length > 0
            wrapMode: Text.WordWrap
        }
    }

    Expander {
        Layout.fillWidth: true
        implicitWidth: 0
        visible: root.styleError.length > 0
        text: qsTr("预览错误")
        Text {
            objectName: "styleErrorLabel"
            Layout.fillWidth: true
            text: root.styleError
            color: Theme.currentTheme.colors.systemCriticalColor
            wrapMode: Text.WrapAnywhere
        }
    }

    RowLayout {
        Layout.fillWidth: true
        spacing: 6
        visible: root.previewNote.length > 0 && root.hasImage

        Icon {
            Layout.alignment: Qt.AlignTop
            size: 16
            name: "ic_fluent_info_20_regular"
            color: Theme.currentTheme.colors.textSecondaryColor
        }

        Text {
            objectName: "previewNoteLabel"
            Layout.fillWidth: true
            typography: Typography.Caption
            color: Theme.currentTheme.colors.textSecondaryColor
            text: root.previewNote
            wrapMode: Text.WordWrap
        }
    }
}
