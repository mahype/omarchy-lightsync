import QtQuick
import Quickshell
import Quickshell.Io
import "HueBridge.js" as Hue
import "SyncEngine.js" as Engine
import "ConfigStore.js" as ConfigStore
import "Model.js" as Model

// Owner of all Light Sync state. Mounted once per session; bar widgets and the
// panel reach it through `bar.shell.serviceFor(...)` and call only the action
// functions below.
//
// Everything runs on tools Omarchy already ships: curl for the bridge API
// (verified against the bundled Hue root CAs), secret-tool for credentials,
// openssl for the DTLS stream (tools/hue-stream.sh) and Quickshell's
// screencopy for capture.
Item {
  id: root

  property string omarchyPath: ""
  property var shell: null
  property var manifest: null

  readonly property string secretService: "io.github.mahype.omarchy-lightsync-hue"
  readonly property string configDir: (Quickshell.env("XDG_CONFIG_HOME") || (Quickshell.env("HOME") + "/.config")) + "/omarchy-lightsync-hue"
  readonly property string configPath: configDir + "/config.json"
  readonly property string caFile: localPath("certs/hue-ca-bundle.pem")
  readonly property string streamTool: localPath("tools/hue-stream.sh")
  readonly property var strings: Model.strings(Qt.locale().name)

  property var config: ConfigStore.defaults()
  property bool configLoaded: false
  readonly property var bridge: config.bridge
  readonly property string area: config.area

  // Credentials stay inside the service; they are never rendered or logged.
  property var credentials: null
  readonly property bool paired: credentials !== null

  property var bridges: []
  property bool bridgesLoaded: false
  property bool discovering: false
  property var pairingBridge: null
  property int pairingSecondsLeft: 0
  readonly property bool pairing: pairingBridge !== null
  property var areas: []
  property bool areasLoaded: false

  property string syncState: "idle"
  property real fps: 0
  property string error: ""
  property string statusMessage: ""
  readonly property bool busy: requestRunning || discovering || pairing
    || syncState === "starting" || syncState === "stopping"
  readonly property bool requestRunning: http.running || queue.length > 0

  property var queue: []
  property var current: null
  property var activeArea: null
  // True once this plugin activated the area; only then may it stop it.
  property bool areaStarted: false
  property var snapshot: []
  property var engineState: Engine.createState()
  property int sequence: 0
  property int framesSent: 0
  property real fpsSince: 0
  property bool autoStarted: false

  function localPath(relative) {
    return decodeURIComponent(String(Qt.resolvedUrl(relative)).replace(/^file:\/\//, ""))
  }

  function fail(code, detail) {
    error = Model.errorText(code, detail, strings)
    statusMessage = ""
  }

  // ---- Settings -------------------------------------------------------------

  function saveConfig(next) {
    config = ConfigStore.normalize(next)
    configFile.setText(ConfigStore.serialize(config))
  }

  function setConfig(key, value) {
    var next = ConfigStore.withSetting(config, String(key), value)
    if (!next) return false
    saveConfig(next)
    statusMessage = strings.updated
    return true
  }

  function setBrightness(value) {
    return setConfig("brightness", Math.max(0, Math.min(100, Math.round(Number(value)))))
  }

  function screenFor(name) {
    var screens = Quickshell.screens
    for (var i = 0; i < screens.length; i++) if (screens[i].name === name) return screens[i]
    return screens.length > 0 ? screens[0] : null
  }

  // ---- Bridge requests --------------------------------------------------------

  // One curl at a time; bridges rate-limit and nothing here is latency critical.
  function request(method, path, body, callback, target) {
    var text = Hue.curlConfig({
      bridge: target || bridge, method: method, path: path, body: body,
      key: target ? "" : (credentials ? credentials.applicationKey : ""), caFile: caFile
    })
    if (!text) { callback({ status: 0, body: null }); return }
    queue = queue.concat([{ config: text, callback: callback }])
    pump()
  }

  function clip(method, path, body, callback) {
    request(method, path, body, function(response) { callback(Hue.parseClip(response)) })
  }

  function pump() {
    if (http.running || queue.length === 0) return
    current = queue[0]
    queue = queue.slice(1)
    http.stdinEnabled = true
    http.running = true
  }

  function refresh() {
    error = ""
    if (paired) refreshAreas()
    return true
  }

  // ---- Discovery and pairing --------------------------------------------------

  function discoverBridges() {
    if (discovering) return false
    error = ""
    statusMessage = strings.discovering
    discovering = true
    bridges = []
    bridgesLoaded = false
    avahi.running = true
    return true
  }

  function finishDiscovery(found) {
    discovering = false
    statusMessage = ""
    bridges = found
    bridgesLoaded = true
    // Follow a paired bridge whose address changed through DHCP.
    for (var i = 0; bridge && i < found.length; i++) {
      if (found[i].id === bridge.id && found[i].host !== bridge.host)
        saveConfig(ConfigStore.withBridge(config, { id: bridge.id, host: found[i].host, name: bridge.name }))
    }
  }

  function useBridge(candidate) {
    if (!candidate || !Hue.normalizeBridgeId(candidate.id) || !Hue.isHost(candidate.host) || syncState !== "idle") return false
    error = ""
    statusMessage = ""
    pairingBridge = { id: Hue.normalizeBridgeId(candidate.id), host: candidate.host, name: candidate.name || "Hue Bridge" }
    pairingSecondsLeft = 30
    pairTimer.restart()
    attemptPairing()
    return true
  }

  function attemptPairing() {
    var target = pairingBridge
    if (!target) return
    request("POST", "/api", { devicetype: Hue.DEVICE_TYPE, generateclientkey: true }, function(response) {
      if (pairingBridge !== target) return
      var result = Hue.parsePairResponse(response)
      if (result.state === "paired") {
        pairingBridge = null
        pairTimer.stop()
        storeCredentials(target, result.credentials)
      } else if (result.state === "error") {
        pairingBridge = null
        pairTimer.stop()
        fail(result.error)
      }
      // "waiting": pairTimer tries again until the link button is pressed.
    }, target)
  }

  function cancelPairing() {
    pairingBridge = null
    pairTimer.stop()
  }

  function storeCredentials(target, value) {
    secretStore.target = target
    secretStore.value = value
    secretStore.command = ["secret-tool", "store", "--label", "Omarchy Light Sync for Hue (" + target.id + ")",
      "service", secretService, "bridge", target.id]
    secretStore.stdinEnabled = true
    secretStore.running = true
  }

  function loadCredentials() {
    credentials = null
    if (!bridge) return
    secretLookup.command = ["secret-tool", "lookup", "service", secretService, "bridge", bridge.id]
    secretLookup.running = true
  }

  function forgetBridge() {
    if (!bridge || syncState !== "idle") return false
    Quickshell.execDetached(["secret-tool", "clear", "service", secretService, "bridge", bridge.id])
    credentials = null
    areas = []
    areasLoaded = false
    saveConfig(ConfigStore.withBridge(config, null))
    statusMessage = strings.updated
    return true
  }

  // ---- Entertainment areas ------------------------------------------------------

  function refreshAreas() {
    if (!paired) return false
    clip("GET", "/clip/v2/resource/entertainment_configuration", undefined, function(result) {
      if (!result.ok) { fail(result.error); return }
      areas = Hue.parseAreas(result.data)
      areasLoaded = true
    })
    return true
  }

  function selectArea(id) {
    if (!Hue.isUuid(id) || syncState !== "idle") return false
    saveConfig(ConfigStore.withArea(config, id))
    statusMessage = strings.updated
    return true
  }

  // ---- Synchronization ------------------------------------------------------------

  function toggleSync() { return Model.isRunning(root) ? stopSync() : startSync() }

  function startSync() {
    if (!Model.canStart(root) || pairing) return false
    error = ""
    statusMessage = ""
    syncState = "starting"
    areaStarted = false
    var areaId = area
    clip("GET", "/clip/v2/resource/entertainment_configuration/" + areaId, undefined, function(result) {
      if (syncState !== "starting") return
      var found = result.ok ? Hue.parseArea(result.data[0]) : null
      if (!result.ok || !found) { abortStart(result.ok ? "areaMissing" : result.error); return }
      if (found.channels.length === 0) { abortStart("areaEmpty"); return }
      activeArea = found
      if (!found.active) { takeSnapshot(result.data[0]); return }
      // A session this plugin left behind (shell restart) may be taken over;
      // anyone else's stream is not interrupted.
      if (!found.streamer || found.streamer !== config.lastStreamer) { abortStart("areaBusy"); return }
      setAreaAction("stop", function(ok) { ok ? takeSnapshot(result.data[0]) : abortStart("areaBusy") })
    })
    return true
  }

  function takeSnapshot(areaResource) {
    snapshot = []
    if (!config.restoreOnStop) { activateArea(); return }
    clip("GET", "/clip/v2/resource/entertainment", undefined, function(entertainment) {
      clip("GET", "/clip/v2/resource/light", undefined, function(lights) {
        clip("GET", "/clip/v2/resource/zigbee_connectivity", undefined, function(connectivity) {
          if (syncState !== "starting") return
          if (!lights.ok) { abortStart(lights.error); return }
          snapshot = Hue.snapshot(areaResource, entertainment.data, lights.data, connectivity.data)
          activateArea()
        })
      })
    })
  }

  function activateArea() {
    setAreaAction("start", function(ok, code) {
      if (syncState !== "starting") return
      if (!ok) { abortStart(code); return }
      areaStarted = true
      engineState = Engine.createState()
      sequence = 0
      stream.command = ["bash", streamTool, bridge.host, credentials.applicationKey]
      stream.stdinEnabled = true
      stream.running = true
      streamReady.restart()
      rememberStreamer()
    })
  }

  function rememberStreamer() {
    clip("GET", "/clip/v2/resource/entertainment_configuration/" + activeArea.id, undefined, function(result) {
      var found = result.ok ? Hue.parseArea(result.data[0]) : null
      if (found && found.streamer && found.streamer !== config.lastStreamer)
        saveConfig(ConfigStore.withStreamer(config, found.streamer))
    })
  }

  function setAreaAction(action, callback) {
    clip("PUT", "/clip/v2/resource/entertainment_configuration/" + (activeArea ? activeArea.id : area),
      { action: action }, function(result) { callback(result.ok, result.error) })
  }

  function abortStart(code, detail) {
    fail(code, detail)
    cleanup()
  }

  function stopSync() {
    if (syncState !== "active" && syncState !== "starting") return false
    cleanup()
    return true
  }

  // Ends the stream, deactivates the area and restores the lights.
  function cleanup() {
    syncState = "stopping"
    streamReady.stop()
    if (stream.running) stream.stdinEnabled = false
    var lights = snapshot
    snapshot = []
    if (!areaStarted) { finishStop(); return }
    setAreaAction("stop", function() { restoreLights(lights, 0, 0) })
  }

  function restoreLights(lights, index, failures) {
    if (index >= lights.length) {
      if (failures > 0 && !error) fail("restore")
      finishStop()
      return
    }
    clip("PUT", "/clip/v2/resource/light/" + lights[index].id, lights[index].state, function(result) {
      restoreLights(lights, index + 1, failures + (result.ok ? 0 : 1))
    })
  }

  function finishStop() {
    activeArea = null
    areaStarted = false
    fps = 0
    syncState = "idle"
    if (stream.running) streamKill.restart()
  }

  function sendFrame(pixels, width, height) {
    if (syncState !== "active" || !activeArea) return
    var colors = Engine.process(engineState, pixels, width, height, activeArea.channels, config)
    var line = Hue.packetLine(activeArea.id, sequence, colors)
    sequence = (sequence + 1) & 0xff
    if (line) stream.write(line + "\n")
    framesSent++
    var now = Date.now()
    if (now - fpsSince >= 1000) {
      fps = framesSent * 1000 / (now - fpsSince)
      framesSent = 0
      fpsSince = now
    }
  }

  function maybeAutoStart() {
    if (autoStarted || !config.autoStart || !Model.canStart(root)) return
    autoStarted = true
    startSync()
  }

  // ---- Plumbing ---------------------------------------------------------------------

  Process {
    id: http
    command: ["curl", "-K", "-"]
    stdout: StdioCollector { id: httpOut; waitForEnd: true }
    stderr: StdioCollector { waitForEnd: true }
    onStarted: {
      write(root.current.config)
      stdinEnabled = false
    }
    onExited: {
      var done = root.current
      root.current = null
      var response = Hue.parseCurlOutput(httpOut.text)
      if (done) done.callback(response)
      Qt.callLater(root.pump)
    }
  }

  Process {
    id: avahi
    command: ["timeout", "4", "avahi-browse", "-rpt", "_hue._tcp"]
    stdout: StdioCollector { id: avahiOut; waitForEnd: true }
    onExited: {
      var found = Hue.parseAvahi(avahiOut.text)
      if (found.length > 0) root.finishDiscovery(found)
      else cloudDiscovery.running = true
    }
  }

  Process {
    id: cloudDiscovery
    command: ["curl", "-sS", "--max-time", "8", Hue.DISCOVERY_URL]
    stdout: StdioCollector { id: cloudOut; waitForEnd: true }
    onExited: root.finishDiscovery(Hue.parseCloudDiscovery(cloudOut.text))
  }

  Process {
    id: secretStore
    property var target: null
    property var value: null
    onStarted: {
      write(JSON.stringify(value))
      value = null
      stdinEnabled = false
    }
    onExited: function(exitCode) {
      var target_ = target
      target = null
      if (exitCode !== 0) { root.fail("secretStore"); return }
      root.saveConfig(ConfigStore.withBridge(root.config, target_))
      root.statusMessage = root.strings.updated
      root.loadCredentials()
    }
  }

  Process {
    id: secretLookup
    stdout: StdioCollector { id: secretOut; waitForEnd: true }
    onExited: function(exitCode) {
      root.credentials = exitCode === 0 ? Hue.parseCredentials(secretOut.text) : null
      if (!root.credentials && root.bridge) root.fail("noCredentials")
      else if (root.credentials) {
        root.refreshAreas()
        root.maybeAutoStart()
      }
    }
  }

  Process {
    id: stream
    stdout: SplitParser {
      onRead: function(line) {
        if (line !== "ready" || root.syncState !== "starting") return
        streamReady.stop()
        root.syncState = "active"
        root.framesSent = 0
        root.fpsSince = Date.now()
      }
    }
    stderr: StdioCollector { id: streamErr; waitForEnd: true }
    onStarted: {
      write(root.credentials.clientKey + "\n")
    }
    onExited: {
      streamKill.stop()
      if (root.syncState === "starting" || root.syncState === "active")
        root.abortStart("stream", streamErr.text || "exit")
    }
  }

  // The DTLS handshake must finish within ten seconds.
  Timer {
    id: streamReady
    interval: 10000
    onTriggered: if (root.syncState === "starting") root.abortStart("streamTimeout")
  }

  // Closing stdin ends the stream; this is only the fallback.
  Timer {
    id: streamKill
    interval: 3000
    onTriggered: if (stream.running) stream.signal(15)
  }

  Timer {
    id: pairTimer
    interval: 2000
    repeat: true
    onTriggered: {
      root.pairingSecondsLeft = Math.max(0, root.pairingSecondsLeft - 2)
      if (!root.pairing) stop()
      else if (root.pairingSecondsLeft <= 0) {
        root.cancelPairing()
        root.fail("pairTimeout")
      } else if (!http.running) root.attemptPairing()
    }
  }

  LazyLoader {
    active: root.syncState === "active"
    ScreenSampler {
      screen: root.screenFor(root.config.screen)
      running: root.syncState === "active"
      onFrame: function(pixels, width, height) { root.sendFrame(pixels, width, height) }
      onFailed: root.abortStart("capture")
    }
  }

  // FileView does not create parent directories.
  Process {
    id: configDirProc
    command: ["install", "-d", "-m", "700", root.configDir]
    running: true
    onExited: configFile.reload()
  }

  FileView {
    id: configFile
    path: root.configPath
    watchChanges: true
    printErrors: false
    onFileChanged: reload()
    onLoaded: root.applyConfig(ConfigStore.parse(text()))
    onLoadFailed: root.applyConfig(ConfigStore.defaults())
  }

  function applyConfig(next) {
    var bridgeChanged = JSON.stringify(next.bridge) !== JSON.stringify(config.bridge)
    config = next
    var first = !configLoaded
    configLoaded = true
    if (first || bridgeChanged) loadCredentials()
  }

  Component.onDestruction: {
    if (stream.running) stream.stdinEnabled = false
  }
}
