import Foundation
import Network

/// A connection problem told in plain words: what happened, what to do, and
/// the button that does it. The technical detail stays in Connection details
/// and the support log.
enum ConnectionProblem: Equatable {
    /// Nothing came back from the address (PC closed, asleep, other network,
    /// blocked by a firewall or the router).
    case noReply(usb: Bool)
    /// The PC answered, but no picture could be shown.
    case noPicture
    /// This iPad could not open its side of the connection.
    case receiverNotReady
    case badAddress(String)
    case noNetwork
    case localNetworkBlocked
    case couldNotConnect
    /// The cable connection failed after it had opened.
    case usbFailed
    /// A live session stopped: heartbeats ran out or the cable came out.
    case lost(LostReason)
    case reconnecting(attempt: Int, of: Int)
    case reconnectFailed
    case pcEndedSession
    /// The PC is streaming to another device, or still to this iPad's cable.
    case pcBusy

    enum LostReason: Equatable {
        case pcStoppedResponding
        case cableUnplugged
        case usbTurnedOff
    }

    enum Action: Equatable {
        case retry
        case openSettings
    }

    var title: String {
        switch self {
        case .noReply(usb: false): return "Your PC didn't answer"
        case .noReply(usb: true): return "Your PC didn't answer over the cable"
        case .noPicture: return "Your PC answered, but no picture arrived"
        case .receiverNotReady: return "This iPad couldn't start the connection"
        case .badAddress: return "That address doesn't look right"
        case .noNetwork: return "This iPad isn't connected to a network"
        case .localNetworkBlocked: return "EternalMonitor isn't allowed on your local network"
        case .couldNotConnect: return "Couldn't connect to your PC"
        case .usbFailed: return "The connection over the cable stopped"
        case .lost(.pcStoppedResponding): return "Lost the connection to your PC"
        case .lost(.cableUnplugged): return "The USB cable was disconnected"
        case .lost(.usbTurnedOff): return "USB connections were turned off"
        case .reconnecting: return "Reconnecting to your PC…"
        case .reconnectFailed: return "Couldn't reconnect to your PC"
        case .pcEndedSession: return "Your PC ended the session"
        case .pcBusy: return "Your PC is already streaming to another device"
        }
    }

    var detail: String {
        switch self {
        case .noReply(usb: false):
            return "Check that EternalMonitor is open on the PC and says Ready for your iPad, that the PC is awake, "
                + "and that both are on the same Wi-Fi, not a guest network. If all of that is right, Windows Firewall "
                + "or security software on the PC may be blocking EternalMonitor: run the PC installer again, or connect with a USB cable."
        case .noReply(usb: true):
            return "Check that EternalMonitor and Apple Devices are both open on the PC, then unplug the cable and plug it in again."
        case .noPicture:
            return "EternalMonitor on the PC couldn't send its screen. Look there for a message and follow it, click Restart, then try again. "
                + "If it keeps happening, send us Settings, Diagnostics from this iPad and Performance, Copy logs from the PC."
        case .receiverNotReady:
            return "Close EternalMonitor on this iPad and open it again."
        case .badAddress(let host):
            return "This iPad couldn't find \"\(host)\". Type the Address exactly as the PC window shows it, for example 192.168.1.20."
        case .noNetwork:
            return "Turn on Wi-Fi and join the same network as your PC, or connect with a USB cable."
        case .localNetworkBlocked:
            return "Open Settings and turn on Local Network for EternalMonitor, then try again."
        case .couldNotConnect:
            return "Check that this iPad and your PC are on the same Wi-Fi and that EternalMonitor is open on the PC, then try again."
        case .usbFailed:
            return "Keep EternalMonitor and Apple Devices open on the PC, unlock this iPad, then unplug the cable and plug it in again."
        case .lost(.pcStoppedResponding):
            return "The PC may have gone to sleep, closed EternalMonitor, or lost its network. Wake the PC, check EternalMonitor is open, then try again."
        case .lost(.cableUnplugged):
            return "Plug the cable back in. This iPad reconnects by itself."
        case .lost(.usbTurnedOff):
            return "Turn on Allow USB connections in Settings to use the cable again."
        case .reconnecting(let attempt, let total):
            return "Attempt \(attempt) of \(total). Keep EternalMonitor open on the PC."
        case .reconnectFailed:
            return "Check that the PC is awake and EternalMonitor is open on it, then try again."
        case .pcEndedSession:
            return "Someone clicked Stop on the PC, changed a setting that restarts the stream, or closed EternalMonitor. Try again when the PC says Ready for your iPad."
        case .pcBusy:
            return "Disconnect the other iPad first, then try again."
        }
    }

    var actions: [Action] {
        switch self {
        case .localNetworkBlocked: return [.openSettings]
        case .reconnecting, .lost(.cableUnplugged), .lost(.usbTurnedOff), .receiverNotReady: return []
        default: return [.retry]
        }
    }

    /// Why a connect attempt timed out, from how far it got.
    /// A PC that accepted the handshake did answer: no picture after that is
    /// a problem on the PC (screen capture or video encoding), not the network.
    static func timeout(listenerReady: Bool, helloAttempts: Int, handshakeDone: Bool,
                        datagramsReceived: Int, isUSB: Bool) -> ConnectionProblem {
        if handshakeDone { return .noPicture }
        if !listenerReady || helloAttempts == 0 { return .receiverNotReady }
        return datagramsReceived == 0 ? .noReply(usb: isUSB) : .noPicture
    }

    /// The banner once automatic reconnecting gives up. A last attempt that
    /// failed for a specific reason keeps that advice; otherwise the PC just
    /// never answered.
    static func afterReconnecting(lastAttempt: ConnectionProblem?) -> ConnectionProblem {
        switch lastAttempt {
        case .localNetworkBlocked, .noNetwork, .badAddress, .usbFailed, .noPicture, .pcBusy, .pcEndedSession:
            return lastAttempt ?? .reconnectFailed
        default:
            return .reconnectFailed
        }
    }

    /// What a link failure means for the user. The raw error stays in the
    /// support log; nothing here shows it.
    static func linkFailure(_ error: LinkError, isUSB: Bool, host: String) -> ConnectionProblem {
        // The cable is a local tunnel: Wi-Fi, DNS and Local Network don't apply.
        if isUSB { return .usbFailed }
        if error.kind == .localNetworkDenied { return .localNetworkBlocked }
        if let nw = error.underlying as? NWError {
            switch nw {
            case .dns(let code):
                // kDNSServiceErr_PolicyDenied: Local Network access is off.
                return code == -65570 ? .localNetworkBlocked : .badAddress(host)
            case .posix(let code) where offline.contains(code):
                return .noNetwork
            default:
                break
            }
        }
        if let posix = error.underlying as? POSIXError, offline.contains(posix.code) { return .noNetwork }
        switch error.kind {
        case .resolve: return .badAddress(host)
        case .pathUnavailable: return .noNetwork
        default: return .couldNotConnect
        }
    }

    private static let offline: Set<POSIXErrorCode> = [.ENETDOWN, .ENETUNREACH, .EHOSTUNREACH]
}

/// What the USB cable card says for each listener status.
enum USBStatusText {
    static func describe(_ status: String) -> String {
        switch status {
        case "USB: off": return "Off. Turn on to connect with a cable."
        case "USB: waiting for a cable": return "Plug this iPad into your PC. Keep Apple Devices open on the PC."
        case "USB: disconnected": return "Paused. Tap Connect USB to use the cable again."
        case "USB: connecting": return "Connecting over the cable…"
        case "USB: unavailable": return "The cable can't be used right now. Wi-Fi still works."
        default:
            if status.hasPrefix("USB: connected to ") {
                return "Connected to \(status.dropFirst("USB: connected to ".count)) over the cable."
            }
            return status
        }
    }
}
