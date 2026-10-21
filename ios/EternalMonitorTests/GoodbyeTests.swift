import Darwin
import XCTest
@testable import EternalMonitor

/// Leaving a session must hand every BYE copy to the network before the link
/// is torn down, and that intentional teardown must not raise a problem.
@MainActor
final class GoodbyeTests: XCTestCase {
    private func connectedManager(_ host: FakeUDPHost) async throws -> ConnectionManager {
        let manager = ConnectionManager()
        manager.connect(host: "127.0.0.1", port: host.port)
        let accepted = await Task.detached { host.acceptSession() }.value
        XCTAssertTrue(accepted, "the iPad never sent HELLO2")
        for _ in 0..<200 where manager.hostInfo == nil {
            try await Task.sleep(nanoseconds: 10_000_000)
        }
        XCTAssertEqual(manager.hostInfo?.sessionId, 41)
        return manager
    }

    func testBackgroundingSendsEveryGoodbyeBeforeTheLinkCloses() async throws {
        let host = FakeUDPHost()
        let manager = try await connectedManager(host)
        defer { manager.cancel() }
        manager.handleAppBackgrounded()
        XCTAssertEqual(manager.state, .disconnected)
        let reasons = await Task.detached { host.goodbyes() }.value
        XCTAssertEqual(reasons.filter { $0 == .appBackground }.count, ControlChannel.byeCopies,
            "every BYE copy must leave before teardown, not on a timer the teardown cancels")
        // The intentional teardown raises no problem banner.
        XCTAssertNil(manager.problem)
        XCTAssertNil(manager.connectionError)
    }

    /// The CI lifecycle scenario's three Home/foreground cycles, without the UI:
    /// every cycle says goodbye in full and resumes.
    func testEveryBackgroundCycleSaysGoodbyeAndResumes() async throws {
        let previous = UserDefaults.standard.object(forKey: "autoResumeOnForeground")
        UserDefaults.standard.set(true, forKey: "autoResumeOnForeground")
        defer { UserDefaults.standard.set(previous, forKey: "autoResumeOnForeground") }
        let host = FakeUDPHost()
        let manager = try await connectedManager(host)
        defer { manager.cancel() }
        for cycle in 1...3 {
            manager.handleAppBackgrounded()
            let reasons = await Task.detached { host.goodbyes() }.value
            XCTAssertEqual(reasons.filter { $0 == .appBackground }.count, ControlChannel.byeCopies, "cycle \(cycle)")
            XCTAssertNil(manager.problem, "cycle \(cycle)")
            manager.handleAppForegrounded()
            XCTAssertEqual(manager.state, .connecting, "cycle \(cycle) must resume")
            let accepted = await Task.detached { host.acceptSession() }.value
            XCTAssertTrue(accepted, "cycle \(cycle) never sent HELLO2")
            for _ in 0..<200 where manager.hostInfo == nil {
                try await Task.sleep(nanoseconds: 10_000_000)
            }
            XCTAssertEqual(manager.hostInfo?.sessionId, 41, "cycle \(cycle)")
        }
    }

    func testDisconnectSendsEveryGoodbyeWithoutAProblem() async throws {
        let host = FakeUDPHost()
        let manager = try await connectedManager(host)
        manager.cancel()
        let reasons = await Task.detached { host.goodbyes() }.value
        XCTAssertEqual(reasons.filter { $0 == .userDisconnect }.count, ControlChannel.byeCopies)
        XCTAssertNil(manager.problem)
        XCTAssertNil(manager.connectionError)
    }
}
