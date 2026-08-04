import XCTest

@MainActor
final class HoloIrohRemoteInputWitnessTests: XCTestCase {
    private let liveSurfaceLabel = "Live remote view of the Mac"
    private let controlBannerLabel = "You're in control — the agent is paused"
    private var launchedApp: XCUIApplication?

    override func tearDown() {
        launchedApp?.terminate()
        launchedApp = nil
        super.tearDown()
    }

    func testPublicPointerAPIsRunOnIPadSimulator() throws {
        let app = XCUIApplication()
        app.launchArguments += ["-AppleLanguages", "(en)", "-AppleLocale", "en_US"]
        app.launch()
        launchedApp = app

        let window = app.windows.firstMatch
        XCTAssertTrue(window.waitForExistence(timeout: 20))
        let center = window.coordinate(withNormalizedOffset: CGVector(dx: 0.50, dy: 0.50))

        XCTContext.runActivity(named: "Public pointer hover") { _ in
            center.hover()
        }

        XCTContext.runActivity(named: "Public secondary click") { _ in
            center.rightClick()
        }

        attachScreenshot(named: "public-pointer-apis-complete", from: app)
    }

    func testPinchAndPanUseTheLiveGestureSurface() throws {
        let app = XCUIApplication()
        app.launchEnvironment["HOLOIROH_WITNESS_GESTURE_SURFACE"] = "1"
        app.launchArguments += ["-AppleLanguages", "(en)", "-AppleLocale", "en_US"]
        app.launch()
        launchedApp = app

        let localGestureSurface = matchingLabel(liveSurfaceLabel, in: app)
        XCTAssertTrue(localGestureSurface.waitForExistence(timeout: 30))
        XCTAssertTrue(waitUntilHittable(localGestureSurface, timeout: 10))

        localGestureSurface.pinch(withScale: 1.8, velocity: 1.0)

        let zoomBadge = app.descendants(matching: .any)
            .matching(NSPredicate(format: "label BEGINSWITH %@", "Zoom "))
            .firstMatch
        XCTAssertTrue(
            zoomBadge.waitForExistence(timeout: 10),
            "The pinch did not expose PanZoomVideoSurface's Zoom accessibility badge."
        )

        attachScreenshot(named: "pinch-committed-zoom", from: app)

        let start = localGestureSurface.coordinate(withNormalizedOffset: CGVector(dx: 0.65, dy: 0.50))
        let end = localGestureSurface.coordinate(withNormalizedOffset: CGVector(dx: 0.35, dy: 0.65))
        start.press(forDuration: 0.10, thenDragTo: end)

        XCTAssertTrue(zoomBadge.exists)
        attachScreenshot(named: "pan-after-zoom", from: app)
    }

    func testDryRunRemoteKeyboardTouchAndPointerInput() throws {
        guard ProcessInfo.processInfo.environment["HOLOIROH_WITNESS_INPUT_DRY_RUN_CONFIRMED"] == "1" else {
            throw XCTSkip(
                "Set HOLOIROH_WITNESS_INPUT_DRY_RUN_CONFIRMED=1 only when the daemon runs with HOLOIROH_INPUT_DRY_RUN=1."
            )
        }

        let app = try launchConnectedApp()
        let surface = try waitForControlledSurface(in: app)
        let center = surface.coordinate(withNormalizedOffset: CGVector(dx: 0.50, dy: 0.50))
        let dragEnd = surface.coordinate(withNormalizedOffset: CGVector(dx: 0.65, dy: 0.60))

        XCTContext.runActivity(named: "Primary touch click") { _ in
            surface.tap()
        }

        XCTContext.runActivity(named: "Primary touch drag") { _ in
            center.press(forDuration: 0.10, thenDragTo: dragEnd)
        }

        XCTContext.runActivity(named: "Primary pointer click") { _ in
            center.click()
        }

        XCTContext.runActivity(named: "Primary pointer drag") { _ in
            center.click(forDuration: 0.10, thenDragTo: dragEnd)
        }

        XCTContext.runActivity(named: "Secondary pointer click") { _ in
            center.rightClick()
        }

        XCTContext.runActivity(named: "Pointer hover") { _ in
            center.hover()
        }

        let sentinel = "holo-witness-\(UUID().uuidString.prefix(8))"
        XCTContext.runActivity(named: "Hardware-style keyboard typeText") { activity in
            let metadata = XCTAttachment(string: "sentinel=\(sentinel)")
            metadata.name = "keyboard-sentinel"
            metadata.lifetime = .keepAlways
            activity.add(metadata)
            surface.typeText(sentinel)
        }

        XCTAssertEqual(surface.value as? String, "Remote control active")
        attachScreenshot(named: "remote-input-dispatch-complete", from: app)
    }

    private func launchConnectedApp() throws -> XCUIApplication {
        let environment = ProcessInfo.processInfo.environment
        let ticket = firstNonempty(
            environment["HOLOIROH_WITNESS_TICKET"],
            environment["HOLOIROH_AUTOPAIR_TICKET"]
        )
        let pin = firstNonempty(
            environment["HOLOIROH_WITNESS_PIN"],
            environment["HOLOIROH_AUTOPAIR_PIN"]
        )
        let useSavedProfile = environment["HOLOIROH_WITNESS_USE_SAVED_PROFILE"] == "1"

        if ticket == nil, pin == nil, !useSavedProfile {
            throw XCTSkip(
                "Set the witness ticket and PIN, or set HOLOIROH_WITNESS_USE_SAVED_PROFILE=1."
            )
        }
        if ticket == nil, pin != nil {
            throw XCTSkip("Set HOLOIROH_WITNESS_TICKET to the current daemon ticket.")
        }
        if ticket != nil, pin == nil {
            throw XCTSkip("Set HOLOIROH_WITNESS_PIN to the current daemon PIN.")
        }

        let app = XCUIApplication()
        if let ticket, let pin {
            app.launchEnvironment["HOLOIROH_AUTOPAIR_TICKET"] = ticket
            app.launchEnvironment["HOLOIROH_AUTOPAIR_PIN"] = pin
        }
        app.launchEnvironment["HOLOIROH_WITNESS_TAKE_CONTROL"] = "1"
        app.launchArguments += ["-AppleLanguages", "(en)", "-AppleLocale", "en_US"]
        app.launch()
        launchedApp = app
        return app
    }

    private func waitForControlledSurface(in app: XCUIApplication) throws -> XCUIElement {
        let surface = matchingLabel(liveSurfaceLabel, in: app)
        guard surface.waitForExistence(timeout: 60) else {
            throw WitnessFailure.timeout("The live remote view did not appear. Check the ticket, PIN, daemon, and bridge.")
        }
        guard waitUntilHittable(surface, timeout: 30) else {
            throw WitnessFailure.timeout("The live remote view appeared but never became hittable.")
        }

        let activeControl = NSPredicate(format: "value == %@", "Remote control active")
        if XCTWaiter.wait(
            for: [XCTNSPredicateExpectation(predicate: activeControl, object: surface)],
            timeout: 5
        ) != .completed {
            let takeControl = app.buttons["Take control of the Mac"]
            guard takeControl.waitForExistence(timeout: 10) else {
                throw WitnessFailure.timeout("Neither the take-control hook nor the take-control button became available.")
            }
            takeControl.tap()
        }
        guard XCTWaiter.wait(
            for: [XCTNSPredicateExpectation(predicate: activeControl, object: surface)],
            timeout: 10
        ) == .completed else {
            throw WitnessFailure.timeout("Take control did not activate remote input on the live surface.")
        }
        return surface
    }

    private func assertTakeControlDiagnostic(in app: XCUIApplication, surface: XCUIElement) throws {
        let controls = app.buttons["Toggle session controls"]
        guard controls.waitForExistence(timeout: 10) else {
            throw WitnessFailure.timeout("The session controls button did not appear.")
        }
        controls.tap()

        let diagnostic = matchingLabel("→ took control of the Mac", in: app)
        guard diagnostic.waitForExistence(timeout: 10) else {
            throw WitnessFailure.timeout("The app did not expose its take-control diagnostic.")
        }
        attachScreenshot(named: "take-control-app-diagnostic", from: app)

        app.swipeDown()
        guard waitUntilHittable(surface, timeout: 10) else {
            throw WitnessFailure.timeout("The live remote view did not become hittable after closing diagnostics.")
        }
    }

    private func matchingLabel(_ label: String, in app: XCUIApplication) -> XCUIElement {
        app.descendants(matching: .any)
            .matching(NSPredicate(format: "label == %@", label))
            .firstMatch
    }

    private func waitUntilHittable(_ element: XCUIElement, timeout: TimeInterval) -> Bool {
        let expectation = XCTNSPredicateExpectation(
            predicate: NSPredicate(format: "exists == true AND hittable == true"),
            object: element
        )
        return XCTWaiter.wait(for: [expectation], timeout: timeout) == .completed
    }

    private func firstNonempty(_ values: String?...) -> String? {
        values.compactMap { value in
            guard let value, !value.isEmpty else { return nil }
            return value
        }.first
    }

    private func attachScreenshot(named name: String, from app: XCUIApplication) {
        let attachment = XCTAttachment(screenshot: app.screenshot())
        attachment.name = name
        attachment.lifetime = .keepAlways
        add(attachment)
    }
}

private enum WitnessFailure: LocalizedError {
    case timeout(String)

    var errorDescription: String? {
        switch self {
        case .timeout(let message): message
        }
    }
}
