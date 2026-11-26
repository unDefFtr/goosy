import QtQuick
import QtQuick.Layouts
import RinUI

ColumnLayout {
    id: root

    property string label: ""
    property string suffix: "%"
    property string hint: ""
    property real from: 0
    property real to: 100
    property real stepSize: 1
    property real value: 0
    property string controlObjectName: ""
    readonly property alias sliderValue: slider.value

    signal moved(real value)

    Layout.fillWidth: true
    spacing: 0

    RowLayout {
        Layout.fillWidth: true
        spacing: 8

        Text {
            Layout.fillWidth: true
            typography: Typography.Body
            text: root.label
            wrapMode: Text.WordWrap
        }

        Text {
            typography: Typography.Body
            color: Theme.currentTheme.colors.textSecondaryColor
            text: Math.round(slider.value) + root.suffix
        }
    }

    Slider {
        id: slider
        objectName: root.controlObjectName
        Layout.fillWidth: true
        from: root.from
        to: root.to
        stepSize: root.stepSize
        value: root.value
        Accessible.name: root.label

        onMoved: {
            if (root.value !== slider.value) {
                root.value = slider.value
            }
            root.moved(slider.value)
        }
    }

    Text {
        Layout.fillWidth: true
        typography: Typography.Caption
        color: Theme.currentTheme.colors.textSecondaryColor
        text: root.hint
        visible: root.hint.length > 0
        wrapMode: Text.WordWrap
    }
}
