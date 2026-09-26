import QtQuick
import QtQuick.Controls
import Quickshell
import qs.Commons
import qs.Ui
import "Model.js" as Model

Panel {
  id: root
  moduleName: "io.github.mahype.omarchy-lightsync-hue"
  ipcTarget: moduleName
  manageIpc: false

  property var anchorItem: null
  property var hostWidget: null
  property var service: null
  readonly property var barIdentity: hostWidget || root
  readonly property var strings: Model.strings(Qt.locale().name)
  readonly property var config: service ? service.config : ({})
  readonly property var bridges: service ? service.bridges : []
  readonly property var areas: service ? service.areas : []
  readonly property var screens: Quickshell.screens
  readonly property bool running: Model.isRunning(service)
  readonly property bool idle: service ? service.syncState === "idle" : false
  readonly property bool connected: service ? service.bridge !== null && service.paired : false
  readonly property color foreground: bar ? bar.foreground : Color.foreground
  readonly property color urgent: bar ? bar.urgent : Color.urgent
  readonly property color dim: Qt.darker(foreground, 1.55)
  readonly property string fontFamily: bar ? bar.fontFamily : Style.font.family
  readonly property bool editing: false

  function areaName() {
    var id = config.area || ""
    for (var i = 0; i < areas.length; i++) if (areas[i].id === id) return areas[i].name
    return id ? "…" : strings.notConfigured
  }

  function open() {
    controller.show()
    if (service) service.refresh()
    Qt.callLater(function() { keyCatcher.forceActiveFocus() })
  }

  function close() {
    controller.hide()
  }

  onOpenedChanged: if (opened) {
    panelFlick.contentY = 0
    if (service) service.refresh()
    Qt.callLater(function() { keyCatcher.forceActiveFocus() })
  }

  KeyboardPanel {
    id: panel
    anchorItem: root.anchorItem
    owner: root.barIdentity
    bar: root.bar
    open: root.opened
    focusTarget: keyCatcher
    contentWidth: panel.fittedContentWidth(Style.space(520))
    contentHeight: panel.fittedContentHeight(content.implicitHeight, Style.space(720))

    PanelKeyCatcher {
      id: keyCatcher
      anchors.fill: parent
      blocked: root.editing
      onCloseRequested: root.close()
      onTabRequested: function(direction) {
        if (root.bar && typeof root.bar.switchPanelFrom === "function")
          root.bar.switchPanelFrom(root.barIdentity, direction)
      }
      onTextKey: function(text) {
        if (text.toLowerCase() === "r" && root.service) root.service.refresh()
      }

      Flickable {
        id: panelFlick
        anchors.fill: parent
        contentWidth: width
        contentHeight: content.implicitHeight
        clip: true
        boundsBehavior: Flickable.StopAtBounds
        flickableDirection: Flickable.VerticalFlick
        interactive: contentHeight > height
        ScrollBar.vertical: ScrollBar { policy: ScrollBar.AsNeeded }

        Column {
          id: content
          width: panelFlick.width
          spacing: Style.space(14)

          PanelHero {
            width: parent.width
            title: root.strings.title
            meta: Model.headline(root.service, root.strings)
            detail: Model.fps(root.service, root.strings)
            foreground: root.foreground
            fontFamily: root.fontFamily
            iconComponent: Component {
              Text {
                text: "󱍖"
                color: root.foreground
                font.family: root.fontFamily
                font.pixelSize: Style.font.display
              }
            }
          }

          BorderSurface {
            visible: root.service && (root.service.error !== "" || root.service.statusMessage !== "")
            width: parent.width
            implicitHeight: feedback.implicitHeight + Style.space(18)
            color: "transparent"
            borderSpec: Border.flat(root.service && root.service.error !== "" ? root.urgent : root.dim, 1)
            radius: Style.cornerRadius
            Text {
              id: feedback
              anchors.left: parent.left
              anchors.right: parent.right
              anchors.verticalCenter: parent.verticalCenter
              anchors.margins: Style.space(9)
              text: root.service ? (root.service.error || root.service.statusMessage) : ""
              color: root.service && root.service.error !== "" ? root.urgent : root.dim
              font.family: root.fontFamily
              font.pixelSize: Style.font.caption
              wrapMode: Text.WordWrap
            }
          }

          BorderSurface {
            visible: root.service && root.service.configLoaded && !Model.isReady(root.service)
            width: parent.width
            implicitHeight: setupNotice.implicitHeight + Style.space(18)
            color: "transparent"
            borderSpec: Border.flat(root.urgent, 1)
            radius: Style.cornerRadius
            Column {
              id: setupNotice
              anchors.left: parent.left
              anchors.right: parent.right
              anchors.verticalCenter: parent.verticalCenter
              anchors.margins: Style.space(9)
              spacing: Style.space(7)
              Text { width: parent.width; text: root.strings.setupRequired; color: root.foreground; font.family: root.fontFamily; font.pixelSize: Style.font.body; wrapMode: Text.WordWrap }
              Button {
                width: parent.width
                text: root.strings.configureConnection
                bordered: true
                focusable: true
                foreground: root.foreground
                fontFamily: root.fontFamily
                onClicked: panelFlick.contentY = Math.max(0, connectionSection.y - Style.space(20))
              }
            }
          }

          Button {
            width: parent.width
            text: root.running ? root.strings.stop : root.strings.start
            iconText: root.running ? "󰓛" : "󰐊"
            bordered: true
            focusable: true
            enabled: root.running ? root.service.syncState !== "stopping" : Model.canStart(root.service)
            foreground: root.running ? root.urgent : root.foreground
            fontFamily: root.fontFamily
            onClicked: if (root.service) root.service.toggleSync()
          }

          PanelSeparator { foreground: root.foreground }

          Column {
            width: parent.width
            spacing: Style.space(8)
            PanelSectionHeader { text: root.strings.mode; foreground: root.foreground; fontFamily: root.fontFamily }
            Row {
              width: parent.width
              spacing: Style.space(6)
              Repeater {
                model: [
                  { value: "video", label: root.strings.video },
                  { value: "game", label: root.strings.game }
                ]
                Button {
                  required property var modelData
                  width: (parent.width - parent.spacing) / 2
                  text: modelData.label
                  selected: root.config.mode === modelData.value
                  bordered: true
                  focusable: true
                  enabled: !selected
                  foreground: root.foreground
                  fontFamily: root.fontFamily
                  onClicked: if (root.service) root.service.setConfig("mode", modelData.value)
                }
              }
            }
          }

          Column {
            width: parent.width
            spacing: Style.space(8)
            PanelSectionHeader { text: root.strings.brightness; foreground: root.foreground; fontFamily: root.fontFamily }
            Row {
              anchors.horizontalCenter: parent.horizontalCenter
              spacing: Style.space(8)
              Button {
                text: "−"
                bordered: true
                focusable: true
                enabled: Number(root.config.brightness) > 0
                foreground: root.foreground
                fontFamily: root.fontFamily
                onClicked: root.service.setBrightness(Number(root.config.brightness) - 5)
              }
              BorderSurface {
                width: Style.space(80)
                height: parent.children[0].height
                color: "transparent"
                borderSpec: Border.flat(root.dim, 1)
                radius: Style.cornerRadius
                Text {
                  anchors.centerIn: parent
                  text: String(root.config.brightness === undefined ? "–" : root.config.brightness) + "%"
                  color: root.foreground
                  font.family: root.fontFamily
                  font.pixelSize: Style.font.body
                  font.bold: true
                }
              }
              Button {
                text: "+"
                bordered: true
                focusable: true
                enabled: Number(root.config.brightness) < 100
                foreground: root.foreground
                fontFamily: root.fontFamily
                onClicked: root.service.setBrightness(Number(root.config.brightness) + 5)
              }
            }
          }

          Column {
            width: parent.width
            spacing: Style.space(8)
            PanelSectionHeader { text: root.strings.intensity; foreground: root.foreground; fontFamily: root.fontFamily }
            Row {
              width: parent.width
              spacing: Style.space(6)
              Repeater {
                model: [
                  { value: "subtle", label: root.strings.subtle },
                  { value: "moderate", label: root.strings.moderate },
                  { value: "high", label: root.strings.high },
                  { value: "extreme", label: root.strings.extreme }
                ]
                Button {
                  required property var modelData
                  width: (parent.width - parent.spacing * 3) / 4
                  text: modelData.label
                  selected: root.config.intensity === modelData.value
                  bordered: true
                  focusable: true
                  enabled: !selected
                  foreground: root.foreground
                  fontFamily: root.fontFamily
                  horizontalPadding: Style.space(4)
                  onClicked: if (root.service) root.service.setConfig("intensity", modelData.value)
                }
              }
            }
          }

          PanelSeparator { foreground: root.foreground }

          Column {
            width: parent.width
            spacing: Style.space(10)
            PanelSectionHeader { text: root.strings.settings; foreground: root.foreground; fontFamily: root.fontFamily }
            Column {
              visible: root.screens.length > 1
              width: parent.width
              spacing: Style.space(6)
              Text { text: root.strings.display; color: root.dim; font.family: root.fontFamily; font.pixelSize: Style.font.bodySmall }
              Repeater {
                model: root.screens
                Button {
                  required property var modelData
                  required property int index
                  width: parent.width
                  text: modelData.name + " · " + modelData.width + "×" + modelData.height
                  selected: root.config.screen === modelData.name || (root.config.screen === "" && index === 0)
                  bordered: true
                  focusable: true
                  enabled: !selected
                  foreground: root.foreground
                  fontFamily: root.fontFamily
                  onClicked: if (root.service) root.service.setConfig("screen", modelData.name)
                }
              }
            }
            Toggle {
              width: parent.width
              label: root.strings.restoreOnStop
              description: root.strings.restoreOnStopHint
              checked: root.config.restoreOnStop === true
              foreground: root.foreground
              fontFamily: root.fontFamily
              onClicked: if (root.service) root.service.setConfig("restoreOnStop", !checked)
            }
            Toggle {
              width: parent.width
              label: root.strings.autoStart
              description: root.strings.autoStartHint
              checked: root.config.autoStart === true
              foreground: root.foreground
              fontFamily: root.fontFamily
              onClicked: if (root.service) root.service.setConfig("autoStart", !checked)
            }
          }

          PanelSeparator { foreground: root.foreground }

          Column {
            id: connectionSection
            width: parent.width
            spacing: Style.space(8)
            PanelSectionHeader { text: root.strings.connection; foreground: root.foreground; fontFamily: root.fontFamily }
            BorderSurface {
              width: parent.width
              implicitHeight: connectionBody.implicitHeight + Style.space(20)
              color: "transparent"
              borderSpec: Border.flat(root.dim, 1)
              radius: Style.cornerRadius
              Column {
                id: connectionBody
                anchors.left: parent.left
                anchors.right: parent.right
                anchors.verticalCenter: parent.verticalCenter
                anchors.margins: Style.space(10)
                spacing: Style.space(7)
                Text {
                  width: parent.width
                  text: root.strings.bridge + ": " + (root.connected
                    ? root.service.bridge.name + " · " + root.service.bridge.host : root.strings.notConfigured)
                  color: root.foreground
                  font.family: root.fontFamily
                  font.pixelSize: Style.font.body
                  elide: Text.ElideRight
                }
                Text { visible: root.connected; text: root.strings.area + ": " + root.areaName(); color: root.dim; font.family: root.fontFamily; font.pixelSize: Style.font.bodySmall; elide: Text.ElideRight; width: parent.width }

                Button {
                  visible: !root.connected
                  width: parent.width
                  text: root.strings.discoverBridge
                  bordered: true
                  focusable: true
                  enabled: root.service && !root.service.discovering && !root.service.pairing
                  foreground: root.foreground
                  fontFamily: root.fontFamily
                  onClicked: root.service.discoverBridges()
                }
                Repeater {
                  model: root.connected ? [] : root.bridges
                  BorderSurface {
                    required property var modelData
                    width: connectionBody.width
                    implicitHeight: bridgeRow.implicitHeight + Style.space(14)
                    color: "transparent"
                    borderSpec: Border.flat(root.dim, 1)
                    radius: Style.cornerRadius
                    Row {
                      id: bridgeRow
                      anchors.left: parent.left
                      anchors.right: parent.right
                      anchors.verticalCenter: parent.verticalCenter
                      anchors.margins: Style.space(7)
                      spacing: Style.space(7)
                      Column {
                        width: parent.width - pairButton.width - parent.spacing
                        Text { width: parent.width; text: modelData.name || root.strings.bridge; color: root.foreground; font.family: root.fontFamily; font.pixelSize: Style.font.body; elide: Text.ElideRight }
                        Text { width: parent.width; text: modelData.host; color: root.dim; font.family: root.fontFamily; font.pixelSize: Style.font.caption; elide: Text.ElideRight }
                      }
                      Button { id: pairButton; text: root.strings.pair; bordered: true; focusable: true; enabled: root.service && !root.service.pairing; foreground: root.foreground; fontFamily: root.fontFamily; onClicked: root.service.useBridge(modelData) }
                    }
                  }
                }
                Text { visible: !root.connected && root.service && root.service.bridgesLoaded && root.bridges.length === 0; width: parent.width; text: root.strings.noBridges; color: root.dim; font.family: root.fontFamily; font.pixelSize: Style.font.caption; wrapMode: Text.WordWrap }
                Text {
                  visible: root.service && root.service.pairing
                  width: parent.width
                  text: root.strings.pressLink.replace("%1", String(root.service ? root.service.pairingSecondsLeft : 0))
                  color: root.foreground
                  font.family: root.fontFamily
                  font.pixelSize: Style.font.bodySmall
                  wrapMode: Text.WordWrap
                }
                Button {
                  visible: root.connected
                  width: parent.width
                  text: root.strings.refreshAreas
                  bordered: true
                  focusable: true
                  enabled: root.service && !root.service.requestRunning
                  foreground: root.foreground
                  fontFamily: root.fontFamily
                  onClicked: root.service.refreshAreas()
                }
                Repeater {
                  model: root.connected ? root.areas : []
                  Button {
                    required property var modelData
                    width: connectionBody.width
                    text: modelData.name + " · " + root.strings.lights.replace("%1", String(modelData.lights))
                    selected: root.config.area === modelData.id
                    bordered: true
                    focusable: true
                    enabled: root.idle && !selected
                    foreground: root.foreground
                    fontFamily: root.fontFamily
                    onClicked: root.service.selectArea(modelData.id)
                  }
                }
                Text { visible: root.connected && root.service.areasLoaded && root.areas.length === 0; width: parent.width; text: root.strings.noAreas; color: root.dim; font.family: root.fontFamily; font.pixelSize: Style.font.caption; wrapMode: Text.WordWrap }
                Button {
                  visible: root.service && root.service.bridge !== null
                  width: parent.width
                  text: root.strings.forgetBridge
                  bordered: true
                  focusable: true
                  enabled: root.idle
                  foreground: root.urgent
                  fontFamily: root.fontFamily
                  onClicked: root.service.forgetBridge()
                }
              }
            }
          }

          Text {
            width: parent.width
            text: root.strings.keyboardHint
            color: root.dim
            font.family: root.fontFamily
            font.pixelSize: Style.font.caption
            horizontalAlignment: Text.AlignHCenter
            wrapMode: Text.WordWrap
          }
        }
      }
    }
  }
}
