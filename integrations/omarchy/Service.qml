import QtQuick
import Quickshell
import Quickshell.Io
import "Model.js" as Model

// Shared by every monitor. This is the only owner of the status watch process.
Item {
  id: root

  property string omarchyPath: ""
  property var shell: null
  property var manifest: null
  property var status: null
  property var config: ({})
  property var profiles: []
  property bool installed: true
  property string streamError: ""
  property string lastError: ""
  property string statusMessage: ""
  readonly property string error: lastError || streamError
  readonly property bool watching: watcher.running
  readonly property bool actionRunning: actionProcess.running
  readonly property bool refreshing: configProcess.running || profileProcess.running
  readonly property bool busy: actionRunning
  readonly property var strings: Model.strings(Qt.locale().name)

  function take(line) {
    var doc = Model.parse(line)
    if (!doc) return false
    status = doc
    installed = true
    streamError = ""
    return true
  }

  function openApp() {
    Quickshell.execDetached([
      "omarchy-launch-or-focus",
      "io.github.mahype.omarchylightsync",
      "uwsm-app -- lightsync-gui"
    ])
  }

  function refresh(preserveFeedback) {
    if (!installed) return false
    if (preserveFeedback !== true) {
      lastError = ""
      statusMessage = strings.refreshing
    }
    if (!configProcess.running) configProcess.running = true
    if (!profileProcess.running) profileProcess.running = true
    return true
  }

  function runAction(args) {
    if (!installed || actionProcess.running) return false
    lastError = ""
    statusMessage = ""
    actionProcess.command = ["lightsync"].concat(args)
    actionProcess.running = true
    return true
  }

  function control(verb) {
    if (!status) return false
    if (verb === "start" && !Model.canStart(status)) return false
    if (verb === "stop" && !Model.isRunning(status)) return false
    return runAction(["sync", verb])
  }

  function startSync() { return control("start") }
  function stopSync() { return control("stop") }
  function toggleSync() {
    if (!status) return false
    return Model.isRunning(status) ? stopSync() : startSync()
  }

  function setConfig(key, value) {
    var keys = ["backend", "mode", "intensity", "brightness", "restore-on-stop", "auto-start"]
    if (keys.indexOf(String(key)) < 0) return false
    return runAction(["config", "set", String(key), String(value)])
  }

  function setBrightness(value) {
    var next = Math.max(0, Math.min(100, Math.round(Number(value))))
    return setConfig("brightness", next)
  }

  function createProfile(name) {
    var value = String(name || "").trim()
    return value !== "" && runAction(["profile", "create", value])
  }

  function deleteProfile(id) {
    var value = String(id || "").trim()
    return value !== "" && runAction(["profile", "delete", value])
  }

  function activateProfile(id) {
    var value = String(id || "").trim()
    return value !== "" && runAction(["profile", "activate", value])
  }

  Process {
    id: watcher
    // `sh` provides a reliable exit 127 when the optional CLI is absent.
    command: ["sh", "-c", "command -v lightsync >/dev/null 2>&1 || exit 127; exec lightsync status --watch --json"]
    stdout: SplitParser { onRead: function(line) { root.take(line) } }
    stderr: SplitParser {
      onRead: function(line) {
        var message = Model.compactError(line, "")
        if (message) root.streamError = message
      }
    }
    onExited: function(exitCode) {
      if (exitCode === 126 || exitCode === 127) {
        root.installed = false
        root.status = null
        root.streamError = ""
        root.lastError = ""
        root.statusMessage = ""
      } else if (!root.streamError) root.streamError = Model.streamStopped(exitCode, root.strings)
      restart.interval = root.installed ? 2000 : 10000
      restart.restart()
    }
  }

  Process {
    id: configProcess
    command: ["lightsync", "config", "show"]
    stdout: StdioCollector { id: configOutput; waitForEnd: true }
    stderr: StdioCollector { id: configError; waitForEnd: true }
    onExited: function(exitCode) {
      if (exitCode !== 0) root.lastError = Model.compactError(configError.text || configOutput.text, root.strings.commandFailed)
      else {
        var result = Model.parseConfig(configOutput.text)
        if (result.ok) root.config = result.config
        else root.lastError = root.strings.commandFailed
      }
      if (!root.refreshing && root.statusMessage === root.strings.refreshing) root.statusMessage = ""
    }
  }

  Process {
    id: profileProcess
    command: ["lightsync", "profile", "list"]
    stdout: StdioCollector { id: profileOutput; waitForEnd: true }
    stderr: StdioCollector { id: profileError; waitForEnd: true }
    onExited: function(exitCode) {
      if (exitCode !== 0) root.lastError = Model.compactError(profileError.text || profileOutput.text, root.strings.commandFailed)
      else {
        var result = Model.parseProfiles(profileOutput.text)
        if (result.ok) root.profiles = result.profiles
        else root.lastError = root.strings.commandFailed
      }
      if (!root.refreshing && root.statusMessage === root.strings.refreshing) root.statusMessage = ""
    }
  }

  Process {
    id: actionProcess
    command: []
    stdout: StdioCollector { id: actionOutput; waitForEnd: true }
    stderr: StdioCollector { id: actionErrorOutput; waitForEnd: true }
    onExited: function(exitCode) {
      if (exitCode === 0) {
        root.lastError = ""
        root.statusMessage = root.strings.updated
      } else {
        root.lastError = Model.compactError(actionErrorOutput.text || actionOutput.text, root.strings.commandFailed)
        root.statusMessage = ""
      }
      root.refresh(true)
    }
  }

  Timer {
    id: restart
    interval: 2000
    onTriggered: if (!watcher.running) watcher.running = true
  }

  Component.onCompleted: {
    watcher.running = true
    refresh()
  }
}
