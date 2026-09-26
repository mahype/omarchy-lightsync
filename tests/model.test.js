const test = require("node:test")
const assert = require("node:assert/strict")
const Model = require("../Model.js")

const s = Model.strings("en_US")

function state(overrides = {}) {
  return Object.assign({
    bridge: { id: "ecb5fafffe8f7ca6", host: "10.0.0.41", name: "Hue Bridge" },
    paired: true,
    area: "ac342fb3-c480-4555-8e12-3a53e5f52344",
    syncState: "idle",
    error: "",
    fps: 0
  }, overrides)
}

test("maps operational states", () => {
  assert.equal(Model.level(state()), "idle")
  assert.equal(Model.level(state({ syncState: "active" })), "active")
  assert.equal(Model.level(state({ syncState: "starting" })), "starting")
  assert.equal(Model.level(state({ error: "boom" })), "failed")
  assert.equal(Model.level(state({ paired: false })), "setup")
  assert.equal(Model.level(state({ area: "" })), "setup")
  assert.equal(Model.level(null), "setup")
})

test("start needs a paired bridge, an area and an idle sync", () => {
  assert.equal(Model.canStart(state()), true)
  assert.equal(Model.canStart(state({ bridge: null })), false)
  assert.equal(Model.canStart(state({ syncState: "active" })), false)
  assert.equal(Model.isRunning(state({ syncState: "stopping" })), true)
  assert.equal(Model.isRunning(state()), false)
})

test("fps only shows while active", () => {
  assert.equal(Model.fps(state({ syncState: "active", fps: 29.84 }), s), "29.8 FPS")
  assert.equal(Model.fps(state({ fps: 29.84 }), s), s.fpsUnavailable)
})

test("tooltip names the next right-click action", () => {
  assert.match(Model.tooltip(state(), s), /start synchronization/)
  assert.match(Model.tooltip(state({ syncState: "active" }), s), /stop synchronization/)
  assert.doesNotMatch(Model.tooltip(state({ paired: false }), s), /Right click/)
})

test("error texts fill in details and fall back", () => {
  assert.equal(Model.errorText("stream", "handshake failed\nmore", s), "The light stream stopped: handshake failed")
  assert.equal(Model.errorText("nope", "", s), s.commandFailed)
})

test("German and English strings have the same keys", () => {
  const en = Model.STRINGS.en
  const de = Model.STRINGS.de
  assert.deepEqual(Object.keys(de).sort(), Object.keys(en).sort())
  assert.deepEqual(Object.keys(de.errors).sort(), Object.keys(en.errors).sort())
  assert.equal(Model.strings("de_DE"), de)
})
