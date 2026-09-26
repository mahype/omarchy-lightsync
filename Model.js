// Pure state and localization helpers for the bar widget and panel.
// `state` is the service (or any object with the same fields):
//   bridge, paired, area, syncState, error, fps

var STRINGS = {
  en: {
    title: "Light Sync for Hue", idle: "Synchronization is off",
    active: "Synchronizing lights", starting: "Starting synchronization",
    stopping: "Stopping synchronization", failed: "Synchronization needs attention",
    setup: "Light Sync needs setup", commandFailed: "Hue bridge request failed",
    refreshing: "Refreshing…", updated: "Settings saved",
    leftClick: "Left click: open controls", startClick: "Right click: start synchronization",
    stopClick: "Right click: stop synchronization", fps: "%1 FPS", fpsUnavailable: "FPS unavailable",
    start: "Start synchronization", stop: "Stop synchronization",
    mode: "MODE", video: "Video", game: "Game",
    brightness: "BRIGHTNESS", intensity: "INTENSITY", subtle: "Subtle",
    moderate: "Moderate", high: "High", extreme: "Extreme",
    connection: "CONNECTION", bridge: "Bridge", area: "Entertainment area",
    connected: "Connected", notConfigured: "Not configured", discoverBridge: "Find Hue bridge",
    discovering: "Searching for Hue bridges…",
    setupRequired: "Connect a Hue Bridge and choose an Entertainment area before starting synchronization.",
    configureConnection: "Set up Hue connection", noBridges: "No Hue bridges found.", pair: "Pair",
    pressLink: "Press the button on the Hue bridge now. Waiting %1 s…",
    refreshAreas: "Refresh areas", noAreas: "No Entertainment areas found. Create one in the Hue app.",
    forgetBridge: "Forget bridge", lights: "%1 lights",
    settings: "SETTINGS", display: "Display", restoreOnStop: "Restore lights on stop",
    restoreOnStopHint: "Return lights to their previous state after synchronization.",
    autoStart: "Start sync with Omarchy", autoStartHint: "Starts synchronization when the shell loads.",
    keyboardHint: "Esc close · Tab switch panel · R refresh",
    errors: {
      unreachable: "The Hue bridge is not reachable.",
      unauthorized: "The Hue bridge rejected the stored key. Forget the bridge and pair it again.",
      busy: "The Hue bridge is busy. Try again in a moment.",
      failed: "The Hue bridge rejected the request.",
      noCredentials: "No stored key for this bridge. Pair it again.",
      secretStore: "The key could not be saved. Is a Secret Service (GNOME Keyring) running?",
      pairTimeout: "The bridge button was not pressed in time.",
      areaMissing: "The selected Entertainment area no longer exists.",
      areaBusy: "The Entertainment area is in use by another app (e.g. Hue Sync).",
      areaEmpty: "The Entertainment area has no lights.",
      stream: "The light stream stopped: %1",
      streamTimeout: "The Hue bridge did not accept the light stream.",
      capture: "The screen cannot be captured.",
      restore: "Some lights could not be restored."
    }
  },
  de: {
    title: "Light Sync for Hue", idle: "Synchronisierung ist aus",
    active: "Lampen werden synchronisiert", starting: "Synchronisierung wird gestartet",
    stopping: "Synchronisierung wird beendet", failed: "Synchronisierung braucht Aufmerksamkeit",
    setup: "Light Sync muss eingerichtet werden", commandFailed: "Anfrage an die Hue Bridge fehlgeschlagen",
    refreshing: "Wird aktualisiert…", updated: "Einstellungen gespeichert",
    leftClick: "Linksklick: Steuerung öffnen", startClick: "Rechtsklick: Synchronisierung starten",
    stopClick: "Rechtsklick: Synchronisierung stoppen", fps: "%1 FPS", fpsUnavailable: "FPS nicht verfügbar",
    start: "Synchronisierung starten", stop: "Synchronisierung stoppen",
    mode: "MODUS", video: "Video", game: "Spiel",
    brightness: "HELLIGKEIT", intensity: "INTENSITÄT", subtle: "Dezent",
    moderate: "Mittel", high: "Hoch", extreme: "Extrem",
    connection: "VERBINDUNG", bridge: "Bridge", area: "Entertainment-Bereich",
    connected: "Verbunden", notConfigured: "Nicht eingerichtet", discoverBridge: "Hue Bridge suchen",
    discovering: "Suche nach Hue Bridges…",
    setupRequired: "Verbinde eine Hue Bridge und wähle einen Entertainment-Bereich, um die Synchronisierung zu starten.",
    configureConnection: "Hue-Verbindung einrichten", noBridges: "Keine Hue Bridges gefunden.", pair: "Koppeln",
    pressLink: "Drücke jetzt die Taste auf der Hue Bridge. Warte %1 s…",
    refreshAreas: "Bereiche aktualisieren", noAreas: "Keine Entertainment-Bereiche gefunden. Lege einen in der Hue-App an.",
    forgetBridge: "Bridge vergessen", lights: "%1 Lampen",
    settings: "EINSTELLUNGEN", display: "Bildschirm", restoreOnStop: "Lampen beim Stop wiederherstellen",
    restoreOnStopHint: "Setzt die Lampen nach der Synchronisierung auf ihren vorherigen Zustand zurück.",
    autoStart: "Sync mit Omarchy starten", autoStartHint: "Startet die Synchronisierung, wenn die Shell lädt.",
    keyboardHint: "Esc schließen · Tab Panel wechseln · R aktualisieren",
    errors: {
      unreachable: "Die Hue Bridge ist nicht erreichbar.",
      unauthorized: "Die Hue Bridge lehnt den gespeicherten Schlüssel ab. Vergiss die Bridge und kopple sie neu.",
      busy: "Die Hue Bridge ist ausgelastet. Versuche es gleich noch einmal.",
      failed: "Die Hue Bridge hat die Anfrage abgelehnt.",
      noCredentials: "Für diese Bridge ist kein Schlüssel gespeichert. Kopple sie neu.",
      secretStore: "Der Schlüssel konnte nicht gespeichert werden. Läuft ein Secret Service (GNOME Keyring)?",
      pairTimeout: "Die Taste auf der Bridge wurde nicht rechtzeitig gedrückt.",
      areaMissing: "Der gewählte Entertainment-Bereich existiert nicht mehr.",
      areaBusy: "Der Entertainment-Bereich wird von einer anderen App genutzt (z. B. Hue Sync).",
      areaEmpty: "Der Entertainment-Bereich enthält keine Lampen.",
      stream: "Der Lichtstream wurde beendet: %1",
      streamTimeout: "Die Hue Bridge hat den Lichtstream nicht angenommen.",
      capture: "Der Bildschirm kann nicht aufgenommen werden.",
      restore: "Einige Lampen konnten nicht wiederhergestellt werden."
    }
  }
}

function strings(localeName) {
  return String(localeName || "").toLowerCase().indexOf("de") === 0 ? STRINGS.de : STRINGS.en
}

function errorText(code, detail, s) {
  var text = s.errors[code] || s.commandFailed
  return text.replace("%1", String(detail || "").split("\n")[0])
}

function isReady(state) {
  return !!(state && state.bridge && state.paired && state.area)
}

function isRunning(state) {
  return !!(state && ["starting", "active", "stopping"].indexOf(state.syncState) >= 0)
}

function canStart(state) {
  return isReady(state) && state.syncState === "idle"
}

function level(state) {
  if (!state) return "setup"
  if (["active", "starting", "stopping"].indexOf(state.syncState) >= 0) return state.syncState
  if (state.error) return "failed"
  if (!isReady(state)) return "setup"
  return "idle"
}

function headline(state, s) {
  return s[level(state)] || s.idle
}

function fps(state, s) {
  var value = state ? Number(state.fps) : NaN
  return state && state.syncState === "active" && isFinite(value) && value > 0
    ? s.fps.replace("%1", value.toFixed(1)) : s.fpsUnavailable
}

function tooltip(state, s) {
  var lines = [s.title + " · " + headline(state, s)]
  if (state && state.error) lines.push(String(state.error).split("\n")[0])
  lines.push(s.leftClick)
  if (isRunning(state)) lines.push(s.stopClick)
  else if (canStart(state)) lines.push(s.startClick)
  return lines.join("\n")
}

if (typeof module !== "undefined") module.exports = {
  STRINGS: STRINGS, strings: strings, errorText: errorText, isReady: isReady,
  isRunning: isRunning, canStart: canStart, level: level, headline: headline,
  fps: fps, tooltip: tooltip
}
