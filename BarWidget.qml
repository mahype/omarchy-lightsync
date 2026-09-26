import QtQuick
import Quickshell
import Quickshell.Io
import qs.Commons
import qs.Ui
import "Model.js" as Model

BarWidget {
  id: root
  moduleName: "io.github.mahype.omarchy-lightsync-hue"

  readonly property var service: bar && bar.shell && typeof bar.shell.serviceFor === "function"
    ? bar.shell.serviceFor(moduleName) : null
  readonly property var strings: Model.strings(Qt.locale().name)
  readonly property string level: Model.level(service)
  readonly property bool active: Model.isRunning(service)
  readonly property bool attention: level === "failed"
  readonly property bool opened: panelLoader.item ? panelLoader.item.opened === true : false
  readonly property bool popoutSwitchClosing: panelLoader.item
    ? panelLoader.item.popoutSwitchClosing === true : false

  function open() { if (panelLoader.item) panelLoader.item.open() }
  function close() { if (panelLoader.item) panelLoader.item.close() }
  function toggle() { if (panelLoader.item) panelLoader.item.toggle() }
  function closeForPopoutSwitch() {
    if (panelLoader.item) panelLoader.item.closeForPopoutSwitch()
  }

  function injectPanel() {
    var target = panelLoader.item
    if (!target) return
    if ("bar" in target) target.bar = root.bar
    if ("settings" in target) target.settings = root.settings
    if ("anchorItem" in target) target.anchorItem = button
    if ("hostWidget" in target) target.hostWidget = root
    if ("service" in target) target.service = root.service
  }

  visible: setting("showWhenIdle", true) === true || level !== "idle"
  implicitWidth: button.implicitWidth
  implicitHeight: button.implicitHeight
  onBarChanged: injectPanel()
  onServiceChanged: injectPanel()
  onSettingsChanged: injectPanel()

  Loader {
    id: panelLoader
    active: true
    source: Qt.resolvedUrl("Panel.qml")
    visible: false
    onLoaded: {
      root.injectPanel()
      Qt.callLater(root.injectPanel)
    }
  }

  IpcHandler {
    target: root.moduleName
    function open(): void { root.open() }
    function close(): void { root.close() }
    function show(): void { root.open() }
    function hide(): void { root.close() }
    function toggle(): void { root.toggle() }
    function refresh(): string {
      if (root.service) root.service.refresh()
      return "ok"
    }
    function start(): string { return root.service && root.service.startSync() ? "ok" : "unavailable" }
    function stop(): string { return root.service && root.service.stopSync() ? "ok" : "unavailable" }
    function toggleSync(): string { return root.service && root.service.toggleSync() ? "ok" : "unavailable" }
    function status(): string {
      var s = root.service
      if (!s) return "{}"
      return JSON.stringify({ state: s.syncState, level: root.level, paired: s.paired,
        bridge: s.bridge ? s.bridge.id : "", area: s.area, fps: Math.round(s.fps * 10) / 10, error: s.error })
    }
  }

  BarIconButton {
    id: button
    anchors.fill: parent
    bar: root.bar
    text: "󱍖"
    active: root.opened || root.active
    tooltipText: Model.tooltip(root.service, root.strings)
    onPressed: function(buttonCode) {
      if (buttonCode === Qt.RightButton) {
        if (root.service) root.service.toggleSync()
      } else if (buttonCode === Qt.LeftButton) root.toggle()
    }

    Rectangle {
      visible: root.active || root.attention
      width: Math.max(4, Style.space(5))
      height: width
      radius: width / 2
      color: root.attention ? (root.bar ? root.bar.urgent : Color.urgent)
        : (root.bar ? root.bar.barForeground : Color.foreground)
      anchors.right: parent.right
      anchors.top: parent.top
      anchors.margins: Style.space(2)

      SequentialAnimation on opacity {
        running: root.active && !root.attention
        loops: Animation.Infinite
        NumberAnimation { from: 1.0; to: 0.25; duration: 900 }
        NumberAnimation { from: 0.25; to: 1.0; duration: 900 }
      }
    }
  }
}
