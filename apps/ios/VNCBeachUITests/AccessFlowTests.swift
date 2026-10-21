import XCTest

nonisolated final class AccessFlowTests: XCTestCase {
    @MainActor func testCapabilityEditorAndFailureActions() throws {
        XCUIDevice.shared.orientation = .portrait
        let app = XCUIApplication()
        app.launch()
        app.buttons["Computer hinzufügen"].firstMatch.tap()
        let name = "Erreichbarkeit UI-Test \(UUID().uuidString.prefix(6))"
        app.textFields["Name"].tap()
        app.textFields["Name"].typeText(name)
        app.textFields["Adresse"].tap()
        app.textFields["Adresse"].typeText("127.0.0.1::1")
        app.buttons["Erreichbarkeit & Start"].tap()
        XCTAssertTrue(app.navigationBars["Erreichbarkeit & Start"].waitForExistence(timeout: 5))
        app.switches["SSH-Anmeldung anbieten"].switches.firstMatch.tap()
        app.textFields["SSH-Adresse"].tap()
        app.textFields["SSH-Adresse"].typeText("127.0.0.1:22")
        app.collectionViews.element(boundBy: app.collectionViews.count - 1).swipeDown()
        app.textFields["SSH-Benutzername"].tap()
        app.textFields["SSH-Benutzername"].typeText("fixture")
        app.swipeUp()
        app.switches["Aufwecken anbieten"].switches.firstMatch.tap()
        app.textFields["MAC-Adresse"].tap()
        app.textFields["MAC-Adresse"].typeText("00:11:22:33:44:55")
        app.collectionViews.element(boundBy: app.collectionViews.count - 1).swipeUp()
        app.buttons["wake-relays"].tap()
        app.buttons["Zugangsgerät hinzufügen"].tap()
        app.buttons["Neues Zugangsgerät"].tap()
        app.textFields["SSH-Adresse"].tap()
        app.textFields["SSH-Adresse"].typeText("pi.local")
        app.collectionViews.element(boundBy: app.collectionViews.count - 1).swipeDown()
        app.textFields["SSH-Benutzername"].tap()
        app.textFields["SSH-Benutzername"].typeText("remote")
        app.navigationBars["Zugangsgerät"].buttons.firstMatch.tap()
        app.navigationBars["Zugangsgeräte"].buttons.firstMatch.tap()
        let screenshot = XCTAttachment(screenshot: app.screenshot())
        screenshot.name = "Erreichbarkeit und Start auf dem iPad"
        screenshot.lifetime = .keepAlways
        add(screenshot)
        app.navigationBars["Erreichbarkeit & Start"].buttons.firstMatch.tap()
        app.buttons["Sichern"].tap()
        XCTAssertTrue(app.buttons.matching(NSPredicate(format: "label BEGINSWITH %@", name)).firstMatch.waitForExistence(timeout: 5))
        app.buttons.matching(NSPredicate(format: "label BEGINSWITH %@", name)).firstMatch.tap()
        XCTAssertTrue(app.buttons["Erneut verbinden"].waitForExistence(timeout: 10))
        XCTAssertTrue(app.buttons["Per SSH anmelden"].exists)
        XCTAssertTrue(app.buttons["Aufwecken & verbinden"].exists)
        let failure = XCTAttachment(screenshot: app.screenshot())
        failure.name = "Verbindungsaktionen nach fehlgeschlagener VNC-Verbindung"
        failure.lifetime = .keepAlways
        add(failure)
        app.buttons["Zurück"].tap()
        app.buttons.matching(NSPredicate(format: "label BEGINSWITH %@", name)).firstMatch.swipeLeft()
        app.buttons["Löschen"].tap()
    }
}
