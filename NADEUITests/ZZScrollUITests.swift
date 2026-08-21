import XCTest

/// TEMPORARY look-at helper — delete before commit. Scrolls a screen and then
/// holds it still long enough for `xcrun simctl io … screenshot` to catch it.
final class ZZScrollUITests: XCTestCase {
    func testHoldScrolled1e() {
        let app = XCUIApplication.nade(seed: .fixtures, screen: "1e", mailbox: "INBOX")
        _ = app.staticTexts["maillist.title"].waitForExistence(timeout: 10)
        app.swipeUp()
        app.swipeUp()
        print("=====HELD=====")
        sleep(25)
    }

    func testHoldScrolled2a() {
        let app = XCUIApplication.nade(seed: .fixtures, screen: "2a")
        _ = app.staticTexts["home.date"].waitForExistence(timeout: 10)
        app.swipeUp()
        app.swipeUp()
        print("=====HELD=====")
        sleep(25)
    }
}
