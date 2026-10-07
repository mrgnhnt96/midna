// "Open in Midna" on Finder's right-click menu, with the app's icon. A Finder Sync extension
// (Contents/PlugIns/MidnaFinderSync.appex), built by packaging/build-app.sh with swiftc; the
// setting finder.quick_action turns it on and off (midna-app finder.rs, via pluginkit).
// Services and Quick Action workflows can't show an icon there; see docs/DECISIONS.md.
import Cocoa
import FinderSync

@objc(FinderSync)
class FinderSync: FIFinderSync {
    /// The Midna.app this extension ships in (…/Midna.app/Contents/PlugIns/X.appex).
    let app = Bundle.main.bundleURL.deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()

    override init() {
        super.init()
        // The menu is offered everywhere; a Finder Sync extension only gets it under these folders.
        FIFinderSyncController.default().directoryURLs = [URL(fileURLWithPath: "/")]
    }

    override func menu(for menuKind: FIMenuKind) -> NSMenu {
        let menu = NSMenu(title: "")
        guard menuKind == .contextualMenuForItems || menuKind == .contextualMenuForContainer else { return menu }
        // The app's name (Midna, Midna Dev): build-app.sh copies it here; the sandbox can't read the app's.
        let name = Bundle.main.object(forInfoDictionaryKey: "CFBundleDisplayName") as? String ?? "Midna"
        let item = NSMenuItem(title: "Open in \(name)", action: #selector(openInMidna(_:)), keyEquivalent: "")
        let icon = NSWorkspace.shared.icon(forFile: app.path)
        icon.size = NSSize(width: 16, height: 16)
        item.image = icon
        menu.addItem(item)
        return menu
    }

    /// The selected items, else the folder that was right-clicked. The sandbox won't hand file
    /// URLs to another app, so each goes as `<app bundle id>://open?path=<path>` (the app
    /// registers its bundle id as a URL scheme; midna-app finder::folder_of). Files open their
    /// folder.
    @objc func openInMidna(_ sender: AnyObject?) {
        let c = FIFinderSyncController.default()
        let items = c.selectedItemURLs().flatMap { $0.isEmpty ? nil : $0 } ?? c.targetedURL().map { [$0] } ?? []
        let scheme = (Bundle.main.bundleIdentifier ?? "com.mrgnhnt.midna.finder-sync").replacingOccurrences(of: ".finder-sync", with: "")
        let urls = items.compactMap { item -> URL? in
            var u = URLComponents()
            u.scheme = scheme
            u.host = "open"
            u.queryItems = [URLQueryItem(name: "path", value: item.path)]
            return u.url
        }
        guard !urls.isEmpty else { return }
        let config = NSWorkspace.OpenConfiguration()
        config.activates = true
        NSWorkspace.shared.open(urls, withApplicationAt: app, configuration: config) { _, error in
            if let error { NSLog("Open in Midna: \(error)") }
        }
    }
}
