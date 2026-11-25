import QtQuick
import QtQuick.Layouts
import RinUI

Card {
    id: root

    property string title: qsTr("创建渲染任务")
    property string description: qsTr("导入音频、歌词与样式")
    property string iconName: "ic_fluent_filmstrip_play_20_filled"

    focusPolicy: Qt.StrongFocus
    hoverEnabled: true
    leftPadding: 16
    rightPadding: 16
    topPadding: 14
    bottomPadding: 14

    radius: Theme.currentTheme.appearance.windowRadius
    color: root.pressed
        ? Theme.currentTheme.colors.controlTertiaryColor
        : root.hovered
            ? Theme.currentTheme.colors.controlSecondaryColor
            : Theme.currentTheme.colors.cardColor
    border.color: (root.hovered || root.pressed)
        ? Qt.alpha(Theme.currentTheme.colors.primaryColor, 0.45)
        : Theme.currentTheme.colors.cardBorderColor

    Accessible.role: Accessible.Button
    Accessible.name: root.title
    Accessible.description: root.description

    implicitWidth: Math.max(cardContent.implicitWidth + leftPadding + rightPadding, 220)
    implicitHeight: cardContent.implicitHeight + topPadding + bottomPadding

    Keys.onReturnPressed: function(event) {
        root.click()
        event.accepted = true
    }
    Keys.onEnterPressed: function(event) {
        root.click()
        event.accepted = true
    }

    HoverHandler {
        cursorShape: Qt.PointingHandCursor
    }

    contentItem: RowLayout {
        id: cardContent
        spacing: 14

        Rectangle {
            id: iconBadge
            Layout.preferredWidth: 40
            Layout.preferredHeight: 40
            Layout.minimumWidth: 40
            Layout.minimumHeight: 40
            Layout.alignment: Qt.AlignVCenter
            radius: Theme.currentTheme.appearance.buttonRadius
            color: Theme.currentTheme.colors.primaryColor
            scale: root.pressed ? 0.94 : 1.0

            Behavior on scale {
                NumberAnimation { duration: Utils.animationSpeedFaster; easing.type: Easing.OutQuad }
            }

            Icon {
                anchors.centerIn: parent
                name: root.iconName
                size: 20
                color: Theme.currentTheme.colors.textOnAccentColor
            }
        }

        ColumnLayout {
            Layout.fillWidth: true
            Layout.minimumWidth: 0
            Layout.alignment: Qt.AlignVCenter
            spacing: 2

            Text {
                Layout.fillWidth: true
                text: root.title
                typography: Typography.BodyStrong
                color: Theme.currentTheme.colors.textColor
                wrapMode: Text.WordWrap
            }

            Text {
                Layout.fillWidth: true
                text: root.description
                typography: Typography.Caption
                color: Theme.currentTheme.colors.textSecondaryColor
                wrapMode: Text.WordWrap
            }
        }

        Icon {
            Layout.alignment: Qt.AlignVCenter
            name: "ic_fluent_chevron_right_20_regular"
            size: 16
            color: root.hovered
                ? Theme.currentTheme.colors.textAccentColor
                : Theme.currentTheme.colors.textSecondaryColor

            transform: Translate {
                x: root.hovered ? 2 : 0
                Behavior on x {
                    NumberAnimation { duration: Utils.appearanceSpeed; easing.type: Easing.OutQuad }
                }
            }
        }
    }
}
