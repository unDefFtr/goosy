import QtQuick
import QtQuick.Layouts
import QtQuick.Dialogs
import RinUI
import "../js/ui.js" as Ui

ColumnLayout {
    id: root

    property var defaults: ({})
    property var project: ({})
    property int step: 0
    readonly property var issues: (root.project.wizard || {}).issues || []
    readonly property bool redo: (root.project.wizard || {}).redo === true
    function messageWithDetail(message) {
        var summary = Ui.messageText(message)
        return message && message.detail ? summary + "\n" + message.detail : summary
    }

    function errorFor(field) {
        return root.issues.filter(function(issue) { return issue.field === field })
            .map(function(issue) { return root.messageWithDetail(issue.message) }).join("\n")
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

    function syncSelections() {
        if (lyricsCombo && lyricsCombo.currentIndex !== root.lyricsIndex) {
            lyricsCombo.currentIndex = root.lyricsIndex
        }
        if (coverCombo && coverCombo.currentIndex !== root.coverIndex) {
            coverCombo.currentIndex = root.coverIndex
        }
        var encoder = root.cfg("encoder", 0)
        if (encoderCombo && encoderCombo.currentIndex !== encoder) encoderCombo.currentIndex = encoder
        var mode = (root.project.wizard || {}).mode || 0
        if (modeCombo && modeCombo.currentIndex !== mode) modeCombo.currentIndex = mode
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
            root.syncSelections()
            if (titleField && titleField.text !== (root.project.title || "")) {
                titleField.text = root.project.title ? String(root.project.title) : ""
            }
        }
    }

    onDefaultsChanged: {
        root.syncSelections()
        root.refreshZeroCopy()
    }

    Component.onCompleted: {
        root.syncSelections()
        root.refreshZeroCopy()
    }

    SectionCard {
        Layout.fillWidth: true
        visible: root.step === 0
        showHeading: false
        contentSpacing: 14

        Repeater {
            model: root.issues.filter(function(issue) { return issue.canAccept === true })
            Button {
                required property var modelData
                text: ({
                    audio: qsTr("Use current audio file"), lyrics: qsTr("Use current lyrics file"),
                    cover: qsTr("Use current cover image"), background: qsTr("Use current background image")
                })[modelData.field]
                onClicked: Backend.command("acceptAsset:" + modelData.field)
            }
        }

        RowLayout {
            Layout.fillWidth: true
            spacing: 12

            Text {
                Layout.preferredWidth: 80
                wrapMode: Text.WordWrap
                typography: Typography.Body
                text: qsTr("Audio")
                color: Theme.currentTheme.colors.textSecondaryColor
            }

            Text {
                Layout.fillWidth: true
                typography: Typography.Body
                textFormat: Text.PlainText
                text: root.project.audio ? String(root.project.audio) : qsTr("No audio selected")
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
                text: qsTr("Browse…")
                onClicked: audioDialog.open()
            }
            Item { Layout.preferredWidth: 32 }
        }
        Text {
            objectName: "materialStatus"
            Layout.fillWidth: true
            Layout.leftMargin: 92
            visible: !!root.project.notice
            text: root.messageWithDetail(root.project.notice)
            textFormat: Text.PlainText
            typography: Typography.Caption
            color: Theme.currentTheme.colors.textSecondaryColor
            wrapMode: Text.WordWrap
        }
        Text {
            Layout.fillWidth: true
            Layout.leftMargin: 92
            textFormat: Text.PlainText
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
                wrapMode: Text.WordWrap
                typography: Typography.Body
                text: qsTr("Lyrics")
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
                    model: root.lyricsCandidates.map(function(filename, index) {
                        return index === 0 ? qsTr("Embedded lyrics") : (filename || qsTr("Unknown filename"))
                    })
                    onModelChanged: Qt.callLater(root.syncSelections)
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
                    text: root.project.audio ? qsTr("Use audio metadata") : qsTr("Match automatically")
                    elide: Text.ElideRight
                }
            }

            Button {
                objectName: "lyricsSelectButton"
                Layout.preferredHeight: 36
                text: qsTr("Browse…")
                onClicked: lyricsDialog.open()
            }

            ToolButton {
                objectName: "lyricsClearButton"
                Layout.preferredWidth: 32
                Layout.preferredHeight: 36
                icon.name: "ic_fluent_dismiss_20_regular"
                Accessible.name: qsTr("Clear lyrics selection")
                onClicked: Backend.selectAsset("lyrics", "")
            }
        }
        Text {
            Layout.fillWidth: true
            Layout.leftMargin: 92
            textFormat: Text.PlainText
            text: root.errorFor("lyrics")
            visible: text.length > 0
            color: Theme.currentTheme.colors.systemCriticalColor
            wrapMode: Text.WordWrap
        }
        Button {
            id: lyricsContentToggle
            objectName: "lyricsContentToggle"
            text: checked ? qsTr("Hide lyrics") : qsTr("Show lyrics")
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
                wrapMode: Text.WordWrap
                typography: Typography.Body
                text: qsTr("Cover")
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
                    model: root.coverCandidates.map(function(filename, index) {
                        if (index !== 0) return filename || qsTr("Unknown filename")
                        return root.cfg("embeddedCover", true) ? qsTr("Use embedded cover") : qsTr("No external cover")
                    })
                    onModelChanged: Qt.callLater(root.syncSelections)
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
                    text: root.cfg("embeddedCover", true) ? qsTr("Use embedded cover") : qsTr("No cover")
                    elide: Text.ElideRight
                }
            }

            Button {
                objectName: "coverSelectButton"
                Layout.preferredHeight: 36
                text: qsTr("Browse…")
                onClicked: coverDialog.open()
            }

            ToolButton {
                objectName: "coverClearButton"
                Layout.preferredWidth: 32
                Layout.preferredHeight: 36
                icon.name: "ic_fluent_dismiss_20_regular"
                Accessible.name: qsTr("Clear cover selection")
                onClicked: Backend.selectAsset("cover", "")
            }
        }
        Text {
            Layout.fillWidth: true
            Layout.leftMargin: 92
            textFormat: Text.PlainText
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
                wrapMode: Text.WordWrap
                typography: Typography.Body
                text: qsTr("Background")
                color: Theme.currentTheme.colors.textSecondaryColor
            }

            Text {
                Layout.fillWidth: true
                typography: Typography.Body
                textFormat: Text.PlainText
                text: root.project.background ? String(root.project.background) : qsTr("Automatic background")
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
                text: qsTr("Browse…")
                onClicked: backgroundDialog.open()
            }

            ToolButton {
                objectName: "backgroundClearButton"
                Layout.preferredWidth: 32
                Layout.preferredHeight: 36
                icon.name: "ic_fluent_dismiss_20_regular"
                Accessible.name: qsTr("Clear background selection")
                onClicked: Backend.selectAsset("background", "")
            }
        }
        Text {
            Layout.fillWidth: true
            Layout.leftMargin: 92
            textFormat: Text.PlainText
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
                wrapMode: Text.WordWrap
                typography: Typography.Body
                text: qsTr("Song title")
                color: Theme.currentTheme.colors.textSecondaryColor
            }

            TextField {
                id: titleField
                objectName: "titleField"
                Layout.fillWidth: true
                Layout.preferredHeight: 36
                text: root.project.title ? String(root.project.title) : ""
                placeholderText: qsTr("Use audio metadata")
                onTextEdited: Backend.setText("title", text)
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
                Layout.preferredWidth: 88
                wrapMode: Text.WordWrap
                typography: Typography.Body
                text: qsTr("Output file")
                color: Theme.currentTheme.colors.textSecondaryColor
            }

            Text {
                Layout.fillWidth: true
                typography: Typography.Body
                textFormat: Text.PlainText
                text: root.project.output
                    ? String(root.project.output)
                    : qsTr("Generated when you select audio")
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
                text: qsTr("Browse…")
                onClicked: outputDialog.open()
            }
        }
        Text {
            Layout.fillWidth: true
            visible: text.length > 0
            textFormat: Text.PlainText
            text: root.errorFor("output")
            color: Theme.currentTheme.colors.systemCriticalColor
            wrapMode: Text.WordWrap
        }

        RowLayout {
            Layout.fillWidth: true
            spacing: 8

            Text {
                Layout.preferredWidth: 88
                wrapMode: Text.WordWrap
                typography: Typography.Body
                text: qsTr("Dimensions")
                color: Theme.currentTheme.colors.textSecondaryColor
            }

            NumberField {
                accessibleName: qsTr("Width")
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
                accessibleName: qsTr("Height")
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
            textFormat: Text.PlainText
            text: root.errorFor("dimensions")
            visible: text.length > 0
            color: Theme.currentTheme.colors.systemCriticalColor
            wrapMode: Text.WordWrap
        }

        NumberField {
            Layout.fillWidth: true
            label: qsTr("Frame rate")
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
            label: qsTr("Preview interval")
            suffix: qsTr("frames per preview")
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
                Layout.preferredWidth: 88
                wrapMode: Text.WordWrap
                typography: Typography.Body
                text: qsTr("Encoder")
                color: Theme.currentTheme.colors.textSecondaryColor
            }

            ComboBox {
                id: encoderCombo
                objectName: "encoderCombo"
                Layout.fillWidth: true
                model: root.cfg("encoders", []).map(function(value) { return Ui.encoderLabel(value) })
                onModelChanged: Qt.callLater(root.syncSelections)
                currentIndex: root.cfg("encoder", 0)
                onCurrentIndexChanged: root.refreshZeroCopy()
                onActivated: function(index) {
                    Backend.setNumber("encoder", index)
                }
            }
        }
        Text {
            Layout.fillWidth: true
            textFormat: Text.PlainText
            text: root.errorFor("encoder")
            visible: text.length > 0
            color: Theme.currentTheme.colors.systemCriticalColor
            wrapMode: Text.WordWrap
        }

        RowLayout {
            Layout.fillWidth: true
            spacing: 8

            Text {
                Layout.preferredWidth: 88
                wrapMode: Text.WordWrap
                typography: Typography.Body
                text: qsTr("Codec")
                color: Theme.currentTheme.colors.textSecondaryColor
            }

            ComboBox {
                id: codecCombo
                objectName: "codecCombo"
                Layout.fillWidth: true
                model: root.cfg("codecs", [])
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
                Layout.preferredWidth: 88
                wrapMode: Text.WordWrap
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
            text: qsTr("Include audio")
            checked: root.cfg("includeAudio", true) === true
            onToggled: Backend.setFlag("includeAudio", checked)
        }

    }

    SectionCard {
        Layout.fillWidth: true
        visible: root.step === 2
        title: qsTr("Render range")
        ComboBox {
            id: modeCombo
            objectName: "renderModeCombo"
            Layout.fillWidth: true
            model: [Ui.modeLabel(0), root.redo ? Ui.modeLabel(1) : qsTr("%1 (no audio)").arg(Ui.modeLabel(1)),
                    qsTr("%1 (no audio)").arg(Ui.modeLabel(2))]
            onModelChanged: Qt.callLater(root.syncSelections)
            currentIndex: (root.project.wizard || {}).mode || 0
            onActivated: function(index) { Backend.setNumber("renderMode", index) }
        }

        NumberField {
            enabled: !root.redo || (root.project.wizard || {}).mode === 1
            Layout.fillWidth: true
            label: qsTr("Sample start")
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
            enabled: !root.redo || (root.project.wizard || {}).mode === 1
            Layout.fillWidth: true
            label: qsTr("Sample duration")
            suffix: qsTr("ms")
            from: 1
            to: 86400000
            stepSize: 1000
            value: root.cfg("sampleDuration", 15000) ?? 15000
            controlObjectName: "sampleDurationSpin"
            onEdited: function(value) {
                Backend.setNumber("sampleDuration", value)
            }
        }
        Text {
            Layout.fillWidth: true
            wrapMode: Text.WordWrap
            visible: root.redo && (root.project.wizard || {}).mode === 1 && root.cfg("sampleDuration", null) === null
            text: qsTr("This range runs to the end. Set a duration to stop earlier.")
            color: Theme.currentTheme.colors.textSecondaryColor
        }
        Text {
            Layout.fillWidth: true
            textFormat: Text.PlainText
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
                text: qsTr("Use embedded cover")
                checked: root.cfg("embeddedCover", true) === true
                onToggled: Backend.setFlag("embeddedCover", checked)
            }


            CheckBox {
                objectName: "translationCheck"
                Layout.fillWidth: true
                text: qsTr("Render translations")
                checked: root.cfg("translation", true) === true
                onToggled: Backend.setFlag("translation", checked)
            }

            CheckBox {
                objectName: "backgroundVocalCheck"
                Layout.fillWidth: true
                text: qsTr("Render backing vocals")
                checked: root.cfg("backgroundVocal", true) === true
                onToggled: Backend.setFlag("backgroundVocal", checked)
            }
        }

        SliderRow {
            label: qsTr("Font size")
            from: 50
            to: 200
            value: root.cfg("fontScale", 100)
            controlObjectName: "fontScaleSlider"
            onMoved: function(value) {
                Backend.setNumber("fontScale", Math.round(value))
            }
        }

        SliderRow {
            label: qsTr("Translation font size")
            from: 50
            to: 200
            value: root.cfg("translationFontScale", 65)
            controlObjectName: "translationFontScaleSlider"
            onMoved: function(value) {
                Backend.setNumber("translationFontScale", Math.round(value))
            }
        }

        SliderRow {
            label: qsTr("Backing vocal font size")
            from: 50
            to: 200
            value: root.cfg("backgroundFontScale", 70)
            controlObjectName: "backgroundFontScaleSlider"
            onMoved: function(value) {
                Backend.setNumber("backgroundFontScale", Math.round(value))
            }
        }

        SliderRow {
            label: qsTr("Line height")
            from: 50
            to: 180
            value: root.cfg("lineHeight", 65)
            controlObjectName: "lineHeightSlider"
            onMoved: function(value) {
                Backend.setNumber("lineHeight", Math.round(value))
            }
        }

        SliderRow {
            label: qsTr("Lyric line spacing")
            from: 0
            to: 200
            value: root.cfg("lineSpacing", 80)
            controlObjectName: "lineSpacingSlider"
            onMoved: function(value) {
                Backend.setNumber("lineSpacing", Math.round(value))
            }
        }

        SliderRow {
            label: qsTr("Translation spacing")
            from: 0
            to: 200
            value: root.cfg("translationGap", 100)
            controlObjectName: "translationGapSlider"
            onMoved: function(value) {
                Backend.setNumber("translationGap", Math.round(value))
            }
        }

        SliderRow {
            label: qsTr("Backing vocal spacing")
            from: 0
            to: 200
            value: root.cfg("backgroundGap", 100)
            controlObjectName: "backgroundGapSlider"
            onMoved: function(value) {
                Backend.setNumber("backgroundGap", Math.round(value))
            }
        }

        SliderRow {
            label: qsTr("Side margins")
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
            label: qsTr("Blur σ step")
            suffix: ""
            from: 1
            to: 20
            value: root.cfg("blurStep", 6)
            controlObjectName: "blurStepSlider"
            hint: qsTr("Maximum blur: %1 px per line").arg(Math.round(blurRow.sliderValue * 7))
            onMoved: function(value) {
                Backend.setNumber("blurStep", Math.round(value))
            }
        }

        CheckBox {
            objectName: "debugOverlaysCheck"
            text: qsTr("Show debug outlines (containers, glyphs, and line spacing)")
            checked: root.cfg("debugOverlays", false) === true
            onToggled: Backend.setFlag("debugOverlays", checked)
        }
    }

    FileDialog {
        id: audioDialog
        title: qsTr("Select audio")
        fileMode: FileDialog.OpenFile
        nameFilters: [
            qsTr("Audio files (*.mp3 *.flac *.wav *.m4a *.aac *.ogg *.opus)"),
            qsTr("All files (*)")
        ]
        onAccepted: Backend.selectAsset("audio", selectedFile)
    }

    FileDialog {
        id: lyricsDialog
        title: qsTr("Select lyrics")
        fileMode: FileDialog.OpenFile
        nameFilters: [
            qsTr("Lyric files (*.lrc *.ttml *.xml *.yrc)"),
            qsTr("All files (*)")
        ]
        onAccepted: Backend.selectAsset("lyrics", selectedFile)
    }

    FileDialog {
        id: coverDialog
        title: qsTr("Select a cover image")
        fileMode: FileDialog.OpenFile
        nameFilters: [
            qsTr("Image files (*.jpg *.jpeg *.png *.webp *.bmp *.gif *.tif *.tiff *.avif *.heic *.heif)"),
            qsTr("All files (*)")
        ]
        onAccepted: Backend.selectAsset("cover", selectedFile)
    }

    FileDialog {
        id: backgroundDialog
        title: qsTr("Select a background image")
        fileMode: FileDialog.OpenFile
        nameFilters: [
            qsTr("Image files (*.jpg *.jpeg *.png *.webp *.bmp *.gif *.tif *.tiff *.avif *.heic *.heif)"),
            qsTr("All files (*)")
        ]
        onAccepted: Backend.selectAsset("background", selectedFile)
    }

    FileDialog {
        id: outputDialog
        title: qsTr("Select an output file")
        fileMode: FileDialog.SaveFile
        defaultSuffix: "mp4"
        nameFilters: [
            qsTr("MP4 video (*.mp4)"),
            qsTr("All files (*)")
        ]
        onAccepted: Backend.selectAsset("output", selectedFile)
    }
}
