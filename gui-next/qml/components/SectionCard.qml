import QtQuick
import QtQuick.Layouts
import RinUI

Item {
    id: root

    property string title: ""
    property string subtitle: ""
    property string iconName: ""
    property bool showHeading: true
    default property alias content: body.data
    property alias contentSpacing: body.spacing

    implicitHeight: layout.implicitHeight + (root.showHeading ? 16 : 0)

    ColumnLayout {
        id: layout
        x: 0
        y: root.showHeading ? 8 : 0
        width: parent.width
        height: implicitHeight
        spacing: 10

        Rectangle {
            Layout.fillWidth: true
            visible: root.showHeading
            implicitHeight: 1
            color: Theme.currentTheme.colors.dividerBorderColor
        }

        RowLayout {
            Layout.fillWidth: true
            visible: root.showHeading && root.title.length > 0
            spacing: 8

            Text {
                Layout.fillWidth: true
                typography: Typography.BodyLarge
                text: root.title
                wrapMode: Text.WordWrap
            }

            Icon {
                size: 16
                name: root.iconName
                color: Theme.currentTheme.colors.textSecondaryColor
                visible: root.iconName.length > 0
            }
        }

        Text {
            Layout.fillWidth: true
            typography: Typography.Caption
            color: Theme.currentTheme.colors.textSecondaryColor
            text: root.subtitle
            visible: root.subtitle.length > 0
            wrapMode: Text.WordWrap
        }

        ColumnLayout {
            id: body
            Layout.fillWidth: true
            spacing: 8
        }
    }
}
