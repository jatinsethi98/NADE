//
//  InteractivePopGesture.swift
//  NADE
//
//  Swipe-from-the-edge to go back, with the navigation bar hidden.
//
//  Every screen in this app draws its own nav bar — `docs/DESIGN.md` gives 1e,
//  1f, 1g and 1k exact top paddings and their own back affordances — so the
//  system bar is hidden throughout. UIKit disables
//  `interactivePopGestureRecognizer` whenever the bar is hidden, on the
//  reasonable assumption that a screen with no visible back button has no back.
//  Here that assumption is wrong: the back affordance is drawn, it is simply
//  not UIKit's.
//
//  Measured rather than assumed: `ThreadNavigationUITests.testTheSwipeBackGestureStillPops`
//  failed before this existed, with both an edge drag and `swipeRight()`.
//
//  The delegate is ours rather than `nil` on purpose. Clearing it re-enables
//  the gesture at the **root** too, where there is nothing to pop; UIKit has
//  historically deadlocked the navigation stack when that gesture completes.
//  `viewControllers.count > 1` is the whole guard.
//

import SwiftUI
import UIKit

extension View {
    /// Restores the edge-swipe on a screen whose navigation bar is hidden.
    func nadeInteractivePopGesture() -> some View {
        background(InteractivePopGestureEnabler().frame(width: 0, height: 0))
    }
}

private struct InteractivePopGestureEnabler: UIViewControllerRepresentable {

    /// **One, for the process.** `UINavigationController` holds its gesture's
    /// delegate weakly, and every destination was installing its own — so
    /// popping the top screen deallocated the delegate that the *next* screen's
    /// gesture was still relying on, leaving the recogniser enabled with a nil
    /// delegate at the root. That is the state the guard exists to prevent: a
    /// completed edge swipe with nothing to pop has historically wedged the
    /// stack. A shared delegate cannot go away underneath a screen that is
    /// still on it.
    @MainActor
    final class PopGestureGuard: NSObject, UIGestureRecognizerDelegate {
        static let shared = PopGestureGuard()

        nonisolated func gestureRecognizerShouldBegin(_ gestureRecognizer: UIGestureRecognizer) -> Bool {
            MainActor.assumeIsolated {
                guard let navigation = gestureRecognizer.view?.next(of: UINavigationController.self) else {
                    return false
                }
                return navigation.viewControllers.count > 1
            }
        }
    }

    func makeUIViewController(context: Context) -> UIViewController {
        Holder()
    }

    func updateUIViewController(_ controller: UIViewController, context: Context) {}

    /// The enabling has to happen after the controller joins a hierarchy, so it
    /// runs from `didMove(toParent:)` rather than from `makeUIViewController`.
    final class Holder: UIViewController {
        override func didMove(toParent parent: UIViewController?) {
            super.didMove(toParent: parent)
            guard let gesture = navigationController?.interactivePopGestureRecognizer else { return }
            gesture.delegate = PopGestureGuard.shared
            gesture.isEnabled = true
        }
    }
}

private extension UIResponder {
    /// The nearest ancestor of a type, walking the responder chain.
    func next<T: UIResponder>(of type: T.Type) -> T? {
        var responder: UIResponder? = self
        while let current = responder {
            if let match = current as? T { return match }
            responder = current.next
        }
        return nil
    }
}
