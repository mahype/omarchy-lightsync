import QtQuick
import Quickshell
import Quickshell.Io
import "Model.js" as Model

// A single service owns the long-lived status stream for every monitor. It
// never starts synchronization; capture begins only after an explicit action.
Item {
  id: root

  property string omarchyPath: ""
  property var shell: null
  property var manifest: null
  property var status: null
  property bool installed: true
  property string error: ""
  readonly property bool watching: watcher.running
  readonly property bool actionRunning: action.running
  readonly property var strings: Model.strings(Qt.locale().name)

  function take(line) {
    var doc = Model.parse(line)
    if (!doc) return false
    root.status = doc
    root.installed = true
    root.error = ""
    return true
  }

  function openApp() {
    Quickshell.execDetached([
      "omarchy-launch-or-focus",
      "io.github.mahype.omarchylightsync",
      "uwsm-app -- lightsync-gui"
    ])
  }

  function control(verb) {
    if (!root.installed || !root.status || action.running) return
    action.command = ["lightsync", "sync", verb]
    action.running = true
  }

  function startSync() { root.control("start") }
  function stopSync() { root.control("stop") }
  function toggleSync() {
    if (!root.status) return
    if (Model.isRunning(root.status)) root.control("stop")
    else if (Model.canStart(root.status)) root.control("start")
  }

  Process {
    id: watcher
    // `sh` gives a dependable exit 127 when the optional package is absent.
    command: ["sh", "-c", "command -v lightsync >/dev/null 2>&1 || exit 127; exec lightsync status --watch --json"]
    stdout: SplitParser {
      onRead: function(line) { root.take(line) }
    }
    stderr: SplitParser {
      onRead: function(line) {
        var message = String(line || "").trim()
        if (message) root.error = message
      }
    }
    onExited: function(exitCode) {
      if (exitCode === 126 || exitCode === 127) {
        root.installed = false
        root.status = null
        root.error = ""
      } else if (!root.error) {
        root.error = Model.streamStopped(exitCode, root.strings)
      }
      restart.interval = root.installed ? 2000 : 10000
      restart.restart()
    }
  }

  Process {
    id: action
    stderr: StdioCollector { id: actionError; waitForEnd: true }
    onExited: function(exitCode) {
      if (exitCode !== 0) root.error = String(actionError.text || root.strings.commandFailed).trim()
    }
  }

  Timer {
    id: restart
    interval: 2000
    onTriggered: if (!watcher.running) watcher.running = true
  }

  Component.onCompleted: watcher.running = true
}
