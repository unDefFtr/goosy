import QtQuick
import QtQuick.Controls as Controls
import QtQuick.Layouts
import RinUI
import "qrc:/rinui/RinUI/animations" as RinAnimations
import "../components"

Controls.Page {
    id: page
    objectName: "homePage"
    background: Item {}
    property var project: JSON.parse(Backend.projectJson || "{}")
    property var task: JSON.parse(Backend.taskJson || "{}")
    readonly property var wizard: project.wizard || {}
    readonly property var cfg: project.settings || {}
    readonly property int step: wizard.step || 0
    readonly property var titles: [qsTr("素材"), qsTr("样式"), qsTr("输出"), qsTr("确认")]
    readonly property bool navigating: stepTransition.running
    property int navigationDirection: 1
    property string navigationAction: ""
    property int navigationTarget: 0

    function navigate(target, action) {
        if (navigating || (target === step && !action)) return
        navigationDirection = target > step ? 1 : -1
        navigationTarget = target
        navigationAction = action || ""
        stepButtons.itemAt(step).forceActiveFocus(Qt.OtherFocusReason)
        stepTransition.start()
    }

    SequentialAnimation {
        id: stepTransition
        objectName: "wizardStepTransition"
        ParallelAnimation {
            NumberAnimation { target: form; property: "opacity"; to: 0; duration: Utils.appearanceSpeed; easing.type: Easing.InOutQuad }
            NumberAnimation {
                target: form; property: "x"; to: -page.navigationDirection * formScroll.width / 12
                duration: Utils.appearanceSpeed
                easing.type: Easing.Bezier; easing.bezierCurve: RinAnimations.BezierCurve.softDismiss
            }
        }
        ScriptAction {
            script: {
                var previous = page.step
                if (page.navigationAction) Backend.command(page.navigationAction)
                else Backend.setNumber("wizardStep", page.navigationTarget)
                if (page.step !== previous) page.navigationDirection = page.step > previous ? 1 : -1
                formScroll.contentY = 0
                form.x = page.navigationDirection * formScroll.width / 12
            }
        }
        ParallelAnimation {
            NumberAnimation { target: form; property: "opacity"; to: 1; duration: Utils.appearanceSpeed; easing.type: Easing.InOutQuad }
            NumberAnimation {
                target: form; property: "x"; to: 0; duration: Utils.animationSpeedMiddle
                easing.type: Easing.Bezier; easing.bezierCurve: RinAnimations.BezierCurve.pointToPoint
            }
        }
    }
    padding: 28

    ColumnLayout {
        anchors.centerIn: parent
        width: Math.min(parent.width - 80, 660)
        visible: !page.wizard.active
        spacing: 18
        Text { text: "Goosy"; typography: Typography.Display }
        Text {
            Layout.fillWidth: true
            text: qsTr("音乐与歌词视频渲染工具")
            color: Theme.currentTheme.colors.textSecondaryColor
            wrapMode: Text.WordWrap
        }
        Button {
            objectName: "beginWizardButton"
            text: qsTr("创建任务")
            Controls.ToolTip.visible: hovered
            Controls.ToolTip.text: qsTr("草稿仅保留于本次会话")
            highlighted: true
            icon.name: "ic_fluent_add_20_regular"
            onClicked: Backend.command("wizardBegin")
        }
    }

    RowLayout {
        anchors.fill: parent
        visible: page.wizard.active === true
        spacing: 28
        ColumnLayout {
            Layout.fillWidth: true
            Layout.preferredWidth: 55
            Layout.minimumWidth: 480
            Layout.fillHeight: true
            spacing: 10
            Text {
                objectName: "createTaskTitle"
                text: qsTr("创建任务")
                typography: Typography.Title
                Layout.bottomMargin: 6
            }
            RowLayout {
                Layout.fillWidth: true
                Repeater {
                    id: stepButtons
                    model: page.titles
                    Button {
                        required property string modelData
                        required property int index
                        objectName: "wizardStep" + index
                        Layout.fillWidth: true
                        flat: index !== page.step
                        highlighted: index === page.step
                        text: (index + 1) + "  " + modelData
                        icon.name: ((page.wizard.completed || 0) & (1 << index)) ? "ic_fluent_checkmark_20_regular" : ""
                        focusPolicy: Qt.StrongFocus
                        Accessible.description: index === page.step ? qsTr("当前步骤")
                            : (((page.wizard.completed || 0) & (1 << index)) ? qsTr("已完成") : qsTr("待访问"))
                        onClicked: page.navigate(index)
                        Keys.onLeftPressed: function(event) { stepButtons.itemAt(Math.max(0, index - 1)).forceActiveFocus(Qt.TabFocusReason); event.accepted = true }
                        Keys.onRightPressed: function(event) { stepButtons.itemAt(Math.min(3, index + 1)).forceActiveFocus(Qt.TabFocusReason); event.accepted = true }
                        Keys.onPressed: function(event) {
                            if (event.key === Qt.Key_Home || event.key === Qt.Key_End) {
                                stepButtons.itemAt(event.key === Qt.Key_Home ? 0 : 3).forceActiveFocus(Qt.TabFocusReason)
                                event.accepted = true
                            } else if (event.key === Qt.Key_Return || event.key === Qt.Key_Enter) {
                                page.navigate(index)
                                event.accepted = true
                            }
                        }
                    }
                }
            }
            Flickable {
                id: formScroll
                objectName: "wizardFormScroll"
                Layout.fillWidth: true
                Layout.fillHeight: true
                contentWidth: width
                contentHeight: form.implicitHeight
                clip: true
                boundsBehavior: Flickable.StopAtBounds
                interactive: !page.navigating
                Controls.ScrollBar.vertical: ScrollBar {}
                ColumnLayout {
                    id: form
                    objectName: "wizardAnimatedForm"
                    width: formScroll.width - 16
                    spacing: 12
                    SettingsPanel {
                        Layout.fillWidth: true
                        visible: page.step < 3
                        step: page.step
                        defaults: JSON.parse(Backend.defaultsJson || "{}")
                        project: page.project
                    }
                    ColumnLayout {
                        Layout.fillWidth: true
                        visible: page.step === 3
                        spacing: 14
                        Repeater {
                            model: [
                                { heading: qsTr("素材"), target: 0, body: qsTr("音频：%1\n歌词：%2\n封面：%3\n背景：%4\n歌曲名称：%5").arg(page.project.audio || qsTr("未选择")).arg(page.project.lyricsPath || qsTr("内嵌歌词")).arg(page.project.coverPath || qsTr("无外部封面")).arg(page.project.background || qsTr("自动背景")).arg(page.project.title || qsTr("音频元数据标题")) },
                                { heading: qsTr("样式"), target: 1, body: qsTr("字号 %1% · 行高 %2% · 行间距 %3%\n翻译 %4 · 伴唱 %5").arg(page.cfg.fontScale).arg(page.cfg.lineHeight).arg(page.cfg.lineSpacing).arg(page.cfg.translation ? "开" : "关").arg(page.cfg.backgroundVocal ? "开" : "关"), details: qsTr("翻译字号 %1% · 伴唱字号 %2% · 左右留白 %3%\n翻译间距 %4% · 伴唱间距 %5% · 模糊步长 %6\n内嵌封面 %7 · 调试框 %8").arg(page.cfg.translationFontScale).arg(page.cfg.backgroundFontScale).arg(page.cfg.horizontalPadding).arg(page.cfg.translationGap).arg(page.cfg.backgroundGap).arg(page.cfg.blurStep).arg(page.cfg.embeddedCover ? "开" : "关").arg(page.cfg.debugOverlays ? "开" : "关") },
                                { heading: qsTr("输出"), target: 2, body: qsTr("%1 × %2 · %3 fps\n%4 · %5\n%6\n%7 · 音频%8").arg(page.cfg.width).arg(page.cfg.height).arg(page.cfg.fps).arg((page.cfg.encoders || [])[page.cfg.encoder] || "").arg((page.cfg.codecs || [])[page.cfg.codec] || "").arg(page.wizard.effectiveOutput || page.project.output || qsTr("未选择路径")).arg(["完整渲染", "范围采样", "随机采样"][page.wizard.mode || 0]).arg(page.wizard.mode ? "关" : (page.cfg.includeAudio ? "开" : "关")), details: qsTr("采样起点 %1 ms · 时长 %2 ms\n预览间隔 %3 帧 · NVENC p%4").arg(page.cfg.sampleStart).arg(page.cfg.sampleDuration).arg(page.cfg.previewInterval).arg(Number(page.cfg.nvencPreset) + 1) }
                            ]
                            SectionCard {
                                required property var modelData
                                Layout.fillWidth: true
                                title: modelData.heading
                                Text { Layout.fillWidth: true; text: modelData.body; wrapMode: Text.WrapAnywhere }
                                Expander {
                                    Layout.fillWidth: true
                                    implicitWidth: 0
                                    visible: !!modelData.details
                                    text: qsTr("详细设置")
                                    Text { Layout.fillWidth: true; text: modelData.details || ""; wrapMode: Text.WordWrap }
                                }
                                Button { text: qsTr("修改"); onClicked: page.navigate(modelData.target) }
                            }
                        }
                        Repeater {
                            model: page.wizard.issues || []
                            ColumnLayout {
                                required property var modelData
                                Layout.fillWidth: true
                                RowLayout {
                                    Layout.fillWidth: true
                                    Text {
                                        Layout.fillWidth: true
                                        text: modelData.message
                                        color: Theme.currentTheme.colors.systemCriticalColor
                                        wrapMode: Text.WordWrap
                                    }
                                    Button { text: qsTr("修改"); onClicked: page.navigate(modelData.step) }
                                }
                                Expander {
                                    Layout.fillWidth: true
                                    implicitWidth: 0
                                    visible: !!modelData.detail
                                    text: qsTr("错误详情")
                                    Text { Layout.fillWidth: true; text: modelData.detail || ""; wrapMode: Text.WrapAnywhere }
                                }
                            }
                        }
                        Text {
                            Layout.fillWidth: true
                            visible: page.wizard.checked && (page.wizard.issues || []).length === 0
                            text: qsTr("检查通过")
                            color: Theme.currentTheme.colors.systemSuccessColor
                            wrapMode: Text.WordWrap
                        }
                        Button { text: qsTr("检查配置"); onClicked: Backend.command("wizardCheck") }
                    }
                }
                MouseArea {
                    parent: formScroll
                    anchors.fill: parent
                    z: 1
                    visible: page.navigating
                    acceptedButtons: Qt.AllButtons
                    onWheel: function(event) { event.accepted = true }
                }
            }
            Rectangle { Layout.fillWidth: true; implicitHeight: 1; color: Theme.currentTheme.colors.dividerBorderColor }
            RowLayout {
                Layout.fillWidth: true
                Button {
                    objectName: "wizardBackButton"
                    text: page.step === 0 ? qsTr("返回首页") : qsTr("上一步")
                    enabled: !page.navigating
                    onClicked: {
                        if (page.step === 0) Backend.command("wizardHome")
                        else page.navigate(page.step - 1, "wizardBack")
                    }
                }
                Item { Layout.fillWidth: true }
                Button {
                    objectName: "wizardNextButton"
                    text: page.step === 3 ? qsTr("创建任务") : qsTr("下一步")
                    highlighted: true
                    enabled: !page.navigating
                    onClicked: {
                        if (page.step === 3) Backend.command("wizardCreate")
                        else page.navigate(page.step + 1, "wizardNext")
                    }
                }
            }
        }
        Rectangle { Layout.fillHeight: true; implicitWidth: 1; color: Theme.currentTheme.colors.dividerBorderColor }
        ColumnLayout {
            Layout.fillWidth: true
            Layout.preferredWidth: 45
            Layout.minimumWidth: 360
            Layout.fillHeight: true
            spacing: 14
            Layout.topMargin: 6
            PreviewPanel {
                id: persistentPreview
                objectName: "wizardPersistentPreview"
                Layout.fillWidth: true
                styleSource: Backend.styleSource
                renderSource: Backend.renderSource
                styleError: page.task.styleError || ""
                task: page.task
                mode: 1
            }
            Text {
                Layout.fillWidth: true
                text: qsTr("%1 × %2 · %3 fps\n%4 · %5").arg(page.cfg.width || "").arg(page.cfg.height || "").arg(page.cfg.fps || "").arg((page.cfg.encoders || [])[page.cfg.encoder] || "").arg((page.cfg.codecs || [])[page.cfg.codec] || "")
                wrapMode: Text.WordWrap
                color: Theme.currentTheme.colors.textSecondaryColor
            }
            Item { Layout.fillHeight: true }
        }
    }
    onStepChanged: formScroll.contentY = 0
}
