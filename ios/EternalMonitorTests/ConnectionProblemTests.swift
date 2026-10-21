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

    func testReceiverErrorsBecomePlainProblems() {
        XCTAssertEqual(ConnectionProblem.receiverError("Cannot resolve pc.local: nodename nor servname provided", host: "pc.local"), .badAddress("pc.local"))
        XCTAssertEqual(ConnectionProblem.receiverError("UDP path not ready: The operation couldn’t be completed. (Network.NWError error 50 - Network is down)", host: "x"), .noNetwork)
        XCTAssertEqual(ConnectionProblem.receiverError("UDP connection failed: Local network prohibited", host: "x"), .localNetworkBlocked)
        XCTAssertEqual(ConnectionProblem.receiverError("UDP socket failed: 48", host: "x"), .couldNotConnect("UDP socket failed: 48"))
    }

    func testEveryProblemSaysWhatToDoWithoutTransportJargon() {
        let all: [ConnectionProblem] = [
            .noReply(usb: false), .noReply(usb: true), .noPicture, .receiverNotReady, .badAddress("pc"),
            .noNetwork, .localNetworkBlocked, .lost(.pcStoppedResponding), .lost(.cableUnplugged),
            .lost(.usbTurnedOff), .reconnecting(attempt: 2, of: 6), .reconnectFailed, .pcEndedSession,
        ]
        for problem in all {
            XCTAssertFalse(problem.title.isEmpty)
            XCTAssertFalse(problem.detail.isEmpty)
            for jargon in ["HELLO", "datagram", "heartbeat", "host", "UDP", "VideoToolbox", "FramePacket"] {
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
