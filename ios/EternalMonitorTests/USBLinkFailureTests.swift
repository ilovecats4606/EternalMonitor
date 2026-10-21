import Darwin
import Network
import XCTest
@testable import EternalMonitor

/// A real TCP-backed FramedLink, as the USB tunnel delivers it, failing or
/// closing while a connection is being set up. FramedLink reports a failure
/// with onError and then onClosed; only an ordinary close is a removed cable.
@MainActor
final class USBLinkFailureTests: XCTestCase {
    private var previousAuto: Any?
    private var previousHost: Any?
    private var previousPort: Any?
    private var server: Int32 = -1
    private var peer: Int32 = -1

    override func setUp() async throws {
        previousAuto = UserDefaults.standard.object(forKey: "autoReconnect")
        previousHost = UserDefaults.standard.object(forKey: "lastHost")
        previousPort = UserDefaults.standard.object(forKey: "lastPort")
        UserDefaults.standard.set(false, forKey: "autoReconnect")
    }

    override func tearDown() async throws {
        UserDefaults.standard.set(previousAuto, forKey: "autoReconnect")
        UserDefaults.standard.set(previousHost, forKey: "lastHost")
        UserDefaults.standard.set(previousPort, forKey: "lastPort")
        if peer >= 0 { Darwin.close(peer) }
        if server >= 0 { Darwin.close(server) }
    }

    /// A ConnectionManager connecting over a FramedLink whose far end is
    /// `peer`, past the preamble.
    private func connectOverCable(timing: ConnectionManager.Timing = .init()) async throws -> (ConnectionManager, FramedLink) {
        server = socket(AF_INET, SOCK_STREAM, IPPROTO_TCP)
        XCTAssertGreaterThanOrEqual(server, 0)
        var address = sockaddr_in()
        address.sin_family = sa_family_t(AF_INET)
        address.sin_len = UInt8(MemoryLayout<sockaddr_in>.size)
        address.sin_addr.s_addr = inet_addr("127.0.0.1")
        var size = socklen_t(MemoryLayout<sockaddr_in>.size)
        let bindSize = size
        let port = withUnsafeMutablePointer(to: &address) { pointer in
            pointer.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                XCTAssertEqual(Darwin.bind(server, $0, bindSize), 0)
                XCTAssertEqual(getsockname(server, $0, &size), 0)
            }
            return UInt16(bigEndian: pointer.pointee.sin_port)
        }
        XCTAssertEqual(listen(server, 1), 0)
        var timeout = timeval(tv_sec: 3, tv_usec: 0)
        XCTAssertEqual(setsockopt(server, SOL_SOCKET, SO_RCVTIMEO, &timeout,
            socklen_t(MemoryLayout<timeval>.size)), 0)
        let connection = NWConnection(host: "127.0.0.1", port: NWEndpoint.Port(rawValue: port)!, using: .tcp)
        let link = FramedLink(connection: connection)
        let ready = expectation(description: "Valid USB preamble")
        link.onPreambleReady = { ready.fulfill() }
        link.validatePreamble()
        peer = accept(server, nil, nil)
        XCTAssertGreaterThanOrEqual(peer, 0)
        _ = EMLinkFramer.preamble.withUnsafeBytes { Darwin.send(peer, $0.baseAddress, $0.count, 0) }
        await fulfillment(of: [ready], timeout: 3)
        let manager = ConnectionManager(timing: timing)
        manager.connect(target: .usb(link))
        XCTAssertEqual(manager.state, .connecting)
        return (manager, link)
    }

    private func waitUntilDisconnected(_ manager: ConnectionManager) async throws {
        for _ in 0..<200 where manager.state != .disconnected {
            try await Task.sleep(nanoseconds: 10_000_000)
        }
        // Let any callback still queued behind the failure run.
        try await Task.sleep(nanoseconds: 200_000_000)
        XCTAssertEqual(manager.state, .disconnected)
    }

    func testCableFailureDuringSetupKeepsCableAdviceAfterTheLinkCloses() async throws {
        let (manager, link) = try await connectOverCable()
        defer { link.stop(); manager.cancel() }
        // A framing failure: FramedLink.fail sends onError, then onClosed.
        let invalid = Data([0, 0])
        _ = invalid.withUnsafeBytes { Darwin.send(peer, $0.baseAddress, $0.count, 0) }
        try await waitUntilDisconnected(manager)
        XCTAssertEqual(manager.problem, .usbFailed,
            "A failed USB link must keep the cable repair advice and Try again")
        XCTAssertEqual(manager.problem?.actions, [.retry])
        XCTAssertEqual(manager.usbStatus, "USB: waiting for a cable")
    }

    func testOrdinaryCloseDuringSetupSaysTheCableWasDisconnected() async throws {
        let (manager, link) = try await connectOverCable()
        defer { link.stop(); manager.cancel() }
        // The far end closes cleanly: onClosed alone, as when the cable comes out.
        // A FIN, not close(): closing with the iPad's unread HELLO queued
        // would send a reset, which the link rightly reports as a failure.
        XCTAssertEqual(shutdown(peer, SHUT_WR), 0)
        try await waitUntilDisconnected(manager)
        XCTAssertEqual(manager.problem, .lost(.cableUnplugged))
        XCTAssertEqual(manager.usbStatus, "USB: waiting for a cable")
    }

    // MARK: Automatic reconnecting on: a cable that fails or closes during
    // setup falls back to the saved Wi-Fi PC, once.

    /// Saves `host` as the last Wi-Fi PC and turns automatic reconnecting on.
    private func enableWiFiFallback(to host: FakeUDPHost) {
        UserDefaults.standard.set(true, forKey: "autoReconnect")
        UserDefaults.standard.set("127.0.0.1", forKey: "lastHost")
        UserDefaults.standard.set(Int(host.port), forKey: "lastPort")
    }

    private func assertFallsBackOnce(_ manager: ConnectionManager, to wifi: FakeUDPHost) async throws {
        try await waitUntilDisconnected(manager)
        // Falling back, not giving up: the banner says what is happening.
        XCTAssertEqual(manager.problem, .reconnecting(attempt: 1, of: 6))
        XCTAssertEqual(manager.diagnostics.filter { $0.message.hasPrefix("Connection lost") }.count, 1,
            "the reconnect decision must run exactly once")
        // The first retry waits one second; allow four.
        let attempts = await Task.detached { wifi.helloAttempts(within: 4) }.value
        XCTAssertEqual(attempts, 1, "exactly one Wi-Fi fallback attempt")
    }

    func testCableFailureDuringSetupFallsBackToWiFiOnce() async throws {
        let wifi = FakeUDPHost()
        enableWiFiFallback(to: wifi)
        let (manager, link) = try await connectOverCable()
        defer { link.stop(); manager.cancel() }
        let invalid = Data([0, 0])
        _ = invalid.withUnsafeBytes { Darwin.send(peer, $0.baseAddress, $0.count, 0) }
        try await assertFallsBackOnce(manager, to: wifi)
    }

    func testCleanCloseDuringSetupFallsBackToWiFiOnce() async throws {
        let wifi = FakeUDPHost()
        enableWiFiFallback(to: wifi)
        let (manager, link) = try await connectOverCable()
        defer { link.stop(); manager.cancel() }
        XCTAssertEqual(shutdown(peer, SHUT_WR), 0)
        try await assertFallsBackOnce(manager, to: wifi)
    }

    /// Reconnect timing short enough to run a whole cycle in seconds.
    private let fast = ConnectionManager.Timing(connectTimeoutSeconds: 0.4, reconnectGraceSeconds: 0.1,
        reconnectBackoffSeconds: { _ in 0.05 })

    /// Waits for the reconnect cycle to start and then to run out.
    private func waitForFallbackToGiveUp(_ manager: ConnectionManager) async throws {
        var started = false
        for _ in 0..<1000 {
            if manager.reconnectAttempt > 0 { started = true }
            if started && manager.reconnectAttempt == 0 { return }
            try await Task.sleep(nanoseconds: 10_000_000)
        }
        XCTFail("the Wi-Fi fallback never finished")
    }

    /// Every Wi-Fi fallback attempt times out: the user ends on the cable
    /// advice and Try again, not "Couldn't reconnect".
    func testCableFailureAdviceSurvivesAnExhaustedWiFiFallback() async throws {
        let wifi = FakeUDPHost()  // never answers
        enableWiFiFallback(to: wifi)
        let (manager, link) = try await connectOverCable(timing: fast)
        defer { link.stop(); manager.cancel() }
        let attempts = Task.detached { wifi.helloAttempts(within: 6) }
        let invalid = Data([0, 0])
        _ = invalid.withUnsafeBytes { Darwin.send(peer, $0.baseAddress, $0.count, 0) }
        try await waitForFallbackToGiveUp(manager)
        let attempted = await attempts.value
        XCTAssertEqual(attempted, 6, "all six Wi-Fi attempts ran")
        XCTAssertEqual(manager.state, .disconnected)
        XCTAssertEqual(manager.reconnectAttempt, 0)
        XCTAssertEqual(manager.problem, .usbFailed)
        XCTAssertEqual(manager.problem?.actions, [.retry])
    }

    /// A later, more specific failure (the saved address doesn't resolve)
    /// replaces the cable advice when the fallback gives up.
    func testALaterSpecificFailureReplacesTheCableAdvice() async throws {
        UserDefaults.standard.set(true, forKey: "autoReconnect")
        UserDefaults.standard.set("eternalmonitor-test.invalid", forKey: "lastHost")
        UserDefaults.standard.set(9876, forKey: "lastPort")
        let (manager, link) = try await connectOverCable(timing: fast)
        defer { link.stop(); manager.cancel() }
        let invalid = Data([0, 0])
        _ = invalid.withUnsafeBytes { Darwin.send(peer, $0.baseAddress, $0.count, 0) }
        try await waitForFallbackToGiveUp(manager)
        XCTAssertEqual(manager.reconnectAttempt, 0)
        XCTAssertEqual(manager.problem, .badAddress("eternalmonitor-test.invalid"))
    }
}
