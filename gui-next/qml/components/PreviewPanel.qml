import QtQuick
import QtQuick.Layouts
import RinUI
import "../js/ui.js" as Ui

SectionCard {
    id: root

    property string renderSource: ""
    property string styleSource: ""
    property var styleError: null
    property var previewNote: null
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
            text: root.showRender ? qsTr("Preview · Task #%1").arg(root.task.id || "") : qsTr("Preview")
            elide: Text.ElideRight
        }

        Text {
            typography: Typography.Caption
            color: Theme.currentTheme.colors.textSecondaryColor
            text: root.showRender && (Number(root.task.previewFrame) || 0) > 0
                ? qsTr("Frame %1").arg(root.task.previewFrame)
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
                text: qsTr("Task preview")
                enabled: root.renderAvailable
            }

            SegmentedItem {
                objectName: "stylePreviewTab"
                text: qsTr("Current style")
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
            color: !!root.styleError
                ? Theme.currentTheme.colors.systemCriticalColor
                : Theme.currentTheme.colors.textSecondaryColor
            textFormat: Text.PlainText
            text: {
                if (root.hasImage) return ""
                if (root.styleError) return Ui.messageText(root.styleError)
                if (root.previewNote) return Ui.messageText(root.previewNote)
                return root.active ? qsTr("Waiting for a frame…") : qsTr("Loading preview…")
            }
            visible: text.length > 0
            wrapMode: Text.WordWrap
        }
    }

    Expander {
        Layout.fillWidth: true
        implicitWidth: 0
        visible: !!root.styleError
        text: root.hasImage ? Ui.messageText(root.styleError) : qsTr("Preview error")
        Text {
            objectName: "styleErrorLabel"
            Layout.fillWidth: true
            text: root.styleError ? root.styleError.detail || Ui.messageText(root.styleError) : ""
            textFormat: Text.PlainText
            color: Theme.currentTheme.colors.systemCriticalColor
            wrapMode: Text.WrapAnywhere
        }
    }

    RowLayout {
        Layout.fillWidth: true
        spacing: 6
        visible: !!root.previewNote && (root.hasImage || !!root.previewNote.detail || !!root.styleError)

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
            text: root.previewNote ? [(root.hasImage || root.styleError) ? Ui.messageText(root.previewNote) : "",
                                      root.previewNote.detail].filter(Boolean).join("\n") : ""
            textFormat: Text.PlainText
            wrapMode: Text.WordWrap
        }
    }
}
