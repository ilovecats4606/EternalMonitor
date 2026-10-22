import XCTest

/// Debug builds read `EM_UPDATE_MANIFEST` instead of the website, so these
/// tests never touch the network.
final class UpdateBannerTests: XCTestCase {
    private let manifest = #"{"schema":1,"ipad":{"version":"9.9.9","build":999,"testflight":"https://testflight.apple.com/join/ppc9QfXc"}}"#

    override func setUpWithError() throws {
        continueAfterFailure = false
    }

    private func launch(manifest: String?, resetDismissal: Bool = true) -> XCUIApplication {
        let app = XCUIApplication()
        app.launchArguments = ["-didSeeOnboarding", "YES", "-allowUSB", "NO", "-checkForUpdates", "YES"]
        if resetDismissal { app.launchArguments += ["-updateDismissedBuild", "0"] }
        if let manifest { app.launchEnvironment["EM_UPDATE_MANIFEST"] = manifest }
        app.launch()
        return app
    }

    private func attach(_ app: XCUIApplication, _ name: String) {
        let screenshot = XCTAttachment(screenshot: app.screenshot())
        screenshot.name = name
        screenshot.lifetime = .keepAlways
        add(screenshot)
    }

    func testBannerAndSettingsRowOpenTestFlight() {
        let app = launch(manifest: manifest)
        let banner = app.otherElements["update.banner"]
        XCTAssertTrue(banner.waitForExistence(timeout: 10))
        XCTAssertTrue(app.staticTexts["A new version is ready in TestFlight"].exists)
        XCTAssertTrue(app.staticTexts.containing(NSPredicate(format: "label CONTAINS %@", "Version 9.9.9, build 999")).firstMatch.exists)
        XCTAssertTrue(app.buttons["update.openTestFlight"].exists)
        attach(app, "update-banner")

        app.buttons["settings.button"].tap()
        XCTAssertTrue(app.navigationBars["Settings"].waitForExistence(timeout: 5))
        let row = app.buttons["settings.openTestFlight"]
        for _ in 0..<8 where !row.isHittable {
            app.swipeUp()
        }
        XCTAssertTrue(row.isHittable)
        XCTAssertTrue(app.switches["settings.checkForUpdates"].exists)
        attach(app, "update-settings")

        // Turning automatic checks off hides the update everywhere.
        app.switches["settings.checkForUpdates"].coordinate(withNormalizedOffset: CGVector(dx: 0.93, dy: 0.5)).tap()
        XCTAssertFalse(row.waitForExistence(timeout: 1))
        app.buttons["settings.done"].tap()
        XCTAssertFalse(banner.waitForExistence(timeout: 2))
    }

    func testDismissHidesTheBanner() {
        let app = launch(manifest: manifest)
        let banner = app.otherElements["update.banner"]
        XCTAssertTrue(banner.waitForExistence(timeout: 10))
        app.buttons["update.dismiss"].tap()
        XCTAssertFalse(banner.waitForExistence(timeout: 2))
        app.terminate()
        let relaunched = launch(manifest: manifest, resetDismissal: false)
        XCTAssertTrue(relaunched.buttons["settings.button"].waitForExistence(timeout: 10))
        XCTAssertFalse(relaunched.otherElements["update.banner"].waitForExistence(timeout: 3),
                       "dismissed for a day, also after relaunch")
    }

    func testNoBannerWithoutAnUpdate() {
        // An older build in the manifest also clears what an earlier test stored.
        let app = launch(manifest: #"{"schema":1,"ipad":{"version":"0.0.1","build":1}}"#)
        XCTAssertTrue(app.buttons["settings.button"].waitForExistence(timeout: 10))
        XCTAssertFalse(app.otherElements["update.banner"].exists)
    }

    func testMismatchOffersTestFlightOnTheIPad() {
        let app = XCUIApplication()
        app.launchArguments = ["-didSeeOnboarding", "YES", "-allowUSB", "NO"]
        app.launchEnvironment["EM_UI_PREVIEW"] = "mismatch"
        app.launch()
        XCTAssertTrue(app.buttons["mismatch.openTestFlight"].waitForExistence(timeout: 10))
        XCTAssertTrue(app.staticTexts.containing(NSPredicate(format: "label CONTAINS %@", "Update the iPad app: open TestFlight")).firstMatch.exists)
        attach(app, "version-mismatch")
    }
}
