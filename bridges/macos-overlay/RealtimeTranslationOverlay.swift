import AppKit
import WebKit

private let overlayWidth: CGFloat = 760
private let overlayHeight: CGFloat = 286
private let bottomMargin: CGFloat = 28
private let frameAutosaveName = "RealtimeTranslationOverlayFrame"

final class OverlayPanel: NSPanel {
    override var canBecomeKey: Bool { true }
    override var canBecomeMain: Bool { false }
}

final class DragHandleView: NSView {
    override func mouseDown(with event: NSEvent) {
        window?.performDrag(with: event)
    }

    override func resetCursorRects() {
        addCursorRect(bounds, cursor: .openHand)
    }
}

final class ResizeHandleView: NSView {
    private var initialFrame: NSRect?
    private var initialMouseLocation: NSPoint?

    override func mouseDown(with event: NSEvent) {
        initialFrame = window?.frame
        initialMouseLocation = NSEvent.mouseLocation
    }

    override func mouseDragged(with event: NSEvent) {
        guard let window, let initialFrame, let initialMouseLocation else { return }
        let currentMouseLocation = NSEvent.mouseLocation
        let deltaX = currentMouseLocation.x - initialMouseLocation.x
        let deltaY = currentMouseLocation.y - initialMouseLocation.y
        let width = min(
            window.maxSize.width,
            max(window.minSize.width, initialFrame.width + deltaX)
        )
        let height = min(
            window.maxSize.height,
            max(window.minSize.height, initialFrame.height - deltaY)
        )
        let frame = NSRect(
            x: initialFrame.minX,
            y: initialFrame.maxY - height,
            width: width,
            height: height
        )
        window.setFrame(frame, display: true)
    }

    override func mouseUp(with event: NSEvent) {
        initialFrame = nil
        initialMouseLocation = nil
    }

    override func resetCursorRects() {
        addCursorRect(bounds, cursor: .crosshair)
    }

    override func draw(_ dirtyRect: NSRect) {
        NSColor(white: 0.82, alpha: 0.42).setStroke()
        let path = NSBezierPath()
        path.lineWidth = 1.2
        for offset: CGFloat in [5, 9, 13] {
            path.move(to: NSPoint(x: bounds.maxX - offset, y: 3))
            path.line(to: NSPoint(x: bounds.maxX - 3, y: offset))
        }
        path.stroke()
    }
}

final class AppDelegate: NSObject, NSApplicationDelegate, WKNavigationDelegate {
    private var panel: OverlayPanel?
    private var webView: WKWebView?

    func applicationDidFinishLaunching(_ notification: Notification) {
        NSApp.setActivationPolicy(.accessory)

        let panel = OverlayPanel(
            contentRect: NSRect(x: 0, y: 0, width: overlayWidth, height: overlayHeight),
            styleMask: [.borderless, .nonactivatingPanel, .resizable],
            backing: .buffered,
            defer: false
        )
        panel.isOpaque = false
        panel.backgroundColor = .clear
        panel.hasShadow = false
        panel.sharingType = .readOnly
        panel.hidesOnDeactivate = false
        panel.level = .floating
        panel.collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary, .transient]
        panel.isMovableByWindowBackground = false
        panel.minSize = NSSize(width: 440, height: 240)
        panel.maxSize = NSSize(width: 1_200, height: 640)
        panel.isReleasedWhenClosed = false

        let container = NSView(frame: panel.contentView?.bounds ?? .zero)
        container.wantsLayer = true
        container.layer?.backgroundColor = NSColor.clear.cgColor
        container.autoresizingMask = [.width, .height]

        let configuration = WKWebViewConfiguration()
        configuration.websiteDataStore = .nonPersistent()
        let webView = WKWebView(frame: container.bounds, configuration: configuration)
        webView.autoresizingMask = [.width, .height]
        webView.navigationDelegate = self
        webView.wantsLayer = true
        webView.layer?.backgroundColor = NSColor.clear.cgColor
        webView.setValue(false, forKey: "drawsBackground")
        container.addSubview(webView)

        let dragHandle = DragHandleView(
            frame: NSRect(x: 34, y: overlayHeight - 27, width: overlayWidth - 230, height: 25)
        )
        dragHandle.autoresizingMask = [.width, .minYMargin]
        container.addSubview(dragHandle)

        let closeButton = NSButton(
            frame: NSRect(x: overlayWidth - 37, y: overlayHeight - 29, width: 24, height: 24)
        )
        closeButton.autoresizingMask = [.minXMargin, .minYMargin]
        closeButton.title = "×"
        closeButton.isBordered = false
        closeButton.font = .systemFont(ofSize: 18, weight: .regular)
        closeButton.contentTintColor = NSColor(white: 0.76, alpha: 0.75)
        closeButton.target = self
        closeButton.action = #selector(closeOverlay)
        closeButton.toolTip = "关闭悬浮字幕"
        container.addSubview(closeButton)

        let resizeHandle = ResizeHandleView(
            frame: NSRect(x: overlayWidth - 23, y: 2, width: 19, height: 19)
        )
        resizeHandle.autoresizingMask = [.minXMargin, .maxYMargin]
        container.addSubview(resizeHandle)

        panel.contentView = container
        if !panel.setFrameUsingName(frameAutosaveName) {
            position(panel)
        }
        panel.setFrameAutosaveName(frameAutosaveName)
        self.panel = panel
        self.webView = webView
        panel.orderFrontRegardless()

        let urlString = CommandLine.arguments.dropFirst().first
            ?? "http://127.0.0.1:8765/overlay"
        guard let url = URL(string: urlString) else {
            showFallback("悬浮字幕地址无效")
            return
        }
        webView.load(URLRequest(url: url))
    }

    func applicationShouldHandleReopen(
        _ sender: NSApplication,
        hasVisibleWindows flag: Bool
    ) -> Bool {
        panel?.orderFrontRegardless()
        webView?.reload()
        return true
    }

    func webView(
        _ webView: WKWebView,
        didFail navigation: WKNavigation!,
        withError error: Error
    ) {
        showFallback("无法连接实时翻译服务")
    }

    func webView(
        _ webView: WKWebView,
        didFailProvisionalNavigation navigation: WKNavigation!,
        withError error: Error
    ) {
        showFallback("无法连接实时翻译服务")
    }

    @objc private func closeOverlay() {
        NSApp.terminate(nil)
    }

    private func position(_ panel: NSPanel) {
        let mouse = NSEvent.mouseLocation
        let screen = NSScreen.screens.first { NSMouseInRect(mouse, $0.frame, false) }
            ?? NSScreen.main
        guard let visibleFrame = screen?.visibleFrame else { return }
        let origin = NSPoint(
            x: visibleFrame.midX - overlayWidth / 2,
            y: visibleFrame.minY + bottomMargin
        )
        panel.setFrameOrigin(origin)
    }

    private func showFallback(_ message: String) {
        guard let webView else { return }
        let escaped = message
            .replacingOccurrences(of: "&", with: "&amp;")
            .replacingOccurrences(of: "<", with: "&lt;")
        webView.loadHTMLString(
            """
            <style>
              html,body{background:transparent;margin:0;color:white;font:14px -apple-system}
              div{margin:28px 10px 10px;padding:28px;border:1px solid #ffffff24;border-radius:18px;
                  background:#11141dd9;text-align:center;box-shadow:0 14px 45px #0006}
            </style><div>\(escaped)</div>
            """,
            baseURL: nil
        )
    }
}

let application = NSApplication.shared
let delegate = AppDelegate()
application.delegate = delegate
application.run()
