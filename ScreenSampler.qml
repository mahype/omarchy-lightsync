import QtQuick
import Quickshell
import Quickshell.Wayland
import "SyncEngine.js" as Engine

// Captures one screen without the ScreenCast portal. The window is a
// transparent 1 px background layer; the capture renders outside its visible
// area, is downsampled on the GPU (mipmaps) and read back through a Canvas.
PanelWindow {
  id: root

  property bool running: false
  readonly property var frameSize: Engine.frameSize(view.sourceSize.width, view.sourceSize.height)
  property bool grabbing: false
  property real grabStarted: 0

  signal frame(var pixels, int width, int height)
  signal failed()

  anchors { top: true; left: true }
  implicitWidth: 1
  implicitHeight: 1
  color: "transparent"
  exclusionMode: ExclusionMode.Ignore
  WlrLayershell.layer: WlrLayer.Background
  WlrLayershell.namespace: "omarchy-light-sync-hue-capture"
  mask: Region {}

  ScreencopyView {
    id: view
    x: 2
    width: root.frameSize.width
    height: root.frameSize.height
    captureSource: root.screen
    live: root.running
    paintCursor: false
    layer.enabled: true
    layer.mipmap: true
    layer.smooth: true
    layer.textureSize: Qt.size(width, height)
  }

  Image {
    id: grabbed
    x: 2
    width: root.frameSize.width
    height: root.frameSize.height
    cache: false
    onStatusChanged: {
      if (status === Image.Ready) canvas.read()
      else if (status === Image.Error) root.grabbing = false
    }
  }

  Canvas {
    id: canvas
    x: 2
    width: root.frameSize.width
    height: root.frameSize.height
    renderTarget: Canvas.Image
    renderStrategy: Canvas.Immediate

    function read() {
      var ctx = getContext("2d")
      root.grabbing = false
      if (!ctx || !root.running) return
      ctx.drawImage(grabbed, 0, 0, width, height)
      root.frame(ctx.getImageData(0, 0, width, height).data, width, height)
    }
  }

  Timer {
    interval: Math.round(1000 / Engine.TARGET_FPS)
    repeat: true
    running: root.running && view.hasContent
    onTriggered: {
      // A lost grab callback must not stall the stream.
      if (root.grabbing && Date.now() - root.grabStarted < 500) return
      root.grabbing = true
      root.grabStarted = Date.now()
      var size = root.frameSize
      var started = view.grabToImage(function(result) { grabbed.source = result.url },
        Qt.size(size.width, size.height))
      if (!started) root.grabbing = false
    }
  }

  // No content after a few seconds means the compositor refused the capture.
  Timer {
    interval: 5000
    running: root.running && !view.hasContent
    onTriggered: root.failed()
  }
}
