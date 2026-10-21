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
    private var server: Int32 = -1
    private var peer: Int32 = -1

    override func setUp() async throws {
        previousAuto = UserDefaults.standard.object(forKey: "autoReconnect")
        UserDefaults.standard.set(false, forKey: "autoReconnect")
    }

    override func tearDown() async throws {
        UserDefaults.standard.set(previousAuto, forKey: "autoReconnect")
        if peer >= 0 { Darwin.close(peer) }
        if server >= 0 { Darwin.close(server) }
    }

    /// A ConnectionManager connecting over a FramedLink whose far end is
    /// `peer`, past the preamble.
    private func connectOverCable() async throws -> (ConnectionManager, FramedLink) {
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
        let manager = ConnectionManager()
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
}
