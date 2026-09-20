// Pure parsing, state, and localization helpers. I/O belongs in Service.qml.
var STRINGS = {
  en: {
    title: "LightSync", missing: "LightSync is not installed",
    missingHint: "Install LightSync; this widget reconnects automatically.",
    unavailable: "LightSync status is unavailable", idle: "Synchronization is off",
    active: "Synchronizing lights", starting: "Starting synchronization",
    stopping: "Stopping synchronization", recovering: "Recovering synchronization",
    failed: "Synchronization needs attention", setup: "LightSync needs setup",
    streamStopped: "Status stream stopped (exit %1); reconnecting",
    commandFailed: "LightSync command failed", refreshing: "Refreshing LightSync…",
    updated: "LightSync updated", leftClick: "Left click: open controls",
    startClick: "Right click: start synchronization",
    stopClick: "Right click: stop synchronization", fps: "%1 FPS",
    fpsUnavailable: "FPS unavailable", start: "Start synchronization",
    stop: "Stop synchronization", mode: "MODE", video: "Video", game: "Game",
    unsupportedMode: "The current mode is not supported by this panel.",
    brightness: "BRIGHTNESS", intensity: "INTENSITY", subtle: "Subtle",
    moderate: "Moderate", high: "High", extreme: "Extreme",
    connection: "CONNECTION", bridge: "Bridge", area: "Entertainment area",
    connected: "Connected", notConfigured: "Not configured", openSetup: "Open setup",
    profiles: "PROFILES", newProfile: "New profile name", create: "Create",
    noProfiles: "No profiles yet.", activeBadge: "ACTIVE", activate: "Activate",
    deleteLabel: "Delete", settings: "SETTINGS", captureBackend: "Capture backend",
    portal: "ScreenCast portal", grim: "Grim", restoreOnStop: "Restore lights on stop",
    restoreOnStopHint: "Return lights to their previous state after synchronization.",
    autoStart: "Start sync with the daemon", autoStartHint: "Requires reusable portal permission.",
    diagnostics: "DIAGNOSTICS", service: "Service", capture: "Capture", sync: "Sync",
    status: "Status", openFullApp: "Open full application", refresh: "Refresh",
    keyboardHint: "Esc close · Tab switch panel · R refresh", unknown: "Unknown",
    on: "On", off: "Off", stateIdle: "Idle", stateActive: "Active",
    stateReady: "Ready", stateStarting: "Starting", stateStopping: "Stopping",
    stateFailed: "Failed", stateRecovering: "Recovering", stateNeedsSetup: "Needs setup",
    stateUnreachable: "Unreachable", stateDiscovering: "Discovering", statePairing: "Pairing",
    stateConnecting: "Connecting", stateRequestingPermission: "Requesting permission"
  },
  de: {
    title: "LightSync", missing: "LightSync ist nicht installiert",
    missingHint: "Installiere LightSync; das Widget verbindet sich automatisch neu.",
    unavailable: "LightSync-Status ist nicht verfügbar", idle: "Synchronisierung ist aus",
    active: "Lampen werden synchronisiert", starting: "Synchronisierung wird gestartet",
    stopping: "Synchronisierung wird beendet", recovering: "Synchronisierung wird wiederhergestellt",
    failed: "Synchronisierung braucht Aufmerksamkeit", setup: "LightSync muss eingerichtet werden",
    streamStopped: "Statusstream beendet (Code %1); Verbindung wird wiederhergestellt",
    commandFailed: "LightSync-Befehl fehlgeschlagen", refreshing: "LightSync wird aktualisiert…",
    updated: "LightSync aktualisiert", leftClick: "Linksklick: Steuerung öffnen",
    startClick: "Rechtsklick: Synchronisierung starten",
    stopClick: "Rechtsklick: Synchronisierung stoppen", fps: "%1 FPS",
    fpsUnavailable: "FPS nicht verfügbar", start: "Synchronisierung starten",
    stop: "Synchronisierung stoppen", mode: "MODUS", video: "Video", game: "Spiel",
    unsupportedMode: "Der aktuelle Modus wird von diesem Panel nicht unterstützt.",
    brightness: "HELLIGKEIT", intensity: "INTENSITÄT", subtle: "Dezent",
    moderate: "Mittel", high: "Hoch", extreme: "Extrem",
    connection: "VERBINDUNG", bridge: "Bridge", area: "Entertainment-Bereich",
    connected: "Verbunden", notConfigured: "Nicht eingerichtet", openSetup: "Einrichtung öffnen",
    profiles: "PROFILE", newProfile: "Neuer Profilname", create: "Erstellen",
    noProfiles: "Noch keine Profile.", activeBadge: "AKTIV", activate: "Aktivieren",
    deleteLabel: "Löschen", settings: "EINSTELLUNGEN", captureBackend: "Aufnahme-Backend",
    portal: "ScreenCast-Portal", grim: "Grim", restoreOnStop: "Lampen beim Stop wiederherstellen",
    restoreOnStopHint: "Setzt die Lampen nach der Synchronisierung auf ihren vorherigen Zustand zurück.",
    autoStart: "Sync mit dem Dienst starten", autoStartHint: "Benötigt eine wiederverwendbare Portal-Freigabe.",
    diagnostics: "DIAGNOSE", service: "Dienst", capture: "Aufnahme", sync: "Sync",
    status: "Status", openFullApp: "Vollständige Anwendung öffnen", refresh: "Aktualisieren",
    keyboardHint: "Esc schließen · Tab Panel wechseln · R aktualisieren", unknown: "Unbekannt",
    on: "An", off: "Aus", stateIdle: "Inaktiv", stateActive: "Aktiv",
    stateReady: "Bereit", stateStarting: "Startet", stateStopping: "Wird beendet",
    stateFailed: "Fehlgeschlagen", stateRecovering: "Wird wiederhergestellt", stateNeedsSetup: "Einrichtung nötig",
    stateUnreachable: "Nicht erreichbar", stateDiscovering: "Suche läuft", statePairing: "Kopplung läuft",
    stateConnecting: "Verbindung wird hergestellt", stateRequestingPermission: "Berechtigung wird angefragt"
  }
}

function strings(localeName) {
  return String(localeName || "").toLowerCase().indexOf("de") === 0 ? STRINGS.de : STRINGS.en
}

function parse(line) {
  try {
    var doc = JSON.parse(String(line || "").trim())
    if (!doc || typeof doc !== "object" || typeof doc.protocol_version !== "number") return null
    if (!doc.service || !doc.bridge || !doc.capture || !doc.sync) return null
    if (typeof doc.sync.state !== "string") return null
    return doc
  } catch (error) { return null }
}

function parseConfig(raw) {
  var config = {}
  var recognized = 0
  var allowed = ["language", "backend", "mode", "intensity", "brightness", "audio-reactive",
    "restore-on-stop", "launch-at-login", "auto-start"]
  String(raw || "").split(/\r?\n/).forEach(function(line) {
    var separator = line.indexOf("=")
    if (separator < 1) return
    var key = line.slice(0, separator).trim().toLowerCase()
    if (allowed.indexOf(key) < 0) return
    var value = line.slice(separator + 1).trim()
    if (key === "brightness") {
      var number = Number(value)
      if (isFinite(number)) config.brightness = Math.max(0, Math.min(100, Math.round(number)))
    } else if (["audio-reactive", "restore-on-stop", "launch-at-login", "auto-start"].indexOf(key) >= 0) {
      config[key] = value.toLowerCase() === "true"
    } else config[key] = value
    recognized++
  })
  return { ok: recognized > 0, config: config }
}

function parseProfiles(raw) {
  var profiles = []
  var source = String(raw || "").replace(/\r/g, "")
  var text = source.trim()
  if (text === "" || /^No profiles configured\.?$/i.test(text)) return { ok: true, profiles: profiles }
  var invalid = false
  source.replace(/\n+$/, "").split("\n").forEach(function(line) {
    if (!line.trim()) return
    var match = line.match(/^([* ])\s*([^\t]+)\t(.*)$/)
    if (!match || !match[2].trim() || !match[3].trim()) { invalid = true; return }
    profiles.push({ id: match[2].trim(), name: match[3].trim(), active: match[1] === "*" })
  })
  return { ok: !invalid, profiles: profiles }
}

function level(doc, installed) {
  if (!installed) return "missing"
  if (!doc) return "unavailable"
  var sync = doc.sync.state
  if (sync === "active") return "active"
  if (["starting", "stopping", "recovering"].indexOf(sync) >= 0) return sync
  if (["failed", "area_busy", "area_invalid"].indexOf(sync) >= 0
      || doc.service.state === "failed" || doc.service.state === "degraded"
      || doc.bridge.state === "unreachable" || doc.bridge.state === "auth_failed") return "failed"
  if (doc.bridge.state !== "ready" || !doc.area) return "setup"
  return "idle"
}

function isRunning(doc) {
  return !!(doc && doc.sync && ["active", "starting", "stopping", "recovering"].indexOf(doc.sync.state) >= 0)
}

function canStart(doc) {
  if (!doc || !doc.sync || !doc.service || !doc.bridge || !doc.area) return false
  if (doc.service.state === "failed" || doc.bridge.state !== "ready") return false
  return ["idle", "failed", "area_busy", "area_invalid"].indexOf(doc.sync.state) >= 0
}

function panelActions(doc, installed, busy) {
  return {
    start: installed === true && !busy && canStart(doc),
    stop: installed === true && !busy && isRunning(doc),
    mutate: installed === true && !busy,
    setup: installed === true && !busy && level(doc, installed) === "setup"
  }
}

function headline(doc, installed, s) {
  var key = level(doc, installed)
  return s[key] || s.unavailable
}

function detail(doc) {
  if (!doc) return ""
  var components = [doc.sync, doc.capture, doc.bridge, doc.service]
  for (var i = 0; i < components.length; i++)
    if (components[i] && components[i].detail) return String(components[i].detail)
  var names = []
  if (doc.area && doc.area.name) names.push(String(doc.area.name))
  if (doc.profile && doc.profile.name) names.push(String(doc.profile.name))
  return names.join(" · ")
}

function fps(doc, s) {
  var value = doc && doc.details ? Number(doc.details.frames_per_second) : NaN
  return isFinite(value) ? s.fps.replace("%1", value.toFixed(1)) : s.fpsUnavailable
}

function stateLabel(value, s) {
  var state = String(value || "").replace(/_/g, " ")
  if (!state) return s.unknown
  var key = "state" + state.split(" ").map(function(part) {
    return part.charAt(0).toUpperCase() + part.slice(1)
  }).join("")
  if (s[key]) return s[key]
  return state.charAt(0).toUpperCase() + state.slice(1)
}

function tooltip(doc, installed, error, s) {
  var lines = ["LightSync · " + headline(doc, installed, s)]
  var extra = !installed ? s.missingHint : detail(doc)
  if (!extra && error) extra = String(error).split("\n")[0]
  if (extra) lines.push(extra)
  lines.push(s.leftClick)
  if (doc && installed && isRunning(doc)) lines.push(s.stopClick)
  else if (doc && installed && canStart(doc)) lines.push(s.startClick)
  return lines.join("\n")
}

function compactError(raw, fallback) {
  var text = String(raw || "").trim().replace(/^lightsync:\s*/i, "")
  return (text || fallback || "LightSync command failed").split(/\r?\n/)[0]
}

function streamStopped(exitCode, s) { return s.streamStopped.replace("%1", String(exitCode)) }

if (typeof module !== "undefined") module.exports = {
  STRINGS: STRINGS, strings: strings, parse: parse, parseConfig: parseConfig,
  parseProfiles: parseProfiles, level: level, isRunning: isRunning, canStart: canStart,
  panelActions: panelActions, headline: headline, detail: detail, fps: fps,
  stateLabel: stateLabel, tooltip: tooltip, compactError: compactError,
  streamStopped: streamStopped
}
