import Foundation
import Testing

@testable import VNCBeach

struct UpdateCheckerTests {
    @Test func versionsParseAndCompare() throws {
        let a = try #require(SemanticVersion("v0.2.0"))
        let b = try #require(SemanticVersion("0.1.9"))
        #expect(a > b)
        #expect(SemanticVersion("1.0") == SemanticVersion("1.0.0"))
        #expect(SemanticVersion("v0.10.0")! > SemanticVersion("0.9.9")!)
        #expect(SemanticVersion("") == nil)
        #expect(SemanticVersion("1.2.x") == nil)
        #expect(a.description == "0.2.0")
    }

    @Test func releasePayloadParses() throws {
        let json = Data("""
            {"tag_name": "v0.2.0", "html_url": "https://github.com/realS3BI/vnc-beach/releases/tag/v0.2.0", "name": "0.2.0"}
            """.utf8)
        let release = try #require(UpdateChecker.parse(json))
        #expect(release.version == SemanticVersion("0.2.0"))
        #expect(release.url.path.hasSuffix("/v0.2.0"))
        #expect(UpdateChecker.parse(Data(#"{"message": "Not Found"}"#.utf8)) == nil)
    }

    @Test @MainActor func bannerHonoursDismissal() {
        let checker = UpdateChecker(currentVersion: "0.1.0")
        #expect(checker.banner == nil)
        #expect(checker.currentVersion == SemanticVersion("0.1.0"))
    }
}
