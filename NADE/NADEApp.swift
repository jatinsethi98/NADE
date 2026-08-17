//
//  NADEApp.swift
//  NADE
//

import SwiftUI

@main
struct NADEApp: App {
    var body: some Scene {
        WindowGroup {
            RootView()
                // DESIGN.md §1 Color: the design ships one visual world, and
                // dark mode is out of scope for v1. Forced app-wide so the
                // palette always resolves as designed. EDGE (E12).
                .preferredColorScheme(.light)
        }
    }
}

private struct RootView: View {
    var body: some View {
        #if DEBUG
        // EDGE (E14): the gallery exists only in DEBUG. A release build has no
        // path to it at all — this whole branch is compiled out.
        //
        //   xcrun simctl launch <device> fsaas.NADE -NADEGallery 1
        //   xcrun simctl launch <device> fsaas.NADE -NADEGallery 1 -NADEGallerySection buttons
        if LaunchOptions.showsGallery {
            GalleryView(initialSection: LaunchOptions.gallerySection)
        } else {
            RootTabView()
        }
        #else
        RootTabView()
        #endif
    }
}

#if DEBUG
enum LaunchOptions {
    /// `-NADEGallery 1`. UserDefaults picks up `-key value` launch-argument
    /// pairs, so `-NADEGallery 0` correctly means *off* — which a bare
    /// `arguments.contains` check would get wrong.
    static var showsGallery: Bool {
        UserDefaults.standard.bool(forKey: "NADEGallery")
    }

    /// `-NADEGallerySection <id>` scrolls straight to a section, so each
    /// screenshot is deterministic instead of depending on a swipe.
    static var gallerySection: String? {
        UserDefaults.standard.string(forKey: "NADEGallerySection")
    }
}
#endif
