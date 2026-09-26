const test = require("node:test")
const assert = require("node:assert/strict")
const Engine = require("../SyncEngine.js")
const ConfigStore = require("../ConfigStore.js")

function frame(width, height, rgb) {
  const pixels = new Uint8ClampedArray(width * height * 4)
  for (let i = 0; i < pixels.length; i += 4) pixels.set([rgb[0], rgb[1], rgb[2], 255], i)
  return pixels
}

const settings = { mode: "video", intensity: "extreme", brightness: 100 }

test("presets match the former daemon", () => {
  assert.deepEqual(Engine.preset("video", "moderate"), { exposure: 0, blackCutoff: 0.006, radius: 3, attack: 0.48, release: 0.2 })
  assert.equal(Engine.preset("game", "high").radius, 1)
  assert.ok(Math.abs(Engine.preset("game", "extreme").exposure - 0.55) < 1e-9)
})

test("frame size follows the screen aspect", () => {
  assert.deepEqual(Engine.frameSize(7680, 2160), { width: 64, height: 18 })
  assert.deepEqual(Engine.frameSize(1920, 1080), { width: 64, height: 36 })
  assert.deepEqual(Engine.frameSize(0, 0), { width: 64, height: 36 })
})

test("extreme attack passes white through at full brightness", () => {
  const state = Engine.createState()
  const colors = Engine.process(state, frame(8, 8, [255, 255, 255]), 8, 8, [{ id: 0, x: 0.5, y: 0.5 }], settings)
  // exposure +0.4 stops saturates, attack 1.0 reaches the target at once
  assert.deepEqual(colors, [{ id: 0, r: 65535, g: 65535, b: 65535 }])
})

test("brightness scales and near-black is cut off", () => {
  const half = Engine.process(Engine.createState(), frame(8, 8, [255, 0, 0]), 8, 8, [{ id: 1, x: 0, y: 0 }],
    { mode: "video", intensity: "extreme", brightness: 0 })
  assert.deepEqual(half, [{ id: 1, r: 0, g: 0, b: 0 }])
  const dark = Engine.process(Engine.createState(), frame(8, 8, [3, 3, 3]), 8, 8, [{ id: 1, x: 0, y: 0 }], settings)
  assert.deepEqual(dark, [{ id: 1, r: 0, g: 0, b: 0 }])
})

test("release smooths towards darker colors", () => {
  const state = Engine.createState()
  const channel = [{ id: 0, x: 0.5, y: 0.5 }]
  Engine.process(state, frame(4, 4, [255, 255, 255]), 4, 4, channel, settings)
  const next = Engine.process(state, frame(4, 4, [0, 0, 0]), 4, 4, channel, settings)[0]
  assert.ok(next.r > 0 && next.r < 65535)
})

test("samples the region around each channel", () => {
  const pixels = frame(4, 1, [0, 0, 0])
  pixels.set([255, 0, 0, 255], 0)
  const left = Engine.sample(pixels, 4, 1, { x: 0, y: 0 }, 0)
  const right = Engine.sample(pixels, 4, 1, { x: 1, y: 0 }, 0)
  assert.equal(left.r, 1)
  assert.equal(right.r, 0)
})

test("config normalizes, rejects bad values and clears the area with the bridge", () => {
  const bridge = { id: "ECB5FAFFFE8F7CA6", host: "10.0.0.41", name: "Hue" }
  let config = ConfigStore.withBridge(ConfigStore.defaults(), bridge)
  assert.equal(config.bridge.id, "ecb5fafffe8f7ca6")
  config = ConfigStore.withArea(config, "ac342fb3-c480-4555-8e12-3a53e5f52344")
  assert.equal(config.area, "ac342fb3-c480-4555-8e12-3a53e5f52344")
  assert.equal(ConfigStore.withSetting(config, "mode", "music"), null)
  assert.equal(ConfigStore.withSetting(config, "bridge", null), null)
  assert.equal(ConfigStore.withSetting(config, "brightness", 55).brightness, 55)
  assert.equal(ConfigStore.withSetting(config, "brightness", 555), null)
  assert.equal(ConfigStore.withBridge(config, null).area, "")
  assert.deepEqual(ConfigStore.parse("garbage"), ConfigStore.defaults())
  const round = ConfigStore.parse(ConfigStore.serialize(config))
  assert.deepEqual(round, config)
})
