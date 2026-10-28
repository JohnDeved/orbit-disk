import Foundation

public enum TrashPolicy {
    public static func rejectionReason(
        for url: URL,
        scanRoot: URL,
        home: URL = FileManager.default.homeDirectoryForCurrentUser,
        excluding roots: [URL]? = nil,
        cloudValidator: CloudStoragePolicy.Validator? = nil
    ) -> String? {
        guard url.isFileURL, scanRoot.isFileURL else { return "Only local files can be removed." }
        do {
            return try CloudStoragePolicy.withoutMaterialization {
                localRejectionReason(for: url, scanRoot: scanRoot, home: home,
                                     excluding: roots, cloudValidator: cloudValidator)
            }
        } catch {
            return "This item cannot be checked without downloading cloud content."
        }
    }

    private static func localRejectionReason(for url: URL, scanRoot: URL, home: URL,
                                             excluding roots: [URL]?, cloudValidator: CloudStoragePolicy.Validator?) -> String? {
        let cloudReason: String?
        if let cloudValidator {
            cloudReason = cloudValidator.exclusionReason(for: url)
        } else {
            cloudReason = CloudStoragePolicy.exclusionReason(for: url, home: home, excluding: roots,
                                                            protectContainingRoots: true)
        }
        if let reason = cloudReason {
            return reason
        }
        let manager = FileManager.default
        let original = url.standardizedFileURL
        let root = scanRoot.standardizedFileURL.resolvingSymlinksInPath()
        let target = original.resolvingSymlinksInPath()
        let home = home.standardizedFileURL.resolvingSymlinksInPath()

        guard target.path != root.path else { return "The mapped folder cannot be removed." }
        guard isWithin(target, root) else { return "This item is outside the scanned folder." }
        guard target.path != "/", !isWithin(home, target) else { return "This location is protected." }

        let protected = ["/System", "/Library", "/Applications", "/usr", "/bin", "/sbin", "/etc"]
            .map { URL(fileURLWithPath: $0).resolvingSymlinksInPath() }
        if protected.contains(where: { isWithin(target, $0) }) || isWithin(target, home.appendingPathComponent("Library")) {
            return "System and Library folders are protected."
        }

        // macOS places ordinary temporary files beneath /private; permit only its temporary locations.
        let privateRoot = URL(fileURLWithPath: "/private")
        let temporaryRoots = [manager.temporaryDirectory.resolvingSymlinksInPath(), URL(fileURLWithPath: "/private/tmp")]
        if isWithin(target, privateRoot), !temporaryRoots.contains(where: { isWithin(target, $0) && target.path != $0.path }) {
            return "This system location is protected."
        }

        if target.pathComponents.contains(where: { $0 == ".Trash" || $0 == ".Trashes" }) {
            return "Items already in Trash are protected."
        }
        let volumes = manager.mountedVolumeURLs(includingResourceValuesForKeys: nil, options: []) ?? []
        if volumes.contains(where: { isWithin($0.resolvingSymlinksInPath(), target) }) || target.path == "/Volumes" || target.deletingLastPathComponent().path == "/Volumes" {
            return "A disk or volume cannot be removed."
        }

        do {
            let attributes = try manager.attributesOfItem(atPath: original.path)
            if attributes[.type] as? FileAttributeType == .typeSymbolicLink {
                return "Symbolic links cannot be removed here."
            }
            var ancestor = target
            while ancestor.path != "/" {
                if ancestor.lastPathComponent.hasPrefix(".") {
                    let values = try ancestor.resourceValues(forKeys: [.isDirectoryKey])
                    if values.isDirectory == true { return "Hidden configuration folders are protected." }
                }
                ancestor.deleteLastPathComponent()
            }
        } catch {
            return "This item is no longer accessible: \(error.localizedDescription)"
        }
        return nil
    }

    private static func isWithin(_ url: URL, _ root: URL) -> Bool {
        url.pathComponents.starts(with: root.pathComponents)
    }
}
