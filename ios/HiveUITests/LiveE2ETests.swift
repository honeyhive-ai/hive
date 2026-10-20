import XCTest

/// Drives the real app against a live `hive serve` (no mocks): pair → new chat →
/// `@agent` message → streamed reply → approve the proposal in Review → Lab →
/// send the agent to another host.
///
/// Skipped unless the runner gets the server's pairing link. Run with:
///
///     TEST_RUNNER_HIVE_E2E_PAIR_URL='hive://pair?url=…&token=…' \
///     TEST_RUNNER_HIVE_E2E_AGENT=coder \
///     TEST_RUNNER_HIVE_E2E_MOVE_TO='MacBook' \
///     TEST_RUNNER_HIVE_E2E_SHOTS=/tmp/hive-ios-shots \
///     xcodebuild -project Hive.xcodeproj -scheme HiveE2E \
///       -destination 'platform=iOS Simulator,name=iPhone 17 Pro' test
///
/// The serve URL must be reachable from the simulator (loopback works: the
/// simulator shares the Mac's network). Screenshots and per-step timings land in
/// `HIVE_E2E_SHOTS` as `e2e-*.png` and `e2e-timings.json`.
@MainActor
final class LiveE2ETests: XCTestCase {
    private var app: XCUIApplication!
    private var env: [String: String] { ProcessInfo.processInfo.environment }
    private var shotsDir: URL?
    private var timings: [(String, TimeInterval)] = []
    private var stepStart = Date()

    override func setUp() async throws {
        continueAfterFailure = false
        guard env["HIVE_E2E_PAIR_URL"]?.isEmpty == false else {
            throw XCTSkip("set TEST_RUNNER_HIVE_E2E_PAIR_URL to run the live end-to-end test")
        }
        if let dir = env["HIVE_E2E_SHOTS"], !dir.isEmpty {
            shotsDir = URL(fileURLWithPath: dir, isDirectory: true)
            try? FileManager.default.createDirectory(at: shotsDir!, withIntermediateDirectories: true)
        }
        app = XCUIApplication()
    }

    func testLiveFlow() throws {
        let pairURL = try XCTUnwrap(URL(string: env["HIVE_E2E_PAIR_URL"] ?? ""))
        let agent = env["HIVE_E2E_AGENT"] ?? "coder"
        let moveTo = env["HIVE_E2E_MOVE_TO"]
        let prompt = env["HIVE_E2E_PROMPT"] ?? "@\(agent) fix the failing test"
        let turnTimeout = TimeInterval(env["HIVE_E2E_TURN_TIMEOUT"] ?? "") ?? 600

        // 1. Pair through the hive://pair deep link.
        app.launch()
        step("launch")
        // Open the link the way the camera / another app would. (`app.open(_:)`
        // relaunches through the automation session and can stall ~15 min.)
        XCUIDevice.shared.system.open(pairURL)
        let springboard = XCUIApplication(bundleIdentifier: "com.apple.springboard")
        let allow = springboard.buttons["Open"]
        if allow.waitForExistence(timeout: 3) { allow.tap() }
        let pair = app.buttons["pair.confirm"]
        XCTAssert(pair.waitForExistence(timeout: 15), "pair confirmation sheet")
        shot("01-pair-confirm")
        pair.tap()
        let chatsTab = app.tabBars.buttons["Chats"]
        XCTAssert(chatsTab.waitForExistence(timeout: 20), "main tabs after pairing")
        XCTAssert(waitGone(app.buttons["pair.confirm"], timeout: 20), "pair sheet dismissed")
        XCTAssert(app.descendants(matching: .any)["Server stream Live"].waitForExistence(timeout: 20),
                  "live event stream")
        shot("02-paired-chats")
        step("pair")

        // 2. New chat.
        app.buttons["New chat"].tap()
        let alert = app.alerts["New chat"]
        XCTAssert(alert.waitForExistence(timeout: 5))
        let title = "E2E \(Int(Date().timeIntervalSince1970) % 100_000)"
        alert.textFields.firstMatch.typeText(title)
        alert.buttons["Create"].tap()
        let composer = composerField()
        XCTAssert(composer.waitForExistence(timeout: 10), "chat composer")
        step("new chat")

        // 3. Mention the agent.
        composer.tap()
        composer.typeText(prompt)
        shot("03-compose")
        app.buttons["Send"].tap()
        XCTAssert(app.staticTexts.matching(NSPredicate(format: "label CONTAINS %@", "fix the failing test"))
                    .firstMatch.waitForExistence(timeout: 10), "sent message shown")
        step("send")

        // 4. Turn status → streaming → done.
        let working = app.staticTexts["\(agent) is working…"]
        let queued = app.staticTexts["\(agent) is queued…"]
        XCTAssert(working.waitForExistence(timeout: 60) || queued.exists, "turn status strip")
        shot("04-working")
        step("turn started")
        // A mid-stream frame, if the reply streams visible text before it ends.
        let deadline = Date().addingTimeInterval(turnTimeout)
        var midShot = false
        while Date() < deadline {
            if !working.exists && !queued.exists { break }
            if !midShot, app.activityIndicators.count > 0, Date().timeIntervalSince(stepStart) > 3 {
                shot("05-streaming")
                midShot = true
            }
            Thread.sleep(forTimeInterval: 2)
        }
        let failed = app.staticTexts.matching(NSPredicate(format: "label BEGINSWITH %@", "\(agent) failed")).firstMatch
        XCTAssertFalse(failed.exists, "agent turn failed: \(failed.exists ? failed.label : "")")
        XCTAssertFalse(working.exists, "turn did not finish in \(Int(turnTimeout))s")
        shot("06-reply")
        step("turn done")

        // A reply watched live is read: no unread badge once back on the list,
        // even after the proposal's chat.updated arrives.
        app.navigationBars.buttons.element(boundBy: 0).tap()
        XCTAssert(chatsTab.waitForExistence(timeout: 5))
        Thread.sleep(forTimeInterval: 4)
        let unread = app.descendants(matching: .any).matching(NSPredicate(format: "label CONTAINS %@ AND label ENDSWITH %@", title, " unread")).firstMatch
        XCTAssertFalse(unread.exists, "chat still shows \(unread.exists ? unread.label : "")")
        shot("06b-chats-read")

        // 5. Review → approve.
        app.tabBars.buttons["Review"].tap()
        // Newest first, so the first row is this turn's proposal.
        let rows = app.buttons.matching(NSPredicate(format: "label CONTAINS %@", "proposed changes"))
        let row = rows.firstMatch
        XCTAssert(row.waitForExistence(timeout: 30), "proposal in Review")
        let openBefore = rows.count
        shot("07-review-list")
        row.tap()
        let approve = app.buttons["Approve"]
        XCTAssert(approve.waitForExistence(timeout: 10))
        shot("08-proposal-diff")
        approve.tap()
        let leftInbox = expectation(for: NSPredicate { _, _ in rows.count == openBefore - 1 && !approve.exists },
                                    evaluatedWith: nil)
        XCTAssertEqual(XCTWaiter().wait(for: [leftInbox], timeout: 15), .completed, "proposal left the inbox")
        shot("09-approved")
        step("approve")

        // 6. Lab status renders (degrades when Prometheus is unreachable).
        app.tabBars.buttons["Lab"].tap()
        XCTAssert(app.staticTexts["Workers"].waitForExistence(timeout: 20), "lab status")
        shot("10-lab")
        step("lab")

        // 7. Send the agent to another host.
        app.tabBars.buttons["Agents"].tap()
        let hostMenu = app.buttons.matching(NSPredicate(format: "label BEGINSWITH %@", "Host for \(agent)")).firstMatch
        XCTAssert(hostMenu.waitForExistence(timeout: 15), "agent row")
        shot("11-agents")
        if let moveTo {
            hostMenu.tap()
            // An offline host reads "<label> (offline)" in the menu.
            let target = app.buttons.matching(NSPredicate(format: "label BEGINSWITH %@", moveTo)).firstMatch
            XCTAssert(target.waitForExistence(timeout: 5), "host \(moveTo) in menu")
            shot("12-host-menu")
            target.tap()
            let moved = app.buttons.matching(NSPredicate(format: "label BEGINSWITH %@", "Host for \(agent): \(moveTo)")).firstMatch
            XCTAssert(moved.waitForExistence(timeout: 15), "agent now on \(moveTo)")
            shot("13-agent-moved")
            step("send to host")
        }
        writeTimings()
    }

    // MARK: Helpers

    private func composerField() -> XCUIElement {
        let field = app.textFields["Message"]
        return field.exists ? field : (app.textViews["Message"].exists ? app.textViews["Message"] : field)
    }

    private func waitGone(_ el: XCUIElement, timeout: TimeInterval) -> Bool {
        let p = NSPredicate(format: "exists == false")
        return XCTWaiter().wait(for: [expectation(for: p, evaluatedWith: el)], timeout: timeout) == .completed
    }

    private func step(_ name: String) {
        let now = Date()
        timings.append((name, now.timeIntervalSince(stepStart)))
        stepStart = now
    }

    private func shot(_ name: String) {
        let png = XCUIScreen.main.screenshot().pngRepresentation
        let att = XCTAttachment(data: png, uniformTypeIdentifier: "public.png")
        att.name = "e2e-\(name)"
        att.lifetime = .keepAlways
        add(att)
        if let dir = shotsDir {
            try? png.write(to: dir.appendingPathComponent("e2e-\(name).png"))
        }
    }

    private func writeTimings() {
        guard let dir = shotsDir else { return }
        let rows = timings.map { ["step": $0.0, "seconds": ($0.1 * 10).rounded() / 10] as [String: Any] }
        if let data = try? JSONSerialization.data(withJSONObject: rows, options: [.prettyPrinted]) {
            try? data.write(to: dir.appendingPathComponent("e2e-timings.json"))
        }
    }
}
