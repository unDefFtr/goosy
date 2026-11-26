import QtQuick
import QtQuick.Layouts
import RinUI
import "../js/ui.js" as Ui

FluentPage {
    id: page
    title: qsTr("Settings")
    property var task: JSON.parse(Backend.taskJson || "{}")
    property string languageSwitchError: ""
    readonly property var languageIssue: languageSwitchError
        ? { code: "languageSwitchFailed", detail: languageSwitchError }
        : Backend.languageSaveError ? { code: "languageSaveFailed", detail: Backend.languageSaveError } : null

    function errorText(message) {
        if (!message) return ""
        const summary = Ui.messageText(message)
        return message.detail ? summary + "\n" + message.detail : summary
    }

    component SettingsComboBox: ComboBox {
        id: control
        Layout.preferredWidth: 140
        // RinUI reports popup selections separately from keyboard activation.
        Connections {
            target: control.popup
            function onItemSelected(index) { control.activated(index) }
        }
    }

    Column {
        Layout.fillWidth: true
        spacing: 3
        Text {
            typography: Typography.BodyStrong
            text: qsTr("Appearance")
        }
        SettingCard {
            id: themeCard
            width: parent.width
            title: qsTr("App theme")
            icon.name: "ic_fluent_color_20_regular"
            SettingsComboBox {
                id: themeCombo
                objectName: "themeCombo"
                Accessible.name: themeCard.title
                readonly property var modes: [Theme.mode.Light, Theme.mode.Dark, Theme.mode.Auto]
                model: [qsTr("Light"), qsTr("Dark"), qsTr("Follow system")]
                function syncSelection() { currentIndex = modes.indexOf(Theme.getTheme()) }
                Component.onCompleted: syncSelection()
                onModelChanged: Qt.callLater(syncSelection)
                onActivated: function(index) {
                    const mode = modes[index]
                    if (mode !== Theme.getTheme()) Theme.setTheme(mode)
                }
                Connections {
                    target: ThemeManager
                    function onThemeChanged() { themeCombo.syncSelection() }
                }
            }
        }
        SettingCard {
            id: languageCard
            width: parent.width
            title: qsTr("Language")
            icon.name: "ic_fluent_translate_20_regular"
            SettingsComboBox {
                id: languageCombo
                objectName: "languageCombo"
                Accessible.name: languageCard.title
                readonly property var preferences: ["system", "zh_CN", "en_US"]
                model: [qsTr("Follow system"), "简体中文", "English"]
                function syncSelection() {
                    currentIndex = preferences.indexOf(Backend.languagePreference)
                }
                Component.onCompleted: syncSelection()
                onModelChanged: Qt.callLater(syncSelection)
                Connections {
                    target: Backend
                    function onLanguagePreferenceChanged() { languageCombo.syncSelection() }
                }
                function selectLanguage(index) {
                    const locale = preferences[index]
                    if (locale === Backend.languagePreference) return
                    page.languageSwitchError = Backend.setLanguage(locale)
                    if (page.languageSwitchError) {
                        Qt.callLater(syncSelection)
                    }
                }
                onActivated: index => selectLanguage(index)
            }
        }
        Text {
            width: parent.width
            text: page.errorText(page.languageIssue)
            textFormat: Text.PlainText
            visible: text.length > 0
            color: Theme.currentTheme.colors.systemCriticalColor
            wrapMode: Text.Wrap
        }
        Button {
            text: qsTr("Retry saving language")
            visible: Backend.languageSaveError.length > 0
            onClicked: page.languageSwitchError = Backend.setLanguage(Backend.languagePreference)
        }
    }

    Column {
        Layout.fillWidth: true
        spacing: 3
        Text {
            typography: Typography.BodyStrong
            text: qsTr("Task scheduling")
        }
        SettingCard {
            id: concurrencyCard
            width: parent.width
            title: qsTr("Maximum concurrent tasks")
            description: qsTr("This limit is saved for the next time you start Goosy.")
            icon.name: "ic_fluent_task_list_ltr_20_regular"
            SpinBox {
                id: concurrency
                objectName: "maxConcurrentSpin"
                Accessible.name: concurrencyCard.title
                Layout.preferredWidth: 140
                editable: true
                from: 1
                to: 8
                value: Number(page.task.maxConcurrent) || 1
                onValueModified: Qt.callLater(function() { Backend.setNumber("maxConcurrent", concurrency.value) })
            }
        }
        Text {
            width: parent.width
            visible: Number(page.task.maxConcurrent) > 1
            text: qsTr("Parallel rendering is experimental. Set the limit to 1 if resources are low.")
            color: Theme.currentTheme.colors.systemCautionColor
            wrapMode: Text.WordWrap
        }
        Text {
            width: parent.width
            text: page.errorText(page.task.error)
            textFormat: Text.PlainText
            visible: text.length > 0
            color: Theme.currentTheme.colors.systemCriticalColor
            wrapMode: Text.Wrap
        }
    }
}
