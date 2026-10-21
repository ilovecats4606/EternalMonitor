import Darwin
import Foundation
@testable import EternalMonitor

/// A fake PC on a loopback UDP socket: accepts the HELLO2, then collects
/// whatever the iPad sends until it goes quiet.
final class FakeUDPHost: @unchecked Sendable {
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
    /// HELLO2 attempts (distinct client nonces) received within `seconds`.
    func helloAttempts(within seconds: Double) -> Int {
        var nonces = Set<UInt32>()
        let deadline = Date().addingTimeInterval(seconds)
        while Date() < deadline {
            guard let (data, _, _) = receive() else { continue }
            if case .hello2(let hello) = Wire.parseControl(data)?.message { nonces.insert(hello.clientNonce) }
        }
        return nonces.count
    }

    func goodbyes() -> [ByeReason] {
        var reasons: [ByeReason] = []
        while let (data, _, _) = receive() {
            if case .bye(let reason) = Wire.parseControl(data)?.message { reasons.append(reason) }
        }
        return reasons
    }
}

