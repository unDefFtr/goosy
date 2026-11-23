import QtQuick
import QtQuick.Layouts
import QtQuick.Dialogs
import RinUI

ColumnLayout {
    id: root

    property var defaults: ({})
    property var project: ({})
    property int step: 0
    readonly property var issues: (root.project.wizard || {}).issues || []
    function errorFor(field) {
        return root.issues.filter(function(issue) { return issue.field === field }).map(function(issue) { return issue.message }).join("\n")
    }

    readonly property var lyricsCandidates: root.project.lyricsCandidates || []
    readonly property var coverCandidates: root.project.coverCandidates || []
    readonly property int lyricsIndex: {
        var count = root.lyricsCandidates.length
        if (count <= 0) return -1
        var index = Number(root.project.lyricsIndex)
        if (isNaN(index)) index = 0
        return Math.max(0, Math.min(count - 1, Math.round(index)))
    }
    readonly property int coverIndex: {
        var count = root.coverCandidates.length
        if (count <= 0) return -1
        var index = Number(root.project.coverIndex)
        if (isNaN(index)) index = 0
        return Math.max(0, Math.min(count - 1, Math.round(index)))
    }
    property bool zeroCopySelected: false

    height: implicitHeight
    spacing: 10

    function cfg(key, fallback) {
        var values = root.project.settings || root.defaults
        if (!values || values[key] === undefined) return fallback
        return values[key]
    }

    function refreshZeroCopy() {
        var zeroCopy = Number(root.cfg("zeroCopyEncoder", -1))
        root.zeroCopySelected = !!encoderCombo
            && zeroCopy >= 0
            && encoderCombo.currentIndex === zeroCopy
    }

    function syncCandidateIndexes() {
        if (lyricsCombo && lyricsCombo.currentIndex !== root.lyricsIndex) {
            lyricsCombo.currentIndex = root.lyricsIndex
        }
        if (coverCombo && coverCombo.currentIndex !== root.coverIndex) {
            coverCombo.currentIndex = root.coverIndex
        }
    }

    Connections {
        target: root

        function onLyricsIndexChanged() {
            if (lyricsCombo && lyricsCombo.currentIndex !== root.lyricsIndex) {
                lyricsCombo.currentIndex = root.lyricsIndex
            }
        }

        function onCoverIndexChanged() {
            if (coverCombo && coverCombo.currentIndex !== root.coverIndex) {
                coverCombo.currentIndex = root.coverIndex
            }
        }

        function onProjectChanged() {
            root.syncCandidateIndexes()
            if (titleField && !titleField.activeFocus) {
                titleField.text = root.project.title ? String(root.project.title) : ""
            }
        }
    }

    onDefaultsChanged: root.refreshZeroCopy()

    Component.onCompleted: {
        root.syncCandidateIndexes()
        root.refreshZeroCopy()
    }

    SectionCard {
        Layout.fillWidth: true
        visible: root.step === 0
        showHeading: false
        contentSpacing: 14

        RowLayout {
            Layout.fillWidth: true
            spacing: 12

            Text {
                Layout.preferredWidth: 80
                typography: Typography.Body
                text: qsTr("音频")
                color: Theme.currentTheme.colors.textSecondaryColor
            }

            Text {
                Layout.fillWidth: true
                typography: Typography.Body
                text: root.project.audio ? String(root.project.audio) : qsTr("尚未选择音频")
                color: root.project.audio
                    ? Theme.currentTheme.colors.textColor
                    : Theme.currentTheme.colors.textSecondaryColor
                elide: Text.ElideMiddle
                maximumLineCount: 1
                wrapMode: Text.NoWrap
            }

            Button {
                objectName: "audioSelectButton"
                Layout.preferredHeight: 36
                Layout.preferredWidth: 76
                text: qsTr("选择…")
                onClicked: audioDialog.open()
            }
            Item { Layout.preferredWidth: 32 }
        }
        Text {
            objectName: "materialStatus"
            Layout.fillWidth: true
            Layout.leftMargin: 92
            visible: !!root.project.notice
            text: root.project.notice || ""
            typography: Typography.Caption
            color: Theme.currentTheme.colors.textSecondaryColor
            wrapMode: Text.WordWrap
        }
        Text {
            Layout.fillWidth: true
            Layout.leftMargin: 92
            text: root.errorFor("audio")
            visible: text.length > 0
            color: Theme.currentTheme.colors.systemCriticalColor
            wrapMode: Text.WordWrap
        }

        RowLayout {
            Layout.fillWidth: true
            spacing: 12

            Text {
                Layout.preferredWidth: 80
                typography: Typography.Body
                text: qsTr("歌词")
                color: Theme.currentTheme.colors.textSecondaryColor
            }

            Item {
                Layout.fillWidth: true
                Layout.preferredHeight: 36

                ComboBox {
                    id: lyricsCombo
                    objectName: "lyricsCombo"
                    anchors.fill: parent
                    visible: root.lyricsCandidates.length > 0
                    model: root.lyricsCandidates
                    currentIndex: root.lyricsIndex
                    onActivated: function(index) {
                        if (index >= 0) Backend.setNumber("lyricsIndex", index)
                    }
                }

                Text {
                    anchors.left: parent.left
                    anchors.right: parent.right
                    anchors.verticalCenter: parent.verticalCenter
                    visible: root.lyricsCandidates.length === 0
                    typography: Typography.Body
                    color: Theme.currentTheme.colors.textSecondaryColor
                    text: root.project.audio ? qsTr("使用音频元数据") : qsTr("自动匹配")
                    elide: Text.ElideRight
                }
            }

            Button {
                objectName: "lyricsSelectButton"
                Layout.preferredHeight: 36
                Layout.preferredWidth: 76
                text: qsTr("选择…")
                onClicked: lyricsDialog.open()
            }

            ToolButton {
                objectName: "lyricsClearButton"
                Layout.preferredWidth: 32
                Layout.preferredHeight: 36
                icon.name: "ic_fluent_dismiss_20_regular"
                onClicked: Backend.selectAsset("lyrics", "")
            }
        }
        Text {
            Layout.fillWidth: true
            Layout.leftMargin: 92
            text: root.errorFor("lyrics")
            visible: text.length > 0
            color: Theme.currentTheme.colors.systemCriticalColor
            wrapMode: Text.WordWrap
        }
        Button {
            id: lyricsContentToggle
            objectName: "lyricsContentToggle"
            text: checked ? qsTr("收起歌词") : qsTr("查看歌词")
            Layout.leftMargin: 92
            checkable: true
            enabled: !!root.project.lyricsContent
        }
        ScrollableTextArea {
            objectName: "lyricsContentView"
            visible: lyricsContentToggle.checked
            Layout.fillWidth: true
            Layout.leftMargin: 92
            Layout.preferredHeight: 130
            implicitHeight: 130
            readOnly: true
            text: root.project.lyricsContent || ""
        }

        RowLayout {
            Layout.fillWidth: true
            spacing: 12

            Text {
                Layout.preferredWidth: 80
                typography: Typography.Body
                text: qsTr("封面")
                color: Theme.currentTheme.colors.textSecondaryColor
            }

            Item {
                Layout.fillWidth: true
                Layout.preferredHeight: 36

                ComboBox {
                    id: coverCombo
                    objectName: "coverCombo"
                    anchors.fill: parent
                    visible: root.coverCandidates.length > 0
                    model: root.coverCandidates.map(function(label, index) {
                        return index === 0 && root.cfg("embeddedCover", true) ? qsTr("使用内嵌封面") : label
                    })
                    currentIndex: root.coverIndex
                    onActivated: function(index) {
                        if (index >= 0) Backend.setNumber("coverIndex", index)
                    }
                }

                Text {
                    anchors.left: parent.left
                    anchors.right: parent.right
                    anchors.verticalCenter: parent.verticalCenter
                    visible: root.coverCandidates.length === 0
                    typography: Typography.Body
                    color: Theme.currentTheme.colors.textSecondaryColor
                    text: root.cfg("embeddedCover", true) ? qsTr("使用内嵌封面") : qsTr("不显示封面")
                    elide: Text.ElideRight
                }
            }

            Button {
                objectName: "coverSelectButton"
                Layout.preferredHeight: 36
                Layout.preferredWidth: 76
                text: qsTr("选择…")
                onClicked: coverDialog.open()
            }

            ToolButton {
                objectName: "coverClearButton"
                Layout.preferredWidth: 32
                Layout.preferredHeight: 36
                icon.name: "ic_fluent_dismiss_20_regular"
                onClicked: Backend.selectAsset("cover", "")
            }
        }
        Text {
            Layout.fillWidth: true
            Layout.leftMargin: 92
            text: root.errorFor("cover")
            visible: text.length > 0
            color: Theme.currentTheme.colors.systemCriticalColor
            wrapMode: Text.WordWrap
        }

        RowLayout {
            Layout.fillWidth: true
            spacing: 12

            Text {
                Layout.preferredWidth: 80
                typography: Typography.Body
                text: qsTr("背景")
                color: Theme.currentTheme.colors.textSecondaryColor
            }

            Text {
                Layout.fillWidth: true
                typography: Typography.Body
                text: root.project.background ? String(root.project.background) : qsTr("自动背景")
                color: root.project.background
                    ? Theme.currentTheme.colors.textColor
                    : Theme.currentTheme.colors.textSecondaryColor
                elide: Text.ElideMiddle
                maximumLineCount: 1
                wrapMode: Text.NoWrap
            }

            Button {
                objectName: "backgroundSelectButton"
                Layout.preferredHeight: 36
                Layout.preferredWidth: 76
                text: qsTr("选择…")
                onClicked: backgroundDialog.open()
            }

            ToolButton {
                objectName: "backgroundClearButton"
                Layout.preferredWidth: 32
                Layout.preferredHeight: 36
                icon.name: "ic_fluent_dismiss_20_regular"
                onClicked: Backend.selectAsset("background", "")
            }
        }
        Text {
            Layout.fillWidth: true
            Layout.leftMargin: 92
            text: root.errorFor("background")
            visible: text.length > 0
            color: Theme.currentTheme.colors.systemCriticalColor
            wrapMode: Text.WordWrap
        }


        RowLayout {
            Layout.fillWidth: true
            spacing: 12

            Text {
                Layout.preferredWidth: 80
                typography: Typography.Body
                text: qsTr("歌曲名称")
                color: Theme.currentTheme.colors.textSecondaryColor
            }

            TextField {
                id: titleField
                objectName: "titleField"
                Layout.fillWidth: true
                Layout.preferredHeight: 36
                text: root.project.title ? String(root.project.title) : ""
                placeholderText: qsTr("使用音频元数据")
                onTextChanged: {
                    if (activeFocus) {
                        Backend.setText("title", text)
                    }
                }
            }
        }

    }

    SectionCard {
        Layout.fillWidth: true
        visible: root.step === 2
        showHeading: false
        RowLayout {
            Layout.fillWidth: true
            spacing: 8

            Text {
                Layout.preferredWidth: 68
                typography: Typography.Body
                text: qsTr("输出文件")
                color: Theme.currentTheme.colors.textSecondaryColor
            }

            Text {
                Layout.fillWidth: true
                typography: Typography.Body
                text: root.project.output
                    ? String(root.project.output)
                    : qsTr("选择音频后自动生成")
                color: root.project.output
                    ? Theme.currentTheme.colors.textColor
                    : Theme.currentTheme.colors.textSecondaryColor
                elide: Text.ElideMiddle
                maximumLineCount: 1
                wrapMode: Text.NoWrap
            }

            Button {
                objectName: "outputSelectButton"
                Layout.preferredHeight: 32
                text: qsTr("选择…")
                onClicked: outputDialog.open()
            }
        }
        Text {
            Layout.fillWidth: true
            visible: text.length > 0
            text: root.errorFor("output")
            color: Theme.currentTheme.colors.systemCriticalColor
            wrapMode: Text.WordWrap
        }

        RowLayout {
            Layout.fillWidth: true
            spacing: 8

            Text {
                Layout.preferredWidth: 68
                typography: Typography.Body
                text: qsTr("尺寸")
                color: Theme.currentTheme.colors.textSecondaryColor
            }

            NumberField {
                label: ""
                suffix: qsTr("px")
                from: 320
                to: 7680
                value: root.cfg("width", 1920)
                controlObjectName: "widthSpin"
                onEdited: function(value) {
                    Backend.setNumber("width", value)
                }
            }

            Text {
                typography: Typography.Body
                color: Theme.currentTheme.colors.textSecondaryColor
                text: "×"
            }

            NumberField {
                label: ""
                suffix: qsTr("px")
                from: 180
                to: 4320
                value: root.cfg("height", 1080)
                controlObjectName: "heightSpin"
                onEdited: function(value) {
                    Backend.setNumber("height", value)
                }
            }

            Item {
                Layout.fillWidth: true
            }
        }
        Text {
            Layout.fillWidth: true
            text: root.errorFor("dimensions")
            visible: text.length > 0
            color: Theme.currentTheme.colors.systemCriticalColor
            wrapMode: Text.WordWrap
        }

        NumberField {
            Layout.fillWidth: true
            label: qsTr("帧率")
            suffix: qsTr("fps")
            from: 1
            to: 120
            value: root.cfg("fps", 30)
            controlObjectName: "fpsSpin"
            onEdited: function(value) {
                Backend.setNumber("fps", value)
            }
        }

        NumberField {
            Layout.fillWidth: true
            label: qsTr("预览间隔")
            suffix: qsTr("帧/次")
            from: 1
            to: 300
            value: root.cfg("previewInterval", 15)
            controlObjectName: "previewIntervalSpin"
            onEdited: function(value) {
                Backend.setNumber("previewInterval", value)
            }
        }

        RowLayout {
            Layout.fillWidth: true
            spacing: 8

            Text {
                Layout.preferredWidth: 68
                typography: Typography.Body
                text: qsTr("编码器")
                color: Theme.currentTheme.colors.textSecondaryColor
            }

            ComboBox {
                id: encoderCombo
                objectName: "encoderCombo"
                Layout.fillWidth: true
                model: root.defaults.encoders || []
                currentIndex: root.cfg("encoder", 0)
                onCurrentIndexChanged: root.refreshZeroCopy()
                onActivated: function(index) {
                    Backend.setNumber("encoder", index)
                }
            }
        }
        Text {
            Layout.fillWidth: true
            text: root.errorFor("encoder")
            visible: text.length > 0
            color: Theme.currentTheme.colors.systemCriticalColor
            wrapMode: Text.WordWrap
        }

        RowLayout {
            Layout.fillWidth: true
            spacing: 8

            Text {
                Layout.preferredWidth: 68
                typography: Typography.Body
                text: qsTr("格式")
                color: Theme.currentTheme.colors.textSecondaryColor
            }

            ComboBox {
                id: codecCombo
                objectName: "codecCombo"
                Layout.fillWidth: true
                model: root.defaults.codecs || []
                currentIndex: root.cfg("codec", 0)
                enabled: !root.zeroCopySelected
                onActivated: function(index) {
                    Backend.setNumber("codec", index)
                }
            }
        }

        RowLayout {
            Layout.fillWidth: true
            spacing: 8
            visible: root.zeroCopySelected

            Text {
                Layout.preferredWidth: 68
                typography: Typography.Body
                text: qsTr("NVENC")
                color: Theme.currentTheme.colors.textSecondaryColor
            }

            ComboBox {
                id: nvencCombo
                objectName: "nvencPresetCombo"
                Layout.fillWidth: true
                model: ["p1", "p2", "p3", "p4", "p5", "p6", "p7"]
                currentIndex: root.cfg("nvencPreset", 4)
                onActivated: function(index) {
                    Backend.setNumber("nvencPreset", index)
                }
            }
        }
        CheckBox {
            objectName: "includeAudioCheck"
            text: qsTr("输出音频")
            checked: root.cfg("includeAudio", true) === true
            onToggled: Backend.setFlag("includeAudio", checked)
        }

    }

    SectionCard {
        Layout.fillWidth: true
        visible: root.step === 2
        title: qsTr("渲染范围")
        ComboBox {
            objectName: "renderModeCombo"
            Layout.fillWidth: true
            model: [qsTr("完整渲染"), qsTr("范围采样（不输出音频）"), qsTr("随机采样（不输出音频）")]
            currentIndex: (root.project.wizard || {}).mode || 0
            onActivated: function(index) { Backend.setNumber("renderMode", index) }
        }

        NumberField {
            Layout.fillWidth: true
            label: qsTr("采样起点")
            suffix: qsTr("ms")
            from: 0
            to: 86400000
            stepSize: 1000
            value: root.cfg("sampleStart", 0)
            controlObjectName: "sampleStartSpin"
            onEdited: function(value) {
                Backend.setNumber("sampleStart", value)
            }
        }

        NumberField {
            Layout.fillWidth: true
            label: qsTr("采样时长")
            suffix: qsTr("ms")
            from: 1
            to: 86400000
            stepSize: 1000
            value: root.cfg("sampleDuration", 15000)
            controlObjectName: "sampleDurationSpin"
            onEdited: function(value) {
                Backend.setNumber("sampleDuration", value)
            }
        }
        Text {
            Layout.fillWidth: true
            text: root.errorFor("sample")
            visible: text.length > 0
            color: Theme.currentTheme.colors.systemCriticalColor
            wrapMode: Text.WordWrap
        }
    }

    SectionCard {
        Layout.fillWidth: true
        visible: root.step === 1
        showHeading: false
        GridLayout {
            Layout.fillWidth: true
            columns: 2
            columnSpacing: 12
            rowSpacing: 2

            CheckBox {
                objectName: "embeddedCoverCheck"
                Layout.fillWidth: true
                text: qsTr("使用内嵌封面")
                checked: root.cfg("embeddedCover", true) === true
                onToggled: Backend.setFlag("embeddedCover", checked)
            }


            CheckBox {
                objectName: "translationCheck"
                Layout.fillWidth: true
                text: qsTr("渲染翻译")
                checked: root.cfg("translation", true) === true
                onToggled: Backend.setFlag("translation", checked)
            }

            CheckBox {
                objectName: "backgroundVocalCheck"
                Layout.fillWidth: true
                text: qsTr("渲染伴唱")
                checked: root.cfg("backgroundVocal", true) === true
                onToggled: Backend.setFlag("backgroundVocal", checked)
            }
        }

        SliderRow {
            label: qsTr("字号")
            from: 50
            to: 200
            value: root.cfg("fontScale", 100)
            controlObjectName: "fontScaleSlider"
            onMoved: function(value) {
                Backend.setNumber("fontScale", Math.round(value))
            }
        }

        SliderRow {
            label: qsTr("翻译字号")
            from: 50
            to: 200
            value: root.cfg("translationFontScale", 65)
            controlObjectName: "translationFontScaleSlider"
            onMoved: function(value) {
                Backend.setNumber("translationFontScale", Math.round(value))
            }
        }

        SliderRow {
            label: qsTr("伴唱字号")
            from: 50
            to: 200
            value: root.cfg("backgroundFontScale", 70)
            controlObjectName: "backgroundFontScaleSlider"
            onMoved: function(value) {
                Backend.setNumber("backgroundFontScale", Math.round(value))
            }
        }

        SliderRow {
            label: qsTr("段内行高")
            from: 50
            to: 180
            value: root.cfg("lineHeight", 65)
            controlObjectName: "lineHeightSlider"
            onMoved: function(value) {
                Backend.setNumber("lineHeight", Math.round(value))
            }
        }

        SliderRow {
            label: qsTr("歌词行间距")
            from: 0
            to: 200
            value: root.cfg("lineSpacing", 80)
            controlObjectName: "lineSpacingSlider"
            onMoved: function(value) {
                Backend.setNumber("lineSpacing", Math.round(value))
            }
        }

        SliderRow {
            label: qsTr("翻译间距")
            from: 0
            to: 200
            value: root.cfg("translationGap", 100)
            controlObjectName: "translationGapSlider"
            onMoved: function(value) {
                Backend.setNumber("translationGap", Math.round(value))
            }
        }

        SliderRow {
            label: qsTr("伴唱间距")
            from: 0
            to: 200
            value: root.cfg("backgroundGap", 100)
            controlObjectName: "backgroundGapSlider"
            onMoved: function(value) {
                Backend.setNumber("backgroundGap", Math.round(value))
            }
        }

        SliderRow {
            label: qsTr("左右留白")
            from: 0
            to: 200
            value: root.cfg("horizontalPadding", 100)
            controlObjectName: "horizontalPaddingSlider"
            onMoved: function(value) {
                Backend.setNumber("horizontalPadding", Math.round(value))
            }
        }

        SliderRow {
            id: blurRow
            label: qsTr("模糊 Sigma 步长")
            suffix: ""
            from: 1
            to: 20
            value: root.cfg("blurStep", 6)
            controlObjectName: "blurStepSlider"
            hint: qsTr("最大模糊 %1 px / 行").arg(Math.round(blurRow.sliderValue * 7))
            onMoved: function(value) {
                Backend.setNumber("blurStep", Math.round(value))
            }
        }

        CheckBox {
            objectName: "debugOverlaysCheck"
            text: qsTr("调试框（容器/字形/行距）")
            checked: root.cfg("debugOverlays", false) === true
            onToggled: Backend.setFlag("debugOverlays", checked)
        }
    }

    FileDialog {
        id: audioDialog
        title: qsTr("选择音频")
        fileMode: FileDialog.OpenFile
        nameFilters: [
            qsTr("音频文件 (*.mp3 *.flac *.wav *.m4a *.aac *.ogg *.opus)"),
            qsTr("所有文件 (*)")
        ]
        onAccepted: Backend.selectAsset("audio", selectedFile)
    }

    FileDialog {
        id: lyricsDialog
        title: qsTr("选择歌词")
        fileMode: FileDialog.OpenFile
        nameFilters: [
            qsTr("歌词文件 (*.lrc *.ttml *.xml *.yrc)"),
            qsTr("所有文件 (*)")
        ]
        onAccepted: Backend.selectAsset("lyrics", selectedFile)
    }

    FileDialog {
        id: coverDialog
        title: qsTr("选择封面")
        fileMode: FileDialog.OpenFile
        nameFilters: [
            qsTr("图片文件 (*.jpg *.jpeg *.png *.webp *.bmp *.gif *.tif *.tiff *.avif *.heic *.heif)"),
            qsTr("所有文件 (*)")
        ]
        onAccepted: Backend.selectAsset("cover", selectedFile)
    }

    FileDialog {
        id: backgroundDialog
        title: qsTr("选择背景")
        fileMode: FileDialog.OpenFile
        nameFilters: [
            qsTr("图片文件 (*.jpg *.jpeg *.png *.webp *.bmp *.gif *.tif *.tiff *.avif *.heic *.heif)"),
            qsTr("所有文件 (*)")
        ]
        onAccepted: Backend.selectAsset("background", selectedFile)
    }

    FileDialog {
        id: outputDialog
        title: qsTr("选择输出文件")
        fileMode: FileDialog.SaveFile
        defaultSuffix: "mp4"
        nameFilters: [
            qsTr("MP4 视频 (*.mp4)"),
            qsTr("所有文件 (*)")
        ]
        onAccepted: Backend.selectAsset("output", selectedFile)
    }
}
