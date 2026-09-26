const test = require("node:test")
const assert = require("node:assert/strict")
const Hue = require("../HueBridge.js")

const bridge = { id: "ECB5FAFFFE8F7CA6", host: "10.0.0.41" }
const AREA = "ac342fb3-c480-4555-8e12-3a53e5f52344"

test("curl config verifies the bridge ID as TLS name", () => {
  const text = Hue.curlConfig({ bridge, method: "PUT", path: "/clip/v2/resource/light/x", key: "k\"ey", body: { on: { on: true } }, caFile: "/ca.pem" })
  assert.match(text, /^url = "https:\/\/ecb5fafffe8f7ca6\/clip\/v2\/resource\/light\/x"$/m)
  assert.match(text, /^resolve = "ecb5fafffe8f7ca6:443:10\.0\.0\.41"$/m)
  assert.match(text, /^cacert = "\/ca\.pem"$/m)
  assert.match(text, /^header = "hue-application-key: k\\"ey"$/m)
  assert.match(text, /^data = "\{\\"on\\":\{\\"on\\":true\}\}"$/m)
  assert.doesNotMatch(text, /insecure/)
})

test("curl config refuses unsafe input", () => {
  assert.equal(Hue.curlConfig({ bridge, path: "/../x", caFile: "/ca" }), "")
  assert.equal(Hue.curlConfig({ bridge, path: "relative", caFile: "/ca" }), "")
  assert.equal(Hue.curlConfig({ bridge: { id: "nothex", host: "1.2.3.4" }, path: "/api", caFile: "/ca" }), "")
  assert.equal(Hue.curlConfig({ bridge: { id: bridge.id, host: "evil.example" }, path: "/api", caFile: "/ca" }), "")
  assert.match(Hue.curlConfig({ bridge: { id: bridge.id, host: "fe80::1" }, path: "/api", caFile: "/ca" }), /resolve = "ecb5fafffe8f7ca6:443:\[fe80::1\]"/)
})

test("parses curl output and CLIP envelopes", () => {
  const ok = Hue.parseCurlOutput('{"data":[{"id":"a"}],"errors":[]}\n200')
  assert.deepEqual(Hue.parseClip(ok), { ok: true, data: [{ id: "a" }], error: "" })
  assert.equal(Hue.parseClip(Hue.parseCurlOutput("\n000")).error, "unreachable")
  assert.equal(Hue.parseClip(Hue.parseCurlOutput("{}\n403")).error, "unauthorized")
  assert.equal(Hue.parseClip(Hue.parseCurlOutput("{}\n503")).error, "busy")
  assert.equal(Hue.parseClip(Hue.parseCurlOutput('{"data":[],"errors":[{"description":"x"}]}\n200')).error, "failed")
})

test("pairing distinguishes waiting, success and failure", () => {
  const wait = Hue.parsePairResponse({ status: 200, body: [{ error: { type: 101 } }] })
  assert.equal(wait.state, "waiting")
  const done = Hue.parsePairResponse({ status: 200, body: [{ success: { username: "app", clientkey: "0A1B" } }] })
  assert.deepEqual(done, { state: "paired", credentials: { applicationKey: "app", clientKey: "0A1B" } })
  assert.equal(Hue.parsePairResponse({ status: 200, body: [{ error: { type: 7 } }] }).state, "error")
  assert.equal(Hue.parsePairResponse({ status: 0, body: null }).error, "unreachable")
  assert.equal(Hue.parseCredentials('{"applicationKey":"a","clientKey":"zz"}'), null)
})

test("discovers bridges through avahi, preferring IPv4", () => {
  const text = [
    "+;wlp8s0;IPv4;Hue\\032Bridge\\032-\\0328F7CA6;_hue._tcp;local",
    '=;wlp8s0;IPv6;Hue\\032Bridge\\032-\\0328F7CA6;_hue._tcp;local;ecb5fa8f7ca6.local;fe80::1;443;"bridgeid=ecb5fafffe8f7ca6" "modelid=BSB002"',
    '=;wlp8s0;IPv4;Hue\\032Bridge\\032-\\0328F7CA6;_hue._tcp;local;ecb5fa8f7ca6.local;10.0.0.41;443;"bridgeid=ecb5fafffe8f7ca6" "modelid=BSB002"'
  ].join("\n")
  assert.deepEqual(Hue.parseAvahi(text), [{ id: "ecb5fafffe8f7ca6", host: "10.0.0.41", name: "Hue Bridge - 8F7CA6" }])
  assert.deepEqual(Hue.parseCloudDiscovery('[{"id":"ECB5FAFFFE8F7CA6","internalipaddress":"10.0.0.41"},{"id":"bad"}]'),
    [{ id: "ecb5fafffe8f7ca6", host: "10.0.0.41", name: "Hue Bridge" }])
})

test("maps channel positions onto the screen", () => {
  assert.deepEqual(Hue.channelPoint({ x: -1, y: 0, z: 1 }), { x: 0, y: 0 })
  assert.deepEqual(Hue.channelPoint({ x: 1, y: 0, z: -1 }), { x: 1, y: 1 })
  assert.deepEqual(Hue.channelPoint({ x: 0, z: 0 }), { x: 0.5, y: 0.5 })
  const area = Hue.parseArea({
    id: AREA, metadata: { name: "PC" }, status: "active", active_streamer: { rid: "s1" },
    channels: [{ channel_id: 0, position: { x: 0, y: 0, z: 0 } }, { channel_id: 0, position: { x: 1, y: 0, z: 1 } }],
    light_services: [{ rid: "l1" }]
  })
  assert.deepEqual(area, { id: AREA, name: "PC", channels: [{ id: 0, x: 0.5, y: 0.5 }], lights: 1, active: true, streamer: "s1" })
})

test("snapshots connected area lights for restoring", () => {
  const L1 = "11111111-1111-1111-1111-111111111111"
  const L2 = "22222222-2222-2222-2222-222222222222"
  const area = { light_services: [{ rid: L1 }, { rid: L2 }] }
  const lights = [
    { id: L1, owner: { rid: "d1" }, on: { on: true }, dimming: { brightness: 50 }, color_temperature: { mirek: 300, mirek_valid: true }, color: { xy: { x: 0.1, y: 0.2 } } },
    { id: L2, owner: { rid: "d2" }, on: { on: false }, color: { xy: { x: 0.3, y: 0.4 } } }
  ]
  const connectivity = [{ owner: { rid: "d2" }, status: "connectivity_issue" }]
  assert.deepEqual(Hue.snapshot(area, [], lights, connectivity), [
    { id: L1, state: { on: { on: true }, dimming: { brightness: 50 }, color_temperature: { mirek: 300 } } }
  ])
})

test("finds area lights through entertainment services on older bridges", () => {
  const area = { channels: [{ members: [{ service: { rid: "e1" } }, { service: { rid: "e2" } }] }] }
  const entertainment = [{ id: "e1", renderer_reference: { rid: "l1" } }, { id: "e2", owner: { rid: "d2" } }]
  const lights = [{ id: "l2", owner: { rid: "d2" } }]
  assert.deepEqual(Hue.areaLightIds(area, entertainment, lights).sort(), ["l1", "l2"])
})

test("encodes HueStream v2 frames as escaped lines", () => {
  const line = Hue.packetLine(AREA, 258, [{ id: 3, r: 65535, g: 256, b: 0 }])
  const bytes = Buffer.from(line.split("\\x").slice(1).map((h) => parseInt(h, 16)))
  assert.equal(bytes.subarray(0, 9).toString(), "HueStream")
  assert.deepEqual([...bytes.subarray(9, 16)], [2, 0, 2, 0, 0, 0, 0])
  assert.equal(bytes.subarray(16, 52).toString(), AREA)
  assert.deepEqual([...bytes.subarray(52)], [3, 0xff, 0xff, 0x01, 0x00, 0x00, 0x00])
  assert.match(line, /^(\\x[0-9a-f]{2})+$/)
  assert.equal(Hue.packetLine("not-a-uuid", 0, []), "")
})
