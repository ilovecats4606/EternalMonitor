import Foundation

/// The iPad half of https://eternalmonitor.dev/latest.json. The website bumps it
/// only after Apple approves the TestFlight build, so it never names a build
/// testers cannot install yet. No Apple credentials are involved.
struct UpdateManifest: Decodable, Equatable {
    struct IPad: Decodable, Equatable {
        let version: String
        let build: Int
        let testflight: String?
    }

    let schema: Int
    let ipad: IPad?

    /// The iPad entry, or nil for anything malformed or from a newer schema.
    static func parse(_ data: Data) -> IPad? {
        guard let manifest = try? JSONDecoder().decode(UpdateManifest.self, from: data),
              manifest.schema == 1,
              let ipad = manifest.ipad,
              AppVersion(ipad.version) != nil,
              ipad.build > 0
        else { return nil }
        return ipad
    }
}

/// A plain `major.minor.patch` release version. Previews and other forms are rejected.
struct AppVersion: Comparable {
    let parts: [Int]

    init?(_ string: String) {
        let pieces = string.split(separator: ".", omittingEmptySubsequences: false)
        guard pieces.count == 3 else { return nil }
        var parts: [Int] = []
        for piece in pieces {
            guard !piece.isEmpty, piece.allSatisfy(\.isASCII), piece.allSatisfy(\.isNumber),
                  let value = Int(piece)
            else { return nil }
            parts.append(value)
        }
        self.parts = parts
    }

    static func < (lhs: AppVersion, rhs: AppVersion) -> Bool {
        lhs.parts.lexicographicallyPrecedes(rhs.parts)
    }
}

struct InstalledApp: Equatable {
    let version: String
    let build: Int

    static var current: InstalledApp {
        let info = Bundle.main.infoDictionary
        return InstalledApp(
            version: info?["CFBundleShortVersionString"] as? String ?? "0.0.0",
            build: Int(info?["CFBundleVersion"] as? String ?? "") ?? 0
        )
    }

    /// True when TestFlight has a newer build than this one.
    func isOlder(than available: UpdateManifest.IPad) -> Bool {
        guard let newest = AppVersion(available.version), let mine = AppVersion(version) else { return false }
        if newest != mine { return mine < newest }
        return build < available.build
    }
}

/// What the Connect screen banner shows. Dismissing hides it for one build
/// for a day; Settings keeps showing the update.
struct UpdateBannerModel: Equatable {
    static let dismissInterval: TimeInterval = 24 * 60 * 60

    let version: String
    let build: Int

    var title: String { "A new version is ready in TestFlight" }
    var detail: String {
        "Version \(version), build \(build). In TestFlight, turn on Automatic Updates to get new versions without asking."
    }

    static func make(
        available: UpdateManifest.IPad?,
        installed: InstalledApp,
        automatic: Bool,
        dismissed: (build: Int, at: Date)?,
        now: Date
    ) -> UpdateBannerModel? {
        guard automatic, let available, installed.isOlder(than: available) else { return nil }
        if let dismissed, dismissed.build == available.build,
           now.timeIntervalSince(dismissed.at) < dismissInterval, dismissed.at <= now {
            return nil
        }
        return UpdateBannerModel(version: available.version, build: available.build)
    }
}

/// Checks the manifest on launch and when the app returns to the foreground,
/// at most every few hours, and stays quiet offline. Debug builds only check
/// a manifest injected with `EM_UPDATE_MANIFEST`, so tests never see a banner
/// they did not ask for.
@MainActor
final class UpdateChecker: ObservableObject {
    static let shared = UpdateChecker()
    nonisolated static let manifestURL = URL(string: "https://eternalmonitor.dev/latest.json")!
    nonisolated static let joinURL = URL(string: "https://testflight.apple.com/join/ppc9QfXc")!
    static let checkInterval: TimeInterval = 4 * 60 * 60

    private enum Key {
        static let automatic = "checkForUpdates"
        static let checkedAt = "updateCheckedAt"
        static let version = "updateAvailableVersion"
        static let build = "updateAvailableBuild"
        static let testflight = "updateAvailableTestFlight"
        static let dismissedBuild = "updateDismissedBuild"
        static let dismissedAt = "updateDismissedAt"
    }

    enum Source {
        case network(URL)
        case injected(Data)
        case disabled
    }

    @Published private(set) var available: UpdateManifest.IPad?
    @Published private(set) var dismissed: (build: Int, at: Date)?
    @Published var automatic: Bool {
        didSet {
            defaults.set(automatic, forKey: Key.automatic)
            if automatic { checkIfDue() }
        }
    }

    let installed: InstalledApp
    private let defaults: UserDefaults
    private let source: Source
    private var checking = false

    init(defaults: UserDefaults = .standard, source: Source = UpdateChecker.defaultSource, installed: InstalledApp = .current) {
        self.defaults = defaults
        self.source = source
        self.installed = installed
        automatic = defaults.object(forKey: Key.automatic) as? Bool ?? true
        if let version = defaults.string(forKey: Key.version) {
            let found = UpdateManifest.IPad(
                version: version,
                build: defaults.integer(forKey: Key.build),
                testflight: defaults.string(forKey: Key.testflight)
            )
            available = installed.isOlder(than: found) ? found : nil
        }
        let dismissedBuild = defaults.integer(forKey: Key.dismissedBuild)
        if dismissedBuild > 0, let at = defaults.object(forKey: Key.dismissedAt) as? Date {
            dismissed = (dismissedBuild, at)
        }
    }

    nonisolated static var defaultSource: Source {
        #if DEBUG
        if let text = ProcessInfo.processInfo.environment["EM_UPDATE_MANIFEST"] {
            return .injected(Data(text.utf8))
        }
        return .disabled
        #else
        return .network(manifestURL)
        #endif
    }

    var banner: UpdateBannerModel? {
        UpdateBannerModel.make(available: available, installed: installed, automatic: automatic, dismissed: dismissed, now: Date())
    }

    /// The TestFlight invite named by the manifest, or the public link.
    var joinURL: URL {
        if let link = available?.testflight, let url = URL(string: link),
           url.scheme == "https", url.host == "testflight.apple.com" {
            return url
        }
        return Self.joinURL
    }

    func dismissBanner(now: Date = Date()) {
        guard let available else { return }
        dismissed = (available.build, now)
        defaults.set(available.build, forKey: Key.dismissedBuild)
        defaults.set(now, forKey: Key.dismissedAt)
    }

    func checkIfDue(now: Date = Date()) {
        guard automatic, !checking else { return }
        switch source {
        case .disabled:
            return
        case .injected(let data):
            apply(UpdateManifest.parse(data), now: now)
        case .network(let url):
            if let last = defaults.object(forKey: Key.checkedAt) as? Date,
               now.timeIntervalSince(last) < Self.checkInterval, last <= now {
                return
            }
            checking = true
            Task {
                let found = await Self.fetch(url)
                checking = false
                // Offline: keep what we knew and try again next time.
                guard let found else { return }
                apply(found, now: now)
            }
        }
    }

    private func apply(_ found: UpdateManifest.IPad?, now: Date) {
        defaults.set(now, forKey: Key.checkedAt)
        guard let found, installed.isOlder(than: found) else {
            available = nil
            for key in [Key.version, Key.build, Key.testflight] { defaults.removeObject(forKey: key) }
            return
        }
        available = found
        defaults.set(found.version, forKey: Key.version)
        defaults.set(found.build, forKey: Key.build)
        defaults.set(found.testflight, forKey: Key.testflight)
    }

    /// A plain GET: no cookies, no cache, no identifiers.
    private nonisolated static func fetch(_ url: URL) async -> UpdateManifest.IPad?? {
        let configuration = URLSessionConfiguration.ephemeral
        configuration.timeoutIntervalForRequest = 10
        configuration.requestCachePolicy = .reloadIgnoringLocalCacheData
        configuration.httpAdditionalHeaders = ["User-Agent": "EternalMonitor"]
        let session = URLSession(configuration: configuration)
        defer { session.finishTasksAndInvalidate() }
        guard let (data, response) = try? await session.data(from: url),
              (response as? HTTPURLResponse)?.statusCode == 200,
              data.count <= 256 * 1024
        else { return nil }
        return .some(UpdateManifest.parse(data))
    }
}
