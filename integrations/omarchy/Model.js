// Pure status and localization helpers. Keeping I/O out of this file makes the
// bar behavior testable without an Omarchy session.
var STRINGS = {
  en: {
    missing: "LightSync is not installed",
    missingHint: "Install the LightSync package; the widget will reconnect automatically.",
    unavailable: "LightSync status is unavailable",
    idle: "Synchronization is off",
    active: "Synchronizing lights",
    starting: "Starting synchronization",
    stopping: "Stopping synchronization",
    recovering: "Recovering synchronization",
    failed: "Synchronization needs attention",
    setup: "LightSync needs setup",
    streamStopped: "LightSync status stream stopped (exit %1); reconnecting",
    commandFailed: "LightSync command failed",
    leftClick: "Left click: open LightSync",
    startClick: "Right click: start synchronization",
    stopClick: "Right click: stop synchronization"
  },
  de: {
    missing: "LightSync ist nicht installiert",
    missingHint: "Installiere das LightSync-Paket; das Widget verbindet sich danach automatisch.",
    unavailable: "LightSync-Status ist nicht verfügbar",
    idle: "Synchronisierung ist aus",
    active: "Lampen werden synchronisiert",
    starting: "Synchronisierung wird gestartet",
    stopping: "Synchronisierung wird beendet",
    recovering: "Synchronisierung wird wiederhergestellt",
    failed: "Synchronisierung braucht Aufmerksamkeit",
    setup: "LightSync muss eingerichtet werden",
    streamStopped: "LightSync-Statusstream wurde beendet (Code %1); Verbindung wird wiederhergestellt",
    commandFailed: "LightSync-Befehl fehlgeschlagen",
    leftClick: "Linksklick: LightSync öffnen",
    startClick: "Rechtsklick: Synchronisierung starten",
    stopClick: "Rechtsklick: Synchronisierung stoppen"
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
  } catch (e) {
    return null
  }
}

function level(doc, installed) {
  if (!installed) return "missing"
  if (!doc) return "unavailable"
  var sync = doc.sync.state
  if (sync === "active") return "active"
  if (sync === "starting" || sync === "stopping" || sync === "recovering") return sync
  if (sync === "failed" || sync === "area_busy" || sync === "area_invalid"
      || doc.service.state === "failed" || doc.service.state === "degraded"
      || doc.bridge.state === "unreachable" || doc.bridge.state === "auth_failed") return "failed"
  if (doc.bridge.state !== "ready" || !doc.area) return "setup"
  return "idle"
}

function isRunning(doc) {
  if (!doc || !doc.sync) return false
  return ["active", "starting", "stopping", "recovering"].indexOf(doc.sync.state) !== -1
}

function canStart(doc) {
  if (!doc || !doc.sync || !doc.service || !doc.bridge || !doc.area) return false
  if (doc.service.state === "failed" || doc.bridge.state !== "ready") return false
  return ["idle", "failed", "area_busy", "area_invalid"].indexOf(doc.sync.state) !== -1
}

function headline(doc, installed, s) {
  switch (level(doc, installed)) {
  case "missing": return s.missing
  case "active": return s.active
  case "starting": return s.starting
  case "stopping": return s.stopping
  case "recovering": return s.recovering
  case "failed": return s.failed
  case "setup": return s.setup
  case "idle": return s.idle
  default: return s.unavailable
  }
}

function detail(doc) {
  if (!doc) return ""
  var components = [doc.sync, doc.capture, doc.bridge, doc.service]
  for (var i = 0; i < components.length; i++) {
    if (components[i] && components[i].detail) return String(components[i].detail)
  }
  var names = []
  if (doc.area && doc.area.name) names.push(String(doc.area.name))
  if (doc.profile && doc.profile.name) names.push(String(doc.profile.name))
  return names.join(" · ")
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

function streamStopped(exitCode, s) {
  return s.streamStopped.replace("%1", String(exitCode))
}

if (typeof module !== "undefined") {
  module.exports = {
    strings: strings,
    parse: parse,
    level: level,
    isRunning: isRunning,
    canStart: canStart,
    headline: headline,
    detail: detail,
    tooltip: tooltip,
    streamStopped: streamStopped
  }
}
