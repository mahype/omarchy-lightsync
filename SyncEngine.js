// Screen sampling and color processing for Entertainment channels.
// Works on the small RGBA frame ScreenSampler.qml reads back from the GPU.

var MODES = ["video", "game"]
var INTENSITIES = ["subtle", "moderate", "high", "extreme"]
var TARGET_FPS = 30

var LINEAR = (function() {
  var table = []
  for (var i = 0; i < 256; i++) {
    var v = i / 255
    table.push(v <= 0.04045 ? v / 12.92 : Math.pow((v + 0.055) / 1.055, 2.4))
  }
  return table
})()

// Sample radius is in pixels of the sampled frame (about 64 px wide).
function preset(mode, intensity) {
  var base = mode === "game"
    ? { exposure: 0.15, blackCutoff: 0.003, radius: 1 }
    : { exposure: 0.0, blackCutoff: 0.006, radius: 2 }
  var delta = {
    subtle: { exposure: -0.25, attack: 0.28, release: 0.12, radius: 2 },
    moderate: { exposure: 0.0, attack: 0.48, release: 0.2, radius: 1 },
    high: { exposure: 0.2, attack: 0.72, release: 0.34, radius: 0 },
    extreme: { exposure: 0.4, attack: 1.0, release: 0.52, radius: 0 }
  }[intensity] || { exposure: 0.2, attack: 0.72, release: 0.34, radius: 0 }
  return {
    exposure: base.exposure + delta.exposure,
    blackCutoff: base.blackCutoff,
    radius: base.radius + delta.radius,
    attack: delta.attack,
    release: delta.release
  }
}

// Size of the sampled frame: fixed width, height follows the screen aspect.
function frameSize(sourceWidth, sourceHeight) {
  var width = 64
  var ratio = sourceWidth > 0 && sourceHeight > 0 ? sourceHeight / sourceWidth : 9 / 16
  return { width: width, height: Math.max(8, Math.min(64, Math.round(width * ratio))) }
}

// Mean linear color of the square around a normalized point. pixels is RGBA.
function sample(pixels, width, height, point, radius) {
  var cx = Math.round(point.x * (width - 1))
  var cy = Math.round(point.y * (height - 1))
  var r = 0, g = 0, b = 0, n = 0
  for (var y = Math.max(0, cy - radius); y <= Math.min(height - 1, cy + radius); y++) {
    for (var x = Math.max(0, cx - radius); x <= Math.min(width - 1, cx + radius); x++) {
      var i = (y * width + x) * 4
      r += LINEAR[pixels[i]]
      g += LINEAR[pixels[i + 1]]
      b += LINEAR[pixels[i + 2]]
      n++
    }
  }
  return n > 0 ? { r: r / n, g: g / n, b: b / n } : { r: 0, g: 0, b: 0 }
}

function toRgb16(value) {
  var v = Math.max(0, Math.min(1, value))
  var srgb = v <= 0.0031308 ? v * 12.92 : 1.055 * Math.pow(v, 1 / 2.4) - 0.055
  return Math.round(srgb * 65535)
}

function smooth(previous, target, attack, release) {
  return previous + (target - previous) * (target >= previous ? attack : release)
}

// state: { previous: { channelId: {r,g,b} } } kept by the caller between frames.
// Returns [{ id, r, g, b }] with 16-bit sRGB components.
function process(state, pixels, width, height, channels, settings) {
  var p = preset(settings.mode, settings.intensity)
  var gain = Math.pow(2, p.exposure) * Math.max(0, Math.min(100, Number(settings.brightness))) / 100
  var out = []
  for (var c = 0; c < channels.length; c++) {
    var channel = channels[c]
    var s = sample(pixels, width, height, channel, p.radius)
    var target = {
      r: Math.min(1, s.r * gain),
      g: Math.min(1, s.g * gain),
      b: Math.min(1, s.b * gain)
    }
    if (Math.max(target.r, target.g, target.b) <= p.blackCutoff) target = { r: 0, g: 0, b: 0 }
    var previous = state.previous[channel.id] || { r: 0, g: 0, b: 0 }
    var next = {
      r: smooth(previous.r, target.r, p.attack, p.release),
      g: smooth(previous.g, target.g, p.attack, p.release),
      b: smooth(previous.b, target.b, p.attack, p.release)
    }
    state.previous[channel.id] = next
    out.push({ id: channel.id, r: toRgb16(next.r), g: toRgb16(next.g), b: toRgb16(next.b) })
  }
  return out
}

function createState() {
  return { previous: {} }
}

if (typeof module !== "undefined") module.exports = {
  MODES: MODES, INTENSITIES: INTENSITIES, TARGET_FPS: TARGET_FPS, LINEAR: LINEAR,
  preset: preset, frameSize: frameSize, sample: sample, toRgb16: toRgb16,
  process: process, createState: createState
}
