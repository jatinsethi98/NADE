//
//  OriginalMessageView.swift
//  NADE
//
//  "View original" — 1f's `body_html`, in a **locked** `WKWebView`.
//
//  PLAN.md §iOS app: "locked WKWebView (JS off, remote blocked, fixed height)".
//  Every one of those three is a security control, not a preference:
//
//  - **JS off.** Mail is attacker-controlled content. A script in it runs in a
//    context that can reach the app's own web state; there is no feature in
//    v1 that needs one.
//  - **Remote blocked.** A remote image is a tracking pixel that reports the
//    moment a message was opened, and a remote stylesheet or font leaks the
//    same. No `http:` or `https:` source of any kind is allowed, in the CSP or
//    in the navigation delegate.
//
//    The message's **own** inline parts do render, through `nade-inline:` and
//    the scheme handler below. This is subtler than it first looked: the server
//    has already rewritten every resolvable `cid:` to a relative
//    `/v1/messages/{id}/attachments/{id}` (`mail/html.rs`), so an `img-src cid:`
//    allowance — which is what this file shipped with — matched **nothing**,
//    and the relative URLs resolved against `about:blank` and were blocked by
//    `default-src 'none'`. Every inline image was a broken box, and the comment
//    claiming they were "served by our own authenticated proxy" was false:
//    `WKWebView` cannot attach a bearer token to a subresource load. The handler
//    is what makes that sentence true, and it is what actually closes
//    `docs/DESIGN.md` deviation 47.
//  - **Fixed height.** The web view lives inside a SwiftUI `ScrollView`; two
//    nested scrollers fight, so the content scrolls in the page and the frame
//    does not grow without bound.
//

import SwiftUI
import WebKit

struct OriginalMessageView: View {

    private enum M {
        static let height: CGFloat = 420
        static let top: CGFloat = 14
    }

    let html: String
    /// Where inline parts come from. The seam, not a closure.
    let source: any MailSource
    /// The message these inline parts belong to.
    let gmailID: String

    var body: some View {
        LockedWebView(html: html, gmailID: gmailID, source: source)
            .frame(height: M.height)
            .overlay {
                RoundedRectangle(cornerRadius: Theme.Radius.md)
                    .strokeBorder(Theme.Color.divider, lineWidth: Theme.Stroke.border)
            }
            .padding(.top, M.top)
            .accessibilityIdentifier("thread.original")
    }
}

private struct LockedWebView: UIViewRepresentable {

    let html: String
    let gmailID: String
    let source: any MailSource

    /// The only scheme with a handler. Anything else — `http:`, `https:`,
    /// `file:`, an unresolved `cid:` — has no way to load.
    static let scheme = "nade-inline"

    func makeCoordinator() -> Coordinator { Coordinator() }

    func makeUIView(context: Context) -> WKWebView {
        let configuration = WKWebViewConfiguration()
        // A fresh, non-persistent store: nothing this content sets survives it,
        // and it shares no cookies with the Gmail consent flow.
        configuration.websiteDataStore = .nonPersistent()
        configuration.defaultWebpagePreferences.allowsContentJavaScript = false
        configuration.allowsInlineMediaPlayback = false
        configuration.suppressesIncrementalRendering = true

        configuration.setURLSchemeHandler(InlineAttachmentHandler(gmailID: gmailID, source: source),
                                          forURLScheme: Self.scheme)

        let view = WKWebView(frame: .zero, configuration: configuration)
        view.navigationDelegate = context.coordinator
        // **Link previews are a third navigation path.** They do not go through
        // `decidePolicyFor`, so a long press on an attacker's `<a href>` makes
        // WebKit fetch the destination for a preview — the exact tracking
        // callback the CSP and the delegate both exist to stop. Off.
        view.allowsLinkPreview = false
        view.isOpaque = false
        view.backgroundColor = .clear
        view.scrollView.backgroundColor = .clear
        // The frame is fixed, so this is the scroller that moves.
        view.scrollView.bounces = false
        view.loadHTMLString(Self.wrapped(Self.rewritingInlineURLs(in: html)), baseURL: nil)
        return view
    }

    func updateUIView(_ view: WKWebView, context: Context) {}

    /// A `Content-Security-Policy` in the document itself, because
    /// `loadHTMLString` has no response headers to carry one.
    ///
    /// `default-src 'none'` blocks everything and the allowances are added back
    /// one at a time: inline styles, because mail is styled inline and stripping
    /// it would defeat the point of showing the original, and `cid:` images,
    /// which are the parts the message actually carried. No `http:` or `https:`
    /// source of any kind appears here — that is the tracking-pixel block, and
    /// it is enforced by the CSP as well as by the navigation delegate below,
    /// so a bug in one does not open the other.
    static func wrapped(_ html: String) -> String {
        """
        <!doctype html>
        <html><head>
        <meta charset="utf-8">
        <meta name="viewport" content="width=device-width, initial-scale=1">
        <meta http-equiv="Content-Security-Policy" content="\
        default-src 'none'; \
        img-src nade-inline:; \
        style-src 'unsafe-inline'; \
        form-action 'none'; \
        base-uri 'none'">
        <style>
          html { -webkit-text-size-adjust: 100%; }
          body { margin: 12px; font: 14px -apple-system, sans-serif; \
                 word-break: break-word; background: transparent; }
          img, table { max-width: 100% !important; height: auto; }
        </style>
        </head><body>\(html)</body></html>
        """
    }

    /// `/v1/messages/{gmail_id}/attachments/{att_id}` → `nade-inline:` .
    ///
    /// A plain string rewrite rather than an HTML parse: the only thing being
    /// changed is a URL prefix the *server* wrote, in a document that is about
    /// to be handed to a renderer with `default-src 'none'`. Parsing it here
    /// would add a second HTML implementation to the client for no gain.
    static func rewritingInlineURLs(in html: String) -> String {
        html.replacingOccurrences(of: "/v1/messages/", with: "\(scheme):/v1/messages/")
    }

    final class Coordinator: NSObject, WKNavigationDelegate {
        /// Only the initial `loadHTMLString` is allowed to commit.
        ///
        /// Everything else — a `<meta refresh>`, a form post, an iframe, a tap
        /// on a link — is cancelled. A link is *not* opened in Safari either:
        /// v1 takes no outbound action and a mail link is the oldest phishing
        /// vector there is, so the honest behaviour is that it does nothing.
        private var hasLoaded = false

        func webView(
            _ webView: WKWebView,
            decidePolicyFor navigationAction: WKNavigationAction,
            decisionHandler: @escaping (WKNavigationActionPolicy) -> Void
        ) {
            guard !hasLoaded, navigationAction.request.url?.scheme == "about" else {
                decisionHandler(.cancel)
                return
            }
            hasLoaded = true
            decisionHandler(.allow)
        }
    }
}

/// Serves the message's own attachment parts, and nothing else.
///
/// The URL it is handed always came from `rewritingInlineURLs`, so the path is
/// the server's own `/v1/messages/{gmail_id}/attachments/{att_id}`. It is still
/// parsed rather than trusted: the `gmail_id` in the document must match the
/// message being rendered, or one mail could pull another's attachments.
private final class InlineAttachmentHandler: NSObject, WKURLSchemeHandler {

    private let gmailID: String
    private let source: any MailSource
    private var tasks: [ObjectIdentifier: Task<Void, Never>] = [:]

    init(gmailID: String, source: any MailSource) {
        self.gmailID = gmailID
        self.source = source
    }

    func webView(_ webView: WKWebView, start task: any WKURLSchemeTask) {
        guard let parts = Self.parse(task.request.url), parts.gmailID == gmailID else {
            task.didFailWithError(URLError(.unsupportedURL))
            return
        }
        let source = source
        tasks[ObjectIdentifier(task)] = Task { [weak self] in
            do {
                let data = try await source.attachmentData(gmailID: parts.gmailID,
                                                           attachmentID: parts.attachmentID)
                guard !Task.isCancelled else { return }
                // A deliberately narrow content type. The bytes are rendered as
                // an image or not at all; letting the server name the type would
                // let a mail part be interpreted as something scriptable.
                let response = URLResponse(url: task.request.url!,
                                           mimeType: Self.mimeType(of: data),
                                           expectedContentLength: data.count,
                                           textEncodingName: nil)
                task.didReceive(response)
                task.didReceive(data)
                task.didFinish()
            } catch {
                guard !Task.isCancelled else { return }
                task.didFailWithError(error)
            }
            self?.tasks[ObjectIdentifier(task)] = nil
        }
    }

    func webView(_ webView: WKWebView, stop task: any WKURLSchemeTask) {
        tasks[ObjectIdentifier(task)]?.cancel()
        tasks[ObjectIdentifier(task)] = nil
    }

    static func parse(_ url: URL?) -> (gmailID: String, attachmentID: String)? {
        guard let url else { return nil }
        let parts = url.path.split(separator: "/").map(String.init)
        // ["v1", "messages", <gmail id>, "attachments", <attachment id>]
        guard parts.count == 5, parts[0] == "v1", parts[1] == "messages",
              parts[3] == "attachments" else { return nil }
        return (parts[2], parts[4])
    }

    /// Sniffed from the bytes, never taken from the message.
    ///
    /// EDGE: anything that is not a picture we recognise is served as
    /// `application/octet-stream`, which an `<img>` simply fails to render —
    /// the safe outcome for a part whose declared type cannot be trusted.
    static func mimeType(of data: Data) -> String {
        let signatures: [(bytes: [UInt8], type: String)] = [
            ([0x89, 0x50, 0x4E, 0x47], "image/png"),
            ([0xFF, 0xD8, 0xFF], "image/jpeg"),
            ([0x47, 0x49, 0x46, 0x38], "image/gif"),
            ([0x52, 0x49, 0x46, 0x46], "image/webp"),
        ]
        for signature in signatures where data.count >= signature.bytes.count {
            if Array(data.prefix(signature.bytes.count)) == signature.bytes { return signature.type }
        }
        return "application/octet-stream"
    }
}
