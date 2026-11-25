import QtQuick
import QtQuick.Layouts
import RinUI
import "../components"

FluentPage {
    id: page
    title: qsTr("Settings")
    horizontalPadding: 28
    property var task: JSON.parse(Backend.taskJson || "{}")

    SectionCard {
        Layout.fillWidth: true
        title: qsTr("任务调度")
        NumberField {
            Layout.fillWidth: true
            label: qsTr("最大并发任务数")
            controlObjectName: "maxConcurrentSpin"
            from: 1
            to: 8
            value: Number(page.task.maxConcurrent) || 1
            onEdited: function(value) { Backend.setNumber("maxConcurrent", value) }
        }
        Text {
            Layout.fillWidth: true
            text: qsTr("并发上限将保存到用户设置，下次启动时继续生效。")
            color: Theme.currentTheme.colors.textSecondaryColor
            wrapMode: Text.WordWrap
        }
        Text {
            Layout.fillWidth: true
            visible: Number(page.task.maxConcurrent) > 1
            text: qsTr("并行渲染为实验性功能。资源不足时请设为 1。")
            color: Theme.currentTheme.colors.systemCautionColor
            wrapMode: Text.WordWrap
        }
    }
    SectionCard {
        Layout.fillWidth: true
        title: qsTr("外观")
        Button {
            objectName: "themeButton"
            text: qsTr("切换主题")
            icon.name: "ic_fluent_dark_theme_20_regular"
            onClicked: Theme.toggleMode()
        }
    }
    Text {
        Layout.fillWidth: true
        text: page.task.error || ""
        visible: text.length > 0
        color: Theme.currentTheme.colors.systemCriticalColor
        wrapMode: Text.WordWrap
    }
}
