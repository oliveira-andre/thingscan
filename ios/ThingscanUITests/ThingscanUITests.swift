import XCTest

/// End-to-end checks against real servers, saving a screenshot of each screen to the folder
/// TEST_RUNNER_SHOTS names. Each test is skipped unless its servers are given:
/// - `testPairSyncAndBrowse`: the Mac's direct pairing; TEST_RUNNER_MAC_ADDRESS and
///   TEST_RUNNER_MAC_CODE (the code the Mac is showing).
/// - `testAccountSync`: a Thingscan account server; TEST_RUNNER_ACCOUNT_SERVER,
///   TEST_RUNNER_ACCOUNT_EMAIL and TEST_RUNNER_ACCOUNT_PASSWORD for an account a Mac has
///   filled. It renames the TV here, then expects the Mac to rename the A/C to "Bedroom A/C".
final class ThingscanUITests: XCTestCase {
    private let env = ProcessInfo.processInfo.environment
    private let app = XCUIApplication()

    override func setUp() {
        continueAfterFailure = false
    }

    func testPairSyncAndBrowse() throws {
        guard let address = env["MAC_ADDRESS"], let code = env["MAC_CODE"] else {
            throw XCTSkip("set TEST_RUNNER_MAC_ADDRESS and TEST_RUNNER_MAC_CODE")
        }
        app.launch()
        shot("01-home-empty")

        app.buttons["Account"].tap()
        shot("02-account")
        tap(app.buttons.matching(identifier: "Pair with Mac"))
        sleep(4) // Bonjour browse
        shot("03-pair-found")
        // With exactly one Mac found the sheet skips to the code; go back and type the address instead.
        if app.buttons["Change"].exists { app.buttons["Change"].tap() }
        let addressField = app.textFields["192.168.1.10:47640"]
        addressField.tap()
        addressField.typeText(address)
        app.buttons["Next"].tap()
        let codeField = app.textFields["000000"]
        XCTAssertTrue(codeField.waitForExistence(timeout: 3))
        codeField.tap()
        codeField.typeText(code)
        shot("04-pair-code")
        app.buttons["Pair"].tap()
        XCTAssertTrue(text(beginningWith: "Synced").waitForExistence(timeout: 20), dump())
        sleep(2)
        shot("05-account-paired")
        app.buttons["Done"].tap()

        sleep(6) // reachability checks
        shot("06-home-devices")

        // The Mac may serve no TV (to leave a real one alone).
        let tv = app.buttons.matching(NSPredicate(format: "label BEGINSWITH %@", "Tv,")).firstMatch
        if tv.exists {
            tv.tap()
            // A TV that's off ends in Retry (and Turn on, through SmartThings); one that's on connects.
            _ = app.buttons["Retry"].waitForExistence(timeout: 15)
            shot("07-tv-remote")
            app.buttons["Touchpad"].tap()
            sleep(1)
            shot("08-tv-touchpad")
            app.buttons["Buttons"].tap()
            app.navigationBars.buttons.element(boundBy: 0).tap()
        }

        try card("Air Conditioner").tap()
        sleep(6) // SmartThings status
        shot("09-ac-controls")
        app.swipeUp()
        shot("10-ac-controls-more")
        app.navigationBars.buttons.element(boundBy: 0).tap()

        tap(app.buttons.matching(identifier: "Add device"))
        allowAlerts()
        sleep(9)
        allowAlerts()
        shot("11-add-scan")
        app.swipeUp()
        shot("12-add-scan-more")
        app.buttons["Done"].tap()

        try card("Air Conditioner").press(forDuration: 1)
        app.buttons["Edit name and room"].tap()
        sleep(1)
        shot("13-edit")
        app.buttons["Cancel"].tap()
    }

    func testAccountSync() throws {
        guard let server = env["ACCOUNT_SERVER"], let email = env["ACCOUNT_EMAIL"], let password = env["ACCOUNT_PASSWORD"] else {
            throw XCTSkip("set TEST_RUNNER_ACCOUNT_SERVER, TEST_RUNNER_ACCOUNT_EMAIL and TEST_RUNNER_ACCOUNT_PASSWORD")
        }
        app.launch()
        app.buttons["Account"].tap()
        tap(app.buttons.matching(identifier: "Sign in"))
        let serverField = app.textFields["sync.example.com"]
        XCTAssertTrue(serverField.waitForExistence(timeout: 3), dump())
        serverField.tap()
        serverField.typeText(server)
        app.textFields["you@example.com"].tap()
        app.textFields["you@example.com"].typeText(email)
        app.secureTextFields["Password"].tap()
        app.secureTextFields["Password"].typeText(password)
        shot("a1-sign-in")
        app.secureTextFields["Password"].typeText("\n") // Return signs in
        XCTAssertTrue(text(beginningWith: "Synced").waitForExistence(timeout: 30), dump())
        sleep(2)
        shot("a2-account-signed-in")
        // AutoFill offers to save the password from a process the test can't query: tap where
        // its "Not Now" is.
        if !app.buttons["Done"].isHittable {
            app.coordinate(withNormalizedOffset: CGVector(dx: 0.32, dy: 0.63)).tap()
            sleep(1)
        }
        app.buttons["Done"].tap()
        sleep(2) // the sheet closing

        // The Mac's devices arrived; rename the TV here.
        try card("Tv").press(forDuration: 1)
        app.buttons["Edit name and room"].tap()
        let name = app.textFields["Name"]
        XCTAssertTrue(name.waitForExistence(timeout: 3))
        name.doubleTap()
        name.typeText("Living room TV")
        shot("a3-rename")
        tap(app.buttons.matching(identifier: "Save"))
        sleep(4) // the change syncs 1.5 s after it's made
        shot("a4-home-renamed")

        // The Mac renames the A/C once it sees ours; pull to sync until it shows up.
        let renamed = app.buttons.matching(NSPredicate(format: "label BEGINSWITH %@", "Bedroom A/C,")).firstMatch
        for _ in 0..<12 where !renamed.exists {
            app.swipeDown()
            sleep(4)
        }
        XCTAssertTrue(renamed.exists, dump())
        shot("a5-home-from-mac")
        app.buttons["Account"].tap()
        sleep(1)
        shot("a6-account-after")
    }

    /// The form sheets, with no server needed: sign in, and adding a device the scan finds on
    /// this network (cancelled, so nothing is added).
    func testSheets() throws {
        app.launch()
        app.buttons["Account"].tap()
        tap(app.buttons.matching(identifier: "Sign in"))
        sleep(1)
        shot("s1-sign-in")
        app.buttons["Cancel"].tap()
        sleep(1)
        app.buttons["Done"].tap()
        sleep(1)
        tap(app.buttons.matching(identifier: "Add device"))
        allowAlerts()
        let add = app.buttons.matching(identifier: "Add").firstMatch
        guard add.waitForExistence(timeout: 15) else { throw XCTSkip("nothing found nearby to add") }
        add.tap()
        sleep(1)
        shot("s2-add-sheet")
        app.buttons["Cancel"].tap()
    }

    private func card(_ name: String) throws -> XCUIElement {
        let card = app.buttons.matching(NSPredicate(format: "label BEGINSWITH %@", name + ",")).firstMatch
        XCTAssertTrue(card.waitForExistence(timeout: 5), dump())
        return card
    }

    /// The one on top: views under a sheet stay in the tree.
    private func tap(_ query: XCUIElementQuery) {
        guard let element = query.allElementsBoundByIndex.first(where: \.isHittable) else {
            return XCTFail("nothing hittable; \(dump())")
        }
        element.tap()
    }

    private func text(beginningWith prefix: String) -> XCUIElement {
        app.staticTexts.matching(NSPredicate(format: "label BEGINSWITH %@", prefix)).firstMatch
    }

    /// Bluetooth and local network prompts come from SpringBoard.
    private func allowAlerts() {
        let springboard = XCUIApplication(bundleIdentifier: "com.apple.springboard")
        for label in ["Allow", "OK"] where springboard.buttons[label].exists {
            springboard.buttons[label].tap()
        }
    }

    private func shot(_ name: String) {
        let screenshot = XCUIScreen.main.screenshot()
        let attachment = XCTAttachment(screenshot: screenshot)
        attachment.name = name
        attachment.lifetime = .keepAlways
        add(attachment)
        if let dir = env["SHOTS"] {
            try? screenshot.pngRepresentation.write(to: URL(fileURLWithPath: dir).appendingPathComponent("\(name).png"))
        }
    }

    /// The element tree, to see what a query should have matched.
    private func dump() -> String {
        if let dir = env["SHOTS"] {
            try? app.debugDescription.write(toFile: dir + "/tree.txt", atomically: true, encoding: .utf8)
        }
        return "see tree.txt"
    }
}
