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

test("parses config show values and ignores unrelated output", () => {
  const result = Model.parseConfig([
    "language=system", "backend=portal-pipewire", "mode=game",
    "intensity=extreme", "brightness=85", "audio-reactive=false",
    "restore-on-stop=true", "launch-at-login=false", "auto-start=true",
    "future-value=ignored"
  ].join("\n"))
  assert.equal(result.ok, true)
  assert.deepEqual(result.config, {
    language: "system", backend: "portal-pipewire", mode: "game",
    intensity: "extreme", brightness: 85, "audio-reactive": false,
    "restore-on-stop": true, "launch-at-login": false, "auto-start": true
  })
  assert.equal(Model.parseConfig("noise only").ok, false)
})

test("parses profile list markers, UUIDs, spaces, and empty output", () => {
  const raw = "* 01234567-89ab-cdef-0123-456789abcdef\tLiving Room\n"
    + "  fedcba98-7654-3210-fedc-ba9876543210\tGames and films"
  assert.deepEqual(Model.parseProfiles(raw), {
    ok: true,
    profiles: [
      { id: "01234567-89ab-cdef-0123-456789abcdef", name: "Living Room", active: true },
      { id: "fedcba98-7654-3210-fedc-ba9876543210", name: "Games and films", active: false }
    ]
  })
  assert.deepEqual(Model.parseProfiles("No profiles configured."), { ok: true, profiles: [] })
  assert.deepEqual(Model.parseProfiles("  first-id\tInactive first profile\n"), {
    ok: true,
    profiles: [{ id: "first-id", name: "Inactive first profile", active: false }]
  })
  assert.equal(Model.parseProfiles("malformed").ok, false)
})

test("English and German expose identical localization keys", () => {
  assert.deepEqual(Object.keys(Model.STRINGS.en).sort(), Object.keys(Model.STRINGS.de).sort())
  for (const value of Object.values(Model.STRINGS.en)) assert.notEqual(value, "")
  for (const value of Object.values(Model.STRINGS.de)) assert.notEqual(value, "")
})

test("localizes diagnostic state labels", () => {
  assert.equal(Model.stateLabel("needs_setup", Model.strings("en_US")), "Needs setup")
  assert.equal(Model.stateLabel("needs_setup", Model.strings("de_DE")), "Einrichtung nötig")
  assert.equal(Model.stateLabel("future_state", Model.strings("de_DE")), "Future state")
})

test("status state coverage keeps failures and setup distinct", () => {
  assert.equal(Model.level(status("starting"), true), "starting")
  assert.equal(Model.level(status("stopping"), true), "stopping")
  assert.equal(Model.level(status("recovering"), true), "recovering")
  assert.equal(Model.level(status("idle", "unreachable"), true), "failed")
  const failedService = status()
  failedService.service.state = "failed"
  assert.equal(Model.level(failedService, true), "failed")
  assert.equal(Model.level(null, true), "unavailable")
})

test("panel action availability prevents duplicate and unsafe actions", () => {
  assert.deepEqual(Model.panelActions(status("idle"), true, false), {
    start: true, stop: false, mutate: true, setup: false
  })
  assert.deepEqual(Model.panelActions(status("active"), true, false), {
    start: false, stop: true, mutate: true, setup: false
  })
  assert.deepEqual(Model.panelActions(status("idle"), true, true), {
    start: false, stop: false, mutate: false, setup: false
  })
  assert.deepEqual(Model.panelActions(status("idle", "needs_setup", false), true, false), {
    start: false, stop: false, mutate: true, setup: true
  })
})
