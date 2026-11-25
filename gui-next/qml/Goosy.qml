import QtQuick
import RinUI

FluentWindow {
    id: window
    visible: true
    width: 1440
    height: 900
    minimumWidth: 1280
    minimumHeight: 720
    title: "Goosy"
    navigationItems: [
        { title: qsTr("Home"), page: Qt.resolvedUrl("pages/Home.qml"), icon: "ic_fluent_home_20_regular" },
        { title: qsTr("Tasks"), page: Qt.resolvedUrl("pages/Tasks.qml"), icon: "ic_fluent_task_list_square_ltr_20_regular" },
        { title: qsTr("Settings"), page: Qt.resolvedUrl("pages/Settings.qml"), icon: "ic_fluent_settings_20_regular", position: Position.Bottom }
    ]
    property var draft: JSON.parse(Backend.projectJson || "{}")
    property string lastCreatedId: ""
    property double lastRedoRevision: 0
    onDraftChanged: {
        var wizard = draft.wizard || {}
        var revision = Number(wizard.redoRevision || 0)
        if (revision > lastRedoRevision) {
            lastRedoRevision = revision
            navigationView.push(Qt.resolvedUrl("pages/Home.qml"))
            return
        }
        var created = wizard.createdId || ""
        if (created && created !== lastCreatedId) {
            lastCreatedId = created
            navigationView.push(Qt.resolvedUrl("pages/Tasks.qml"))
        }
    }
    Timer {
        interval: 50
        repeat: true
        running: true
        onTriggered: Backend.tick()
    }
}
