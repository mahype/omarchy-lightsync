// Persisted settings in ~/.config/omarchy-lightsync-hue/config.json.
// Hue credentials never go here; they live in the Secret Service.

var VERSION = 1

function defaults() {
  return {
    version: VERSION,
    bridge: null,
    area: "",
    screen: "",
    mode: "video",
    intensity: "high",
    brightness: 100,
    restoreOnStop: true,
    autoStart: false,
    lastStreamer: ""
  }
}

function normalizeBridge(value) {
  if (!value || typeof value !== "object") return null
  var id = String(value.id || "").trim().toLowerCase()
  var host = String(value.host || "").trim()
  if (!/^[0-9a-f]{16}$/.test(id) || !host) return null
  return { id: id, host: host, name: String(value.name || "Hue Bridge") }
}

function normalize(raw) {
  var out = defaults()
  if (!raw || typeof raw !== "object") return out
  out.bridge = normalizeBridge(raw.bridge)
  if (out.bridge && /^[0-9a-f-]{36}$/i.test(String(raw.area || ""))) out.area = String(raw.area)
  if (typeof raw.screen === "string") out.screen = raw.screen
  if (["video", "game"].indexOf(raw.mode) >= 0) out.mode = raw.mode
  if (["subtle", "moderate", "high", "extreme"].indexOf(raw.intensity) >= 0) out.intensity = raw.intensity
  var brightness = Number(raw.brightness)
  if (raw.brightness !== undefined && raw.brightness !== null && isFinite(brightness))
    out.brightness = Math.max(0, Math.min(100, Math.round(brightness)))
  if (typeof raw.restoreOnStop === "boolean") out.restoreOnStop = raw.restoreOnStop
  if (typeof raw.autoStart === "boolean") out.autoStart = raw.autoStart
  if (typeof raw.lastStreamer === "string") out.lastStreamer = raw.lastStreamer
  return out
}

function parse(text) {
  try { return normalize(JSON.parse(String(text || ""))) } catch (e) { return defaults() }
}

function serialize(config) {
  return JSON.stringify(normalize(config), null, 2) + "\n"
}

// Returns a new config with one user-facing setting changed, or null when the
// key or value is not allowed.
function withSetting(config, key, value) {
  var allowed = ["mode", "intensity", "brightness", "restoreOnStop", "autoStart", "screen"]
  if (allowed.indexOf(key) < 0) return null
  var next = normalize(config)
  var raw = JSON.parse(JSON.stringify(next))
  raw[key] = value
  var result = normalize(raw)
  return JSON.stringify(result[key]) === JSON.stringify(value) ? result : null
}

function withBridge(config, bridge) {
  var next = normalize(config)
  var normalized = normalizeBridge(bridge)
  if (!normalized || !next.bridge || next.bridge.id !== normalized.id) next.area = ""
  next.bridge = normalized
  if (!normalized) next.lastStreamer = ""
  return next
}

function withArea(config, areaId) {
  var raw = JSON.parse(JSON.stringify(normalize(config)))
  raw.area = String(areaId || "")
  return normalize(raw)
}

function withStreamer(config, streamer) {
  var next = normalize(config)
  next.lastStreamer = String(streamer || "")
  return next
}

if (typeof module !== "undefined") module.exports = {
  VERSION: VERSION, defaults: defaults, normalize: normalize, parse: parse, serialize: serialize,
  withSetting: withSetting, withBridge: withBridge, withArea: withArea, withStreamer: withStreamer
}
