import Darwin
import XCTest
@testable import EternalMonitor

/// Leaving a session must hand every BYE copy to the network before the link
/// is torn down, and that intentional teardown must not raise a problem.
@MainActor
final class GoodbyeTests: XCTestCase {
    /// A fake PC on a loopback UDP socket: accepts the HELLO2, then collects
    /// whatever the iPad sends until it goes quiet.
    private final class FakeHost: @unchecked Sendable {
        let fd = socket(AF_INET, SOCK_DGRAM, IPPROTO_UDP)
        private(set) var port: UInt16 = 0

        init() {
            var address = sockaddr_in()
            address.sin_len = UInt8(MemoryLayout<sockaddr_in>.size)
            address.sin_family = sa_family_t(AF_INET)
            address.sin_addr.s_addr = inet_addr("127.0.0.1")
            var size = socklen_t(MemoryLayout<sockaddr_in>.size)
            let bindSize = size
            port = withUnsafeMutablePointer(to: &address) { pointer in
                pointer.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                    _ = Darwin.bind(fd, $0, bindSize)
                    _ = getsockname(fd, $0, &size)
                }
                return UInt16(bigEndian: pointer.pointee.sin_port)
            }
            var timeout = timeval(tv_sec: 0, tv_usec: 300_000)
            setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &timeout, socklen_t(MemoryLayout<timeval>.size))
        }

        deinit { Darwin.close(fd) }

        private func receive() -> (Data, sockaddr_storage, socklen_t)? {
            var buffer = [UInt8](repeating: 0, count: 2048)
            var from = sockaddr_storage()
            var length = socklen_t(MemoryLayout<sockaddr_storage>.size)
            let count = withUnsafeMutablePointer(to: &from) { pointer in
                pointer.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                    recvfrom(fd, &buffer, buffer.count, 0, $0, &length)
                }
            }
            return count > 0 ? (Data(buffer.prefix(count)), from, length) : nil
        }

        /// Answers the first HELLO2 with session 41. False after ten quiet seconds.
        func acceptSession() -> Bool {
            for _ in 0..<33 {
                guard let (data, from, length) = receive() else { continue }
                guard case .hello2(let hello) = Wire.parseControl(data)?.message else { continue }
                let ack = Wire.encodeControl(sessionId: 0, msgSeq: 1, message: .helloAck(HelloAck(
                    status: .ok, acceptedVersion: 2, clientNonce: hello.clientNonce, sessionId: 41,
                    heartbeatIntervalMs: 1000, reportIntervalMs: 500, livenessTimeoutMs: 3000,
                    streamConfig: StreamConfig(), hostName: "Test PC", authToken: Data(17...32), hostCaps: 0)))
                var to = from
                _ = ack.withUnsafeBytes { bytes in
                    withUnsafePointer(to: &to) { pointer in
                        pointer.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                            sendto(fd, bytes.baseAddress, bytes.count, 0, $0, length)
                        }
                    }
                }
                return true
            }
            return false
        }

        /// The BYE reasons received until the iPad has been quiet for 300 ms.
        func goodbyes() -> [ByeReason] {
            var reasons: [ByeReason] = []
            while let (data, _, _) = receive() {
                if case .bye(let reason) = Wire.parseControl(data)?.message { reasons.append(reason) }
            }
            return reasons
        }
    }

    private func connectedManager(_ host: FakeHost) async throws -> ConnectionManager {
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
        let host = FakeHost()
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
        let host = FakeHost()
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
        let host = FakeHost()
        let manager = try await connectedManager(host)
        manager.cancel()
        let reasons = await Task.detached { host.goodbyes() }.value
        XCTAssertEqual(reasons.filter { $0 == .userDisconnect }.count, ControlChannel.byeCopies)
        XCTAssertNil(manager.problem)
        XCTAssertNil(manager.connectionError)
    }
}
