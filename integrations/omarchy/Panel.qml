import QtQuick
import QtQuick.Controls
import Quickshell
import qs.Commons
import qs.Ui
import "Model.js" as Model

Panel {
  id: root
  moduleName: "io.github.mahype.omarchy-lightsync"
  ipcTarget: moduleName
  manageIpc: false

  property var anchorItem: null
  property var hostWidget: null
  property var service: null
  readonly property var barIdentity: hostWidget || root
  readonly property var strings: Model.strings(Qt.locale().name)
  readonly property var status: service ? service.status : null
  readonly property var config: service ? service.config : ({})
  readonly property var profiles: service ? service.profiles : []
  readonly property var actions: Model.panelActions(status, service ? service.installed : false,
    service ? service.busy : false)
  readonly property bool running: Model.isRunning(status)
  readonly property color foreground: bar ? bar.foreground : Color.foreground
  readonly property color urgent: bar ? bar.urgent : Color.urgent
  readonly property color dim: Qt.darker(foreground, 1.55)
  readonly property string fontFamily: bar ? bar.fontFamily : Style.font.family
  readonly property bool editing: profileField.activeFocus || modeDropdown.popupOpen
    || intensityDropdown.popupOpen || backendDropdown.popupOpen

  function open() {
    controller.show()
    if (service) service.refresh()
    Qt.callLater(function() { keyCatcher.forceActiveFocus() })
  }

  function close() {
    controller.hide()
  }

  function createProfile() {
    if (service && service.createProfile(profileField.text)) {
      profileField.text = ""
      keyCatcher.forceActiveFocus()
    }
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
            meta: Model.headline(root.status, root.service ? root.service.installed : false, root.strings)
            detail: Model.fps(root.status, root.strings)
            foreground: root.foreground
            fontFamily: root.fontFamily
            iconComponent: Component {
              Text {
                text: "󰌵"
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

          Button {
            width: parent.width
            text: root.running ? root.strings.stop : root.strings.start
            iconText: root.running ? "󰓛" : "󰐊"
            bordered: true
            focusable: true
            enabled: root.running ? root.actions.stop : root.actions.start
            foreground: root.running ? root.urgent : root.foreground
            fontFamily: root.fontFamily
            onClicked: if (root.service) root.running ? root.service.stopSync() : root.service.startSync()
          }

          PanelSeparator { foreground: root.foreground }

          Column {
            width: parent.width
            spacing: Style.space(8)
            PanelSectionHeader { text: root.strings.mode; foreground: root.foreground; fontFamily: root.fontFamily }
            Dropdown {
              id: modeDropdown
              width: parent.width
              showLabel: false
              value: String(root.config.mode || "video")
              options: [
                { value: "video", label: root.strings.video },
                { value: "game", label: root.strings.game }
              ]
              enabled: root.actions.mutate
              foreground: root.foreground
              fontFamily: root.fontFamily
              onChanged: function(value) {
                if (root.service && value !== root.config.mode) root.service.setConfig("mode", value)
              }
            }
            Text {
              visible: ["video", "game"].indexOf(String(root.config.mode || "video")) < 0
              width: parent.width
              text: root.strings.unsupportedMode
              color: root.urgent
              font.family: root.fontFamily
              font.pixelSize: Style.font.caption
              wrapMode: Text.WordWrap
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
                enabled: root.actions.mutate && Number(root.config.brightness || 0) > 0
                foreground: root.foreground
                fontFamily: root.fontFamily
                onClicked: root.service.setBrightness(Number(root.config.brightness || 0) - 5)
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
                enabled: root.actions.mutate && Number(root.config.brightness || 0) < 100
                foreground: root.foreground
                fontFamily: root.fontFamily
                onClicked: root.service.setBrightness(Number(root.config.brightness || 0) + 5)
              }
            }
          }

          Column {
            width: parent.width
            spacing: Style.space(8)
            PanelSectionHeader { text: root.strings.intensity; foreground: root.foreground; fontFamily: root.fontFamily }
            Dropdown {
              id: intensityDropdown
              width: parent.width
              showLabel: false
              value: String(root.config.intensity || "moderate")
              options: [
                { value: "subtle", label: root.strings.subtle },
                { value: "moderate", label: root.strings.moderate },
                { value: "high", label: root.strings.high },
                { value: "extreme", label: root.strings.extreme }
              ]
              enabled: root.actions.mutate
              foreground: root.foreground
              fontFamily: root.fontFamily
              onChanged: function(value) {
                if (root.service && value !== root.config.intensity) root.service.setConfig("intensity", value)
              }
            }
          }

          PanelSeparator { foreground: root.foreground }

          Column {
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
                Text { text: root.strings.bridge + ": " + (root.status && root.status.bridge.state === "ready" ? root.strings.connected : root.strings.notConfigured); color: root.foreground; font.family: root.fontFamily; font.pixelSize: Style.font.body }
                Text { text: root.strings.area + ": " + (root.status && root.status.area ? root.status.area.name : root.strings.notConfigured); color: root.dim; font.family: root.fontFamily; font.pixelSize: Style.font.bodySmall; elide: Text.ElideRight; width: parent.width }
                Button { visible: root.actions.setup; text: root.strings.openSetup; bordered: true; focusable: true; foreground: root.foreground; fontFamily: root.fontFamily; onClicked: root.service.openApp() }
              }
            }
          }

          PanelSeparator { foreground: root.foreground }

          Column {
            width: parent.width
            spacing: Style.space(8)
            PanelSectionHeader { text: root.strings.profiles; foreground: root.foreground; fontFamily: root.fontFamily }
            Row {
              width: parent.width
              spacing: Style.space(8)
              TextField {
                id: profileField
                width: parent.width - createButton.width - parent.spacing
                placeholderText: root.strings.newProfile
                foreground: root.foreground
                font.family: root.fontFamily
                enabled: root.actions.mutate
                onAccepted: root.createProfile()
                Keys.onEscapePressed: { text = ""; keyCatcher.forceActiveFocus() }
              }
              Button {
                id: createButton
                text: root.strings.create
                bordered: true
                focusable: true
                enabled: root.actions.mutate && profileField.text.trim() !== ""
                foreground: root.foreground
                fontFamily: root.fontFamily
                onClicked: root.createProfile()
              }
            }
            Text { visible: root.profiles.length === 0 && !(root.service && root.service.refreshing); width: parent.width; text: root.strings.noProfiles; color: root.dim; font.family: root.fontFamily; font.pixelSize: Style.font.body; horizontalAlignment: Text.AlignHCenter }
            Repeater {
              model: root.profiles
              BorderSurface {
                required property var modelData
                width: parent.width
                implicitHeight: profileBody.implicitHeight + Style.space(20)
                color: "transparent"
                borderSpec: Border.flat(modelData.active ? root.foreground : root.dim, 1)
                radius: Style.cornerRadius
                Column {
                  id: profileBody
                  anchors.left: parent.left
                  anchors.right: parent.right
                  anchors.verticalCenter: parent.verticalCenter
                  anchors.margins: Style.space(10)
                  spacing: Style.space(8)
                  Row {
                    width: parent.width
                    Text { width: parent.width - badge.width; text: modelData.name; color: root.foreground; font.family: root.fontFamily; font.pixelSize: Style.font.title; font.bold: true; elide: Text.ElideRight }
                    Text { id: badge; visible: modelData.active; text: root.strings.activeBadge; color: root.foreground; font.family: root.fontFamily; font.pixelSize: Style.font.caption; font.bold: true }
                  }
                  Row {
                    spacing: Style.space(6)
                    Button { text: root.strings.activate; bordered: true; focusable: true; enabled: root.actions.mutate && !modelData.active; foreground: root.foreground; fontFamily: root.fontFamily; onClicked: root.service.activateProfile(modelData.id) }
                    Button { text: root.strings.deleteLabel; bordered: true; focusable: true; enabled: root.actions.mutate; foreground: root.urgent; fontFamily: root.fontFamily; onClicked: root.service.deleteProfile(modelData.id) }
                  }
                }
              }
            }
          }

          PanelSeparator { foreground: root.foreground }

          Column {
            width: parent.width
            spacing: Style.space(10)
            PanelSectionHeader { text: root.strings.settings; foreground: root.foreground; fontFamily: root.fontFamily }
            Dropdown {
              id: backendDropdown
              width: parent.width
              label: root.strings.captureBackend
              value: String(root.config.backend || "portal-pipewire")
              options: [
                { value: "portal-pipewire", label: root.strings.portal },
                { value: "grim", label: root.strings.grim }
              ]
              enabled: root.actions.mutate
              foreground: root.foreground
              fontFamily: root.fontFamily
              onChanged: function(value) {
                if (root.service && value !== root.config.backend) root.service.setConfig("backend", value)
              }
            }
            Toggle {
              width: parent.width
              label: root.strings.restoreOnStop
              description: root.strings.restoreOnStopHint
              checked: root.config["restore-on-stop"] === true
              enabled: root.actions.mutate
              foreground: root.foreground
              fontFamily: root.fontFamily
              onClicked: if (root.service) root.service.setConfig("restore-on-stop", !checked)
            }
            Toggle {
              width: parent.width
              label: root.strings.autoStart
              description: root.strings.autoStartHint
              checked: root.config["auto-start"] === true
              enabled: root.actions.mutate
              foreground: root.foreground
              fontFamily: root.fontFamily
              onClicked: if (root.service) root.service.setConfig("auto-start", !checked)
            }
          }

          PanelSeparator { foreground: root.foreground }

          Column {
            width: parent.width
            spacing: Style.space(7)
            PanelSectionHeader { text: root.strings.diagnostics; foreground: root.foreground; fontFamily: root.fontFamily }
            Text { width: parent.width; text: root.strings.service + ": " + Model.stateLabel(root.status ? root.status.service.state : "", root.strings); color: root.foreground; font.family: root.fontFamily; font.pixelSize: Style.font.bodySmall }
            Text { width: parent.width; text: root.strings.capture + ": " + Model.stateLabel(root.status ? root.status.capture.state : "", root.strings); color: root.foreground; font.family: root.fontFamily; font.pixelSize: Style.font.bodySmall }
            Text { width: parent.width; text: root.strings.sync + ": " + Model.stateLabel(root.status ? root.status.sync.state : "", root.strings); color: root.foreground; font.family: root.fontFamily; font.pixelSize: Style.font.bodySmall }
            Text { width: parent.width; text: Model.fps(root.status, root.strings); color: root.dim; font.family: root.fontFamily; font.pixelSize: Style.font.caption }
          }

          Button {
            width: parent.width
            text: root.strings.openFullApp
            iconText: "󰋜"
            bordered: true
            focusable: true
            enabled: root.service && root.service.installed
            foreground: root.foreground
            fontFamily: root.fontFamily
            onClicked: root.service.openApp()
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
