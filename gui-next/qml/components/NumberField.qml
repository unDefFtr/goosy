import QtQuick
import QtQuick.Layouts
import RinUI

RowLayout {
    id: root

    property string label: ""
    property string accessibleName: label
    property string suffix: ""
    property alias value: spin.value
    property alias from: spin.from
    property alias to: spin.to
    property alias stepSize: spin.stepSize
    property string controlObjectName: ""

    signal edited(int value)

    spacing: 8

    Text {
        Layout.fillWidth: true
        typography: Typography.Body
        text: root.label
        visible: root.label.length > 0
        wrapMode: Text.WordWrap
    }

    SpinBox {
        id: spin
        objectName: root.controlObjectName
        Layout.preferredWidth: 132
        editable: true
        Accessible.name: root.accessibleName

        onValueModified: Qt.callLater(function() { root.edited(Math.round(spin.value)) })
    }

    Text {
        typography: Typography.Caption
        color: Theme.currentTheme.colors.textSecondaryColor
        text: root.suffix
        visible: root.suffix.length > 0
    }

}
