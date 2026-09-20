const test = require("node:test")
const assert = require("node:assert/strict")
const Model = require("../Model.js")

function status(sync = "idle", bridge = "ready", configured = true) {
  const value = {
    protocol_version: 1,
    generation: 3,
    daemon_version: "0.1.0",
    service: { state: "ready" },
    bridge: { state: bridge },
    capture: { state: "idle" },
    sync: { state: sync }
  }
  if (configured) value.area = { id: "1", name: "Desk" }
  return value
}

test("parses one status stream line", () => {
  assert.deepEqual(Model.parse(JSON.stringify(status())), status())
  assert.equal(Model.parse("not json"), null)
  assert.equal(Model.parse('{"protocol_version":1}'), null)
})

test("maps operational states", () => {
  assert.equal(Model.level(status("active"), true), "active")
  assert.equal(Model.level(status("idle"), true), "idle")
  assert.equal(Model.level(status("idle", "needs_setup", false), true), "setup")
  assert.equal(Model.level(status("area_busy"), true), "failed")
  assert.equal(Model.level(null, false), "missing")
})

test("only active-like states count as running", () => {
  assert.equal(Model.isRunning(status("active")), true)
  assert.equal(Model.isRunning(status("recovering")), true)
  assert.equal(Model.isRunning(status("stopping")), true)
  assert.equal(Model.isRunning(status("idle")), false)
})

test("starts only from a configured safe state", () => {
  assert.equal(Model.canStart(status("idle")), true)
  assert.equal(Model.canStart(status("failed")), true)
  assert.equal(Model.canStart(status("stopping")), false)
  assert.equal(Model.canStart(status("idle", "needs_setup", false)), false)
})

test("starts from the daemon's configured idle status", () => {
  const idle = status()
  assert.equal(idle.capture.state, "idle")
  assert.equal(idle.sync.state, "idle")
  assert.equal(Model.level(idle, true), "idle")
  assert.equal(Model.canStart(idle), true)
  assert.equal(Model.detail(idle), "Desk")
})

test("keeps an unconfigured daemon status in setup", () => {
  const setup = status("idle", "needs_setup", false)
  assert.equal(Model.level(setup, true), "setup")
  assert.equal(Model.canStart(setup), false)
})

test("pairing remains setup and cannot start synchronization", () => {
  const pairing = status("idle", "pairing", false)
  assert.equal(Model.level(pairing, true), "setup")
  assert.equal(Model.canStart(pairing), false)
})

test("selects German labels safely", () => {
  assert.match(Model.headline(status("active"), true, Model.strings("de_DE")), /synchronisiert/)
  assert.match(Model.headline(status("active"), true, Model.strings("en_US")), /Synchronizing/)
  assert.match(Model.streamStopped(7, Model.strings("de_DE")), /Code 7/)
  assert.match(Model.streamStopped(7, Model.strings("en_US")), /exit 7/)
})
