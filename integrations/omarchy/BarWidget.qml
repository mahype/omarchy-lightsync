import QtQuick
import Quickshell
import qs.Commons
import qs.Ui
import "Model.js" as Model

BarWidget {
  id: root
  moduleName: "io.github.mahype.omarchy-lightsync"

  property var service: null

  function bindService() {
    if (root.service || !root.bar || !root.bar.shell || typeof root.bar.shell.serviceFor !== "function") return
    var found = root.bar.shell.serviceFor(root.moduleName)
    if (found) root.service = found
  }

  onBarChanged: bindService()

  Timer {
    interval: 250
    repeat: true
    running: root.service === null
    onTriggered: root.bindService()
  }

  readonly property var status: service ? service.status : null
  readonly property bool installed: service ? service.installed : true
  readonly property string error: service ? service.error : ""
  readonly property var strings: Model.strings(Qt.locale().name)
  readonly property string level: Model.level(status, installed)
  readonly property bool active: level === "active" || level === "starting" || level === "recovering"
  readonly property bool attention: level === "failed" || level === "unavailable"
  readonly property color foreground: bar ? bar.barForeground : Color.foreground
  readonly property color urgentColor: bar ? bar.urgent : Color.urgent
  readonly property string fontFamily: bar ? bar.fontFamily : Style.font.family

  visible: setting("showWhenIdle", true) === true || level !== "idle"
  implicitWidth: vertical ? barSize : Style.bar.statusSlot
  implicitHeight: vertical ? Style.bar.iconSlot : barSize

  WidgetButton {
    anchors.fill: parent
    bar: root.bar
    labelVisible: false
    hasVisualContent: true
    tooltipText: Model.tooltip(root.status, root.installed, root.error, root.strings)

    onPressed: function(button) {
      if (!root.service) return
      if (button === Qt.RightButton) root.service.toggleSync()
      else if (button === Qt.LeftButton) root.service.openApp()
    }

    OpticalGlyph {
      id: glyph
      anchors.centerIn: parent
      width: Style.bar.iconCanvas
      height: Style.bar.iconCanvas
      text: root.installed ? "󰌵" : "󰌶"
      fontFamily: root.fontFamily
      fontSize: Style.bar.iconFont
      color: root.attention ? root.urgentColor : root.foreground
      opacity: root.level === "idle" || root.level === "setup" || root.level === "missing" ? 0.55 : 1.0
    }

    Rectangle {
      visible: root.active
      width: Math.max(4, Style.space(5))
      height: width
      radius: width / 2
      color: root.foreground
      anchors.right: glyph.right
      anchors.top: glyph.top

      SequentialAnimation on opacity {
        running: root.active
        loops: Animation.Infinite
        NumberAnimation { from: 1.0; to: 0.25; duration: 900 }
        NumberAnimation { from: 0.25; to: 1.0; duration: 900 }
      }
    }
  }
}
