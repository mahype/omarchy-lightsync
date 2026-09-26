// Hue bridge protocol: request building and response parsing. No I/O here;
// Service.qml runs the requests through curl and the stream through openssl.
//
// Every HTTPS request is verified against Signify's Hue root certificates with
// the bridge ID as TLS name, so the bridge address is only used to resolve it.

var DEVICE_TYPE = "omarchy-light-sync-hue#desktop"
var DISCOVERY_URL = "https://discovery.meethue.com/"
var ENTERTAINMENT_PORT = 2100

function normalizeBridgeId(value) {
  var id = String(value || "").trim().toLowerCase()
  return /^[0-9a-f]{16}$/.test(id) ? id : ""
}

function isUuid(value) {
  return /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(String(value || ""))
}

function isHost(value) {
  var host = String(value || "")
  var v4 = host.match(/^(\d{1,3})\.(\d{1,3})\.(\d{1,3})\.(\d{1,3})$/)
  if (v4) {
    for (var i = 1; i <= 4; i++) if (Number(v4[i]) > 255) return false
    return host !== "0.0.0.0"
  }
  return /^[0-9a-f:]+$/i.test(host) && host.indexOf(":") >= 0 && host !== "::"
}

function validPath(path) {
  var value = String(path || "")
  return value.charAt(0) === "/" && value.indexOf("..") < 0 && !/[\s"\\]/.test(value)
}

// curl reads this from stdin (`curl -K -`), which keeps the application key
// out of the process list.
function quote(value) {
  return "\"" + String(value).replace(/\\/g, "\\\\").replace(/"/g, "\\\"")
    .replace(/\n/g, "\\n").replace(/\r/g, "\\r").replace(/\t/g, "\\t") + "\""
}

function curlConfig(request) {
  var bridge = request.bridge || {}
  var id = normalizeBridgeId(bridge.id)
  if (!id || !isHost(bridge.host) || !validPath(request.path) || !request.caFile) return ""
  var host = bridge.host.indexOf(":") >= 0 ? "[" + bridge.host + "]" : bridge.host
  var lines = [
    "url = " + quote("https://" + id + request.path),
    "request = " + quote(request.method || "GET"),
    "cacert = " + quote(request.caFile),
    "resolve = " + quote(id + ":443:" + host),
    "noproxy = \"*\"",
    "max-time = " + Math.max(1, Math.round(Number(request.timeout) || 10)),
    "silent",
    "show-error",
    "write-out = \"\\n%{http_code}\""
  ]
  if (request.key) lines.push("header = " + quote("hue-application-key: " + request.key))
  if (request.body !== undefined) {
    lines.push("header = \"content-type: application/json\"")
    lines.push("data = " + quote(JSON.stringify(request.body)))
  }
  return lines.join("\n") + "\n"
}

// curl prints the body, a newline and the HTTP status (write-out above).
function parseCurlOutput(text) {
  var raw = String(text || "")
  var split = raw.lastIndexOf("\n")
  var status = Number(raw.slice(split + 1).trim())
  var body = null
  try { body = JSON.parse(raw.slice(0, Math.max(0, split))) } catch (e) { body = null }
  return { status: isFinite(status) ? status : 0, body: body }
}

// Result of a CLIP v2 request: { ok, data, error } with a short error code.
function parseClip(response) {
  if (!response || !response.status) return { ok: false, data: [], error: "unreachable" }
  if (response.status === 403 || response.status === 401) return { ok: false, data: [], error: "unauthorized" }
  if (response.status === 429 || response.status >= 500) return { ok: false, data: [], error: "busy" }
  var body = response.body
  if (response.status < 200 || response.status >= 300 || !body || !Array.isArray(body.data))
    return { ok: false, data: [], error: "failed" }
  if (Array.isArray(body.errors) && body.errors.length > 0) return { ok: false, data: body.data, error: "failed" }
  return { ok: true, data: body.data, error: "" }
}

// Registration answers with a v1 array: [{ success: {...} }] or [{ error: {...} }].
function parsePairResponse(response) {
  var entry = response && Array.isArray(response.body) ? response.body[0] : null
  if (!response || !response.status) return { state: "error", error: "unreachable" }
  if (entry && entry.success && entry.success.username && /^[0-9a-f]+$/i.test(String(entry.success.clientkey || "")))
    return { state: "paired", credentials: { applicationKey: String(entry.success.username), clientKey: String(entry.success.clientkey) } }
  if (entry && entry.error && Number(entry.error.type) === 101) return { state: "waiting" }
  return { state: "error", error: "failed" }
}

function parseCredentials(text) {
  try {
    var value = JSON.parse(String(text || ""))
    if (value && value.applicationKey && /^[0-9a-f]+$/i.test(String(value.clientKey || "")))
      return { applicationKey: String(value.applicationKey), clientKey: String(value.clientKey) }
  } catch (e) {}
  return null
}

// `avahi-browse -rpt _hue._tcp`: resolved lines start with "=" and carry the
// bridge ID in the TXT record. IPv4 wins over IPv6 for the same bridge.
function parseAvahi(text) {
  var found = {}
  String(text || "").split("\n").forEach(function(line) {
    if (line.charAt(0) !== "=") return
    var fields = line.split(";")
    if (fields.length < 10) return
    var match = fields.slice(9).join(";").match(/"bridgeid=([0-9a-fA-F]{16})"/)
    var id = normalizeBridgeId(match ? match[1] : "")
    var host = fields[7]
    if (!id || !isHost(host)) return
    var ipv4 = fields[2] === "IPv4"
    if (found[id] && found[id].ipv4) return
    found[id] = { id: id, host: host, name: avahiName(fields[3]), ipv4: ipv4 }
  })
  return Object.keys(found).sort().map(function(id) {
    return { id: id, host: found[id].host, name: found[id].name }
  })
}

function avahiName(value) {
  return String(value || "").replace(/\\(\d{3})/g, function(_, code) {
    return String.fromCharCode(Number(code))
  }) || "Hue Bridge"
}

function parseCloudDiscovery(text) {
  var list
  try { list = JSON.parse(String(text || "")) } catch (e) { return [] }
  if (!Array.isArray(list)) return []
  var bridges = []
  list.forEach(function(entry) {
    var id = normalizeBridgeId(entry && entry.id)
    if (id && isHost(entry.internalipaddress)) bridges.push({ id: id, host: entry.internalipaddress, name: "Hue Bridge" })
  })
  return bridges
}

// Hue positions run from -1 to 1. x is left to right and z bottom to top,
// which maps onto the screen as seen from the viewer; y (depth) is ignored.
function channelPoint(position) {
  var x = Number(position && position.x)
  var z = Number(position && position.z)
  return {
    x: Math.max(0, Math.min(1, ((isFinite(x) ? x : 0) + 1) / 2)),
    y: Math.max(0, Math.min(1, (1 - (isFinite(z) ? z : 0)) / 2))
  }
}

function parseArea(resource) {
  if (!resource || !isUuid(resource.id)) return null
  var channels = []
  var seen = {}
  ;(resource.channels || []).forEach(function(channel) {
    var id = Number(channel && channel.channel_id)
    if (!isFinite(id) || id < 0 || id > 255 || seen[id]) return
    seen[id] = true
    var point = channelPoint(channel.position)
    channels.push({ id: id, x: point.x, y: point.y })
  })
  var lights = (resource.light_services || []).length
  return {
    id: String(resource.id),
    name: resource.metadata && resource.metadata.name ? String(resource.metadata.name) : String(resource.id),
    channels: channels,
    lights: lights || channels.length,
    active: resource.status === "active",
    streamer: resource.active_streamer && resource.active_streamer.rid ? String(resource.active_streamer.rid) : ""
  }
}

function parseAreas(data) {
  var areas = []
  ;(data || []).forEach(function(resource) {
    var area = parseArea(resource)
    if (area) areas.push(area)
  })
  return areas.sort(function(a, b) { return a.name.localeCompare(b.name) })
}

// Lights that belong to an area. Newer bridges list them in light_services;
// older ones only reference entertainment services, which point to lights.
function areaLightIds(area, entertainment, lights) {
  var ids = {}
  ;(area && area.light_services || []).forEach(function(service) { if (service && service.rid) ids[service.rid] = true })
  if (Object.keys(ids).length > 0) return Object.keys(ids)
  var services = {}
  ;(area && area.channels || []).forEach(function(channel) {
    ;(channel.members || []).forEach(function(member) {
      if (member && member.service && member.service.rid) services[member.service.rid] = true
    })
  })
  var lightByOwner = {}
  ;(lights || []).forEach(function(light) { if (light.owner && light.owner.rid) lightByOwner[light.owner.rid] = light.id })
  ;(entertainment || []).forEach(function(service) {
    if (!services[service.id]) return
    if (service.renderer_reference && service.renderer_reference.rid) ids[service.renderer_reference.rid] = true
    else if (service.owner && lightByOwner[service.owner.rid]) ids[lightByOwner[service.owner.rid]] = true
  })
  return Object.keys(ids)
}

function restoreState(light) {
  var state = { on: { on: !!(light.on && light.on.on) } }
  if (light.dimming && isFinite(Number(light.dimming.brightness))) state.dimming = { brightness: Number(light.dimming.brightness) }
  if (light.gradient && Array.isArray(light.gradient.points) && light.gradient.points.length > 0) {
    state.gradient = { points: light.gradient.points.map(function(point) { return { color: { xy: point.color.xy } } }) }
    if (light.gradient.mode) state.gradient.mode = light.gradient.mode
  } else if (light.color_temperature && light.color_temperature.mirek_valid && light.color_temperature.mirek) {
    state.color_temperature = { mirek: light.color_temperature.mirek }
  } else if (light.color && light.color.xy) {
    state.color = { xy: light.color.xy }
  }
  return state
}

// Snapshot of the connected lights in an area, to be PUT back after streaming.
function snapshot(area, entertainment, lights, connectivity) {
  var wanted = {}
  areaLightIds(area, entertainment, lights).forEach(function(id) { wanted[id] = true })
  var offline = {}
  ;(connectivity || []).forEach(function(entry) {
    if (entry.status !== "connected" && entry.owner) offline[entry.owner.rid] = true
  })
  var result = []
  ;(lights || []).forEach(function(light) {
    if (!wanted[light.id] || !isUuid(light.id)) return
    if (light.owner && offline[light.owner.rid]) return
    result.push({ id: light.id, state: restoreState(light) })
  })
  return result
}

function hex2(value) {
  return "\\x" + (value < 16 ? "0" : "") + value.toString(16)
}

// One HueStream v2 RGB frame as a line of "\xHH" escapes; tools/hue-stream.sh
// turns each line back into one DTLS record. colors: [{ id, r, g, b }] 16 bit.
function packetLine(areaId, sequence, colors) {
  if (!isUuid(areaId)) return ""
  var bytes = [0x48, 0x75, 0x65, 0x53, 0x74, 0x72, 0x65, 0x61, 0x6d, 0x02, 0x00, sequence & 0xff, 0, 0, 0, 0]
  var id = String(areaId).toLowerCase()
  for (var i = 0; i < id.length; i++) bytes.push(id.charCodeAt(i))
  for (var c = 0; c < colors.length && c < 20; c++) {
    var color = colors[c]
    bytes.push(color.id & 0xff)
    ;[color.r, color.g, color.b].forEach(function(value) {
      var v = Math.max(0, Math.min(65535, Math.round(Number(value) || 0)))
      bytes.push(v >> 8, v & 0xff)
    })
  }
  var out = ""
  for (var b = 0; b < bytes.length; b++) out += hex2(bytes[b])
  return out
}

if (typeof module !== "undefined") module.exports = {
  DEVICE_TYPE: DEVICE_TYPE, DISCOVERY_URL: DISCOVERY_URL, ENTERTAINMENT_PORT: ENTERTAINMENT_PORT,
  normalizeBridgeId: normalizeBridgeId, isUuid: isUuid, isHost: isHost, validPath: validPath,
  quote: quote, curlConfig: curlConfig, parseCurlOutput: parseCurlOutput, parseClip: parseClip,
  parsePairResponse: parsePairResponse, parseCredentials: parseCredentials,
  parseAvahi: parseAvahi, parseCloudDiscovery: parseCloudDiscovery,
  channelPoint: channelPoint, parseArea: parseArea, parseAreas: parseAreas,
  areaLightIds: areaLightIds, restoreState: restoreState, snapshot: snapshot, packetLine: packetLine
}
