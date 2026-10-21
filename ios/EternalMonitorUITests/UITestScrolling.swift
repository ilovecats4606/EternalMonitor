import XCTest

extension XCUIApplication {
    /// Swipes up until `element` is on screen. Settings rows load lazily, so
    /// a row further down may not exist at all until it is scrolled near;
    /// a fixed number of swipes breaks whenever a section is added above.
    @discardableResult
    func swipeUp(toReveal element: XCUIElement, maxSwipes: Int = 12) -> Bool {
        for _ in 0..<maxSwipes {
            if element.exists && element.isHittable { return true }
            swipeUp()
        }
        return element.exists && element.isHittable
    }
}
