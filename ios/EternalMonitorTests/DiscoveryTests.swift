import Darwin
import XCTest
@testable import EternalMonitor

/// A PC is only offered once it has an address the host's IPv4 listener
/// answers on, and saved PCs stay visible next to what discovery finds.
@MainActor
final class DiscoveryTests: XCTestCase {
    private func ipv4(_ text: String) -> Data {
        var addr = sockaddr_in()
        addr.sin_len = UInt8(MemoryLayout<sockaddr_in>.size)
        addr.sin_family = sa_family_t(AF_INET)
        inet_pton(AF_INET, text, &addr.sin_addr)
        return withUnsafeBytes(of: &addr) { Data($0) }
    }

    private func ipv6(_ text: String) -> Data {
        var addr = sockaddr_in6()
        addr.sin6_len = UInt8(MemoryLayout<sockaddr_in6>.size)
        addr.sin6_family = sa_family_t(AF_INET6)
        inet_pton(AF_INET6, text, &addr.sin6_addr)
        return withUnsafeBytes(of: &addr) { Data($0) }
    }

    func testLinkLocalIPv6AloneIsNotOfferedAsAnAddress() {
        // The case seen on the reference PC: Bonjour answered with only an
        // unscoped fe80:: address first.
        let host = DiscoveredHost.resolved(name: "EternalMonitor (STUDIO)", port: 9876,
            addresses: [ipv6("fe80::b38d:6347:a022:8ec5")])
        XCTAssertNil(host.address)
        XCTAssertTrue(host.finding)
        XCTAssertEqual(ConnectView.discoveredSubtitle(host), "Finding…")
        XCTAssertFalse(ConnectView.discoveredSubtitle(host).contains("fe80"))

        let global = DiscoveredHost.resolved(name: "PC", port: 9876, addresses: [ipv6("2001:db8::20")])
        XCTAssertNil(global.address, "the host listens on IPv4 only")
    }

    func testIPv4IsPreferredWhateverOrderBonjourAnswers() {
        let host = DiscoveredHost.resolved(name: "PC", port: 9876,
            addresses: [ipv6("fe80::1"), ipv4("192.168.1.20"), ipv6("2001:db8::20")])
        XCTAssertEqual(host.address, "192.168.1.20")
        XCTAssertFalse(host.finding)
        XCTAssertEqual(ConnectView.discoveredSubtitle(host), "Tap to connect · 192.168.1.20")
        XCTAssertEqual(DiscoveredHost.resolved(name: "PC", port: 0, addresses: [ipv4("10.0.0.5")]).port, 9876)
    }

    func testAPCStillBeingFoundNeverReplacesOneWithAnAddress() {
        let scanner = NetworkScanner()
        scanner.merge(DiscoveredHost.resolved(name: "PC", port: 9876, addresses: [ipv6("fe80::1")]))
        XCTAssertEqual(scanner.hosts.map(\.address), [nil])
        scanner.merge(DiscoveredHost(name: "PC", address: "192.168.1.20", port: 9876))
        XCTAssertEqual(scanner.hosts.map(\.address), ["192.168.1.20"])
        // A later IPv6-only answer for the same PC keeps the IPv4 entry.
        scanner.merge(DiscoveredHost.resolved(name: "PC", port: 9876, addresses: [ipv6("fe80::1")]))
        XCTAssertEqual(scanner.hosts.map(\.address), ["192.168.1.20"])
        let gaveUp = DiscoveredHost(name: "Other", address: nil, port: 9876, finding: false)
        XCTAssertTrue(ConnectView.discoveredSubtitle(gaveUp).contains("Type the Address"))
    }

    /// A slow IPv4 lookup can't bring back a PC that left the network, or
    /// replace the address a newer scan found.
    func testALateIPv4LookupCannotBringBackOrOverwriteAPC() async throws {
        let lookup = HeldLookup()
        let scanner = NetworkScanner()
        scanner.resolveIPv4 = { lookup.resolve($0) }
        // A lookup still wanted gives its PC the address.
        let other = DiscoveredHost.resolved(name: "Other", port: 9876, addresses: [ipv6("fe80::2")])
        scanner.merge(other)
        scanner.lookUpIPv4(hostName: "other.local.", service: other)
        lookup.answer("192.168.1.30")
        try await Task.sleep(for: .milliseconds(300))
        XCTAssertEqual(scanner.hosts.map(\.address), ["192.168.1.30"])

        // The PC leaves the network while its lookup runs.
        let found = DiscoveredHost.resolved(name: "PC", port: 9876, addresses: [ipv6("fe80::1")])
        scanner.merge(found)
        scanner.lookUpIPv4(hostName: "studio.local.", service: found)
        scanner.netServiceBrowser(NetServiceBrowser(),
            didRemove: NetService(domain: "local.", type: "_eternaldisplay._udp.", name: "PC"), moreComing: false)
        try await Task.sleep(for: .milliseconds(100))
        XCTAssertEqual(scanner.hosts.map(\.name), ["Other"])
        lookup.answer("192.168.1.20")
        try await Task.sleep(for: .milliseconds(300))
        XCTAssertEqual(scanner.hosts.map(\.name), ["Other"], "a PC that left must not come back")

        // A new scan finds the PC at a newer address before an older lookup answers.
        scanner.merge(found)
        scanner.lookUpIPv4(hostName: "studio.local.", service: found)
        scanner.startScan()
        defer { scanner.stopScan() }
        scanner.merge(DiscoveredHost(name: "PC", address: "192.168.1.21", port: 9876))
        lookup.answer("192.168.1.20")
        try await Task.sleep(for: .milliseconds(300))
        XCTAssertEqual(scanner.hosts.map(\.address), ["192.168.1.21"])
    }

    func testSavedPCsStayVisibleUnlessDiscoveryListsTheSameOne() {
        let saved = [
            RecentConnection(host: "192.168.1.20", port: 9876, lastUsed: .now, isUSB: false),
            RecentConnection(host: "100.80.149.7", port: 9876, lastUsed: .now, isUSB: false),
            RecentConnection(host: "Studio PC", port: 9877, lastUsed: .now, isUSB: true),
        ]
        // Someone else's PC on a shared network hides nothing.
        let neighbour = [DiscoveredHost(name: "EternalMonitor (NEIGHBOUR)", address: "192.168.1.44", port: 9876)]
        XCTAssertEqual(RecentConnection.unlisted(saved, discovered: neighbour).map(\.host),
            ["192.168.1.20", "100.80.149.7", "Studio PC"])
        // The user's own PC found again is listed once, under Found.
        let mine = neighbour + [DiscoveredHost(name: "EternalMonitor (STUDIO)", address: "192.168.1.20", port: 9876)]
        XCTAssertEqual(RecentConnection.unlisted(saved, discovered: mine).map(\.host), ["100.80.149.7", "Studio PC"])
        // Same address on another port is a different entry.
        let otherPort = [DiscoveredHost(name: "PC", address: "192.168.1.20", port: 9900)]
        XCTAssertEqual(RecentConnection.unlisted(saved, discovered: otherPort).count, 3)
        // A PC still being found matches nothing.
        let finding = [DiscoveredHost(name: "PC", address: nil, port: 9876, finding: true)]
        XCTAssertEqual(RecentConnection.unlisted(saved, discovered: finding).count, 3)
    }
}

/// A host-name lookup that answers when the test says so, as a slow
/// multicast DNS reply would. One lookup runs at a time.
private final class HeldLookup: @unchecked Sendable {
    private let answered = DispatchSemaphore(value: 0)
    private var address: String?

    func resolve(_ hostName: String) -> String? {
        answered.wait()
        return address
    }

    func answer(_ address: String) {
        self.address = address
        answered.signal()
    }
}
