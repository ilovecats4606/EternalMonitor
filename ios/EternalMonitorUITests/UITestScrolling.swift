import XCTest

extension XCUIApplication {
    /// Swipes up until `element` is on screen. Settings rows load lazily, so
    /// a row further down may not exist at all until it is scrolled near;
    /// a fixed number of swipes breaks whenever a section is added above.
    /// Returns once the list has stopped moving: a tap while it still
    /// decelerates only stops the scroll, and the element is never pressed.
    @discardableResult
    func swipeUp(toReveal element: XCUIElement, maxSwipes: Int = 12) -> Bool {
        for _ in 0..<maxSwipes {
            if element.exists && element.isHittable { return Self.settled(element) }
            swipeUp()
        }
        return element.exists && element.isHittable && Self.settled(element)
    }

    /// Whether `element` keeps the same frame across two polls and can be
    /// tapped, within `timeout`.
    private static func settled(_ element: XCUIElement, timeout: TimeInterval = 5) -> Bool {
        let deadline = Date().addingTimeInterval(timeout)
        var last: CGRect?
        while Date() < deadline {
            guard element.exists else { return false }
            let frame = element.frame
            if frame == last && element.isHittable { return true }
            last = frame
            Thread.sleep(forTimeInterval: 0.15)
        }
        return false
    }
}
