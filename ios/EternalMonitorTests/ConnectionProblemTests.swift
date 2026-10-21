import Network
import XCTest
@testable import EternalMonitor

final class ConnectionProblemTests: XCTestCase {
    func testTimeoutsMapToHowFarTheAttemptGot() {
        XCTAssertEqual(ConnectionProblem.timeout(listenerReady: false, helloAttempts: 0, handshakeDone: false, datagramsReceived: 0, isUSB: false), .receiverNotReady)
        XCTAssertEqual(ConnectionProblem.timeout(listenerReady: true, helloAttempts: 0, handshakeDone: false, datagramsReceived: 0, isUSB: false), .receiverNotReady)
        XCTAssertEqual(ConnectionProblem.timeout(listenerReady: true, helloAttempts: 6, handshakeDone: false, datagramsReceived: 0, isUSB: false), .noReply(usb: false))
        XCTAssertEqual(ConnectionProblem.timeout(listenerReady: true, helloAttempts: 6, handshakeDone: false, datagramsReceived: 0, isUSB: true), .noReply(usb: true))
        XCTAssertEqual(ConnectionProblem.timeout(listenerReady: true, helloAttempts: 2, handshakeDone: false, datagramsReceived: 40, isUSB: false), .noPicture)
        // The PC accepted the session but sent no video: its capture or encoder
        // failed. That is not a network problem, whatever arrived.
        XCTAssertEqual(ConnectionProblem.timeout(listenerReady: true, helloAttempts: 1, handshakeDone: true, datagramsReceived: 0, isUSB: true), .noPicture)
    }

    func testLinkFailuresBecomePlainProblemsForTheirTransport() {
        let wifi = { (error: LinkError) in ConnectionProblem.linkFailure(error, isUSB: false, host: "studio-pc") }
        // DNS: a name that doesn't resolve, through either socket backend.
        let noSuchRecord = LinkError(.pathUnavailable,
            "UDP path not ready: The operation couldn’t be completed. (Network.NWError error -65554 - NoSuchRecord)",
            underlying: NWError.dns(-65554))
        XCTAssertEqual(wifi(noSuchRecord), .badAddress("studio-pc"))
        XCTAssertEqual(wifi(LinkError(.resolve, "Cannot resolve studio-pc: nodename nor servname provided")), .badAddress("studio-pc"))
        // Permission: Local Network access off, reported either way.
        XCTAssertEqual(wifi(LinkError(.localNetworkDenied, "UDP path not ready: denied", underlying: NWError.posix(.EPERM))), .localNetworkBlocked)
        XCTAssertEqual(wifi(LinkError(.connectionFailed, "UDP connection failed: PolicyDenied", underlying: NWError.dns(-65570))), .localNetworkBlocked)
        // Offline.
        XCTAssertEqual(wifi(LinkError(.pathUnavailable, "UDP path not ready: Network is down", underlying: NWError.posix(.ENETDOWN))), .noNetwork)
        XCTAssertEqual(wifi(LinkError(.receive, "UDP receive error: No route to host", underlying: POSIXError(.EHOSTUNREACH))), .noNetwork)
        // Unknown failures get general advice, never the raw error.
        let unknown = LinkError(.connectionFailed, "UDP socket failed: Address already in use", underlying: POSIXError(.EADDRINUSE))
        XCTAssertEqual(wifi(unknown), .couldNotConnect)
        XCTAssertFalse(ConnectionProblem.couldNotConnect.detail.contains("Address already in use"))
        // USB: a cable failure is never a Wi-Fi or DNS problem.
        let usbReset = LinkError(.receive, "USB receive failed: The operation couldn’t be completed. (Network.NWError error 54 - Connection reset by peer)",
            underlying: NWError.posix(.ECONNRESET))
        for error in [usbReset, noSuchRecord, LinkError(.timeout, "USB handshake timed out.")] {
            let problem = ConnectionProblem.linkFailure(error, isUSB: true, host: "USB")
            XCTAssertEqual(problem, .usbFailed, error.message)
        }
        let usb = ConnectionProblem.usbFailed
        XCTAssertTrue(usb.detail.contains("cable") && usb.detail.contains("Apple Devices"), usb.detail)
        XCTAssertFalse(usb.detail.contains("Wi-Fi"), usb.detail)
        XCTAssertEqual(usb.actions, [.retry])
    }

    func testGivingUpOnReconnectingKeepsASpecificLastFailure() {
        // A classified failure in the last attempt is more useful than
        // "Couldn't reconnect"; a PC that just never answered is not.
        for specific: ConnectionProblem in [.localNetworkBlocked, .noNetwork, .badAddress("pc"), .usbFailed, .noPicture,
                                             .pcBusy, .pcEndedSession] {
            XCTAssertEqual(ConnectionProblem.afterReconnecting(lastAttempt: specific), specific)
        }
        for generic: ConnectionProblem? in [nil, .noReply(usb: false), .reconnecting(attempt: 6, of: 6), .couldNotConnect] {
            XCTAssertEqual(ConnectionProblem.afterReconnecting(lastAttempt: generic), .reconnectFailed)
        }
    }

    func testEveryProblemSaysWhatToDoWithoutTransportJargon() {
        let all: [ConnectionProblem] = [
            .noReply(usb: false), .noReply(usb: true), .noPicture, .receiverNotReady, .badAddress("pc"),
            .noNetwork, .localNetworkBlocked, .couldNotConnect, .usbFailed, .lost(.pcStoppedResponding), .lost(.cableUnplugged),
            .lost(.usbTurnedOff), .reconnecting(attempt: 2, of: 6), .reconnectFailed, .pcEndedSession,
            .pcBusy,
        ]
        for problem in all {
            XCTAssertFalse(problem.title.isEmpty)
            XCTAssertFalse(problem.detail.isEmpty)
            for jargon in ["HELLO", "datagram", "heartbeat", "host", "UDP", "VideoToolbox", "FramePacket", "NWError", "POSIX", "NoSuchRecord"] {
                XCTAssertFalse(problem.title.contains(jargon) || problem.detail.contains(jargon), "\(problem): \(jargon)")
            }
        }
        XCTAssertEqual(ConnectionProblem.noReply(usb: false).actions, [.retry])
        XCTAssertEqual(ConnectionProblem.localNetworkBlocked.actions, [.openSettings])
        XCTAssertEqual(ConnectionProblem.lost(.cableUnplugged).actions, [])
        XCTAssertTrue(ConnectionProblem.noReply(usb: false).detail.contains("Firewall"))
        XCTAssertTrue(ConnectionProblem.noReply(usb: true).detail.contains("Apple Devices"))
    }

    func testUSBCardTextIsPlain() {
        XCTAssertEqual(USBStatusText.describe("USB: off"), "Off. Turn on to connect with a cable.")
        XCTAssertEqual(USBStatusText.describe("USB: connected to Studio PC"), "Connected to Studio PC over the cable.")
        XCTAssertTrue(USBStatusText.describe("USB: waiting for a cable").contains("Apple Devices"))
        XCTAssertTrue(USBStatusText.describe("USB: disconnected").contains("Connect USB"))
        for status in ["USB: off", "USB: waiting for a cable", "USB: disconnected", "USB: connecting", "USB: unavailable", "USB: connected to PC"] {
            XCTAssertFalse(USBStatusText.describe(status).hasPrefix("USB:"), status)
        }
    }
}
