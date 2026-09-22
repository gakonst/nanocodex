#if os(iOS)
import Foundation
import InboxCore
import Contacts
import Photos

enum IOSPersonalTools {
    static func call(name: String, fields: [String: JSON]) async throws -> JSON {
        switch name {
        case "search_contacts": return try contacts(fields)
        case "search_photos", "photo_details", "list_photo_albums": return try photos(name, fields)
        case "current_location":
            let request = try PersonalToolRequest(fields, allowed: ["timeoutSeconds"])
            let timeout = try request.integer("timeoutSeconds", default: 8, range: 1...10)
            let fix = try await HandLocationProvider.shared.currentSnapshot(timeoutSeconds: Double(timeout))
            return fix.json
        default: throw HandFailure.invalidInput
        }
    }
    private static func contacts(_ fields: [String: JSON]) throws -> JSON {
        let request = try PersonalToolRequest(fields, allowed: ["query", "limit", "cursor"])
        let query = try request.text("query") ?? ""
        let offset = try request.offset(), limit = try request.integer("limit", default: 20, range: 1...50)
        let status = CNContactStore.authorizationStatus(for: .contacts)
        var allowed = status == .authorized
        if #available(iOS 18.0, *) { allowed = allowed || status == .limited }
        guard allowed else { throw HandFailure.contextAccess("Contacts permission is not granted. Enable it in Settings before searching contacts.") }
        let fetch = CNContactFetchRequest(keysToFetch: [CNContactIdentifierKey, CNContactGivenNameKey, CNContactFamilyNameKey, CNContactMiddleNameKey, CNContactOrganizationNameKey, CNContactEmailAddressesKey, CNContactPhoneNumbersKey].map { $0 as CNKeyDescriptor })
        fetch.sortOrder = .userDefault
        var results: [JSON] = [], index = 0, scanned = 0, more = false
        var cancelled = false
        let digits = query.filter(\.isNumber)
        try CNContactStore().enumerateContacts(with: fetch) { contact, stop in
            if Task.isCancelled { cancelled = true; stop.pointee = true; return }
            if index < offset { index += 1; return }
            if scanned >= 500 || results.count >= limit { more = true; stop.pointee = true; return }
            index += 1; scanned += 1
            let name = [contact.givenName, contact.middleName, contact.familyName].filter { !$0.isEmpty }.joined(separator: " ")
            let emails = contact.emailAddresses.map { String($0.value) }, phones = contact.phoneNumbers.map { $0.value.stringValue }
            let matches = query.isEmpty || ([name, contact.organizationName] + emails + phones).contains { $0.localizedCaseInsensitiveContains(query) }
                || (!digits.isEmpty && phones.contains { $0.filter(\.isNumber).contains(digits) })
            if matches {
                results.append(.object(["id": .string(contact.identifier), "name": .string(String(name.prefix(1024))), "organization": .string(String(contact.organizationName.prefix(1024))),
                    "emails": .array(emails.prefix(20).map { .string(String($0.prefix(1024))) }), "phones": .array(phones.prefix(20).map { .string(String($0.prefix(256))) })]))
            }
        }
        if cancelled { throw CancellationError() }
        var result: [String: JSON] = ["contacts": .array(results), "has_more": .bool(more), "scanned": .number(Double(scanned)), "scope": .string("currently accessible contacts")]
        if more { result["nextCursor"] = try request.cursor(index) }
        return .object(result)
    }
    private static func photos(_ name: String, _ fields: [String: JSON]) throws -> JSON {
        let request = try PersonalToolRequest(fields, allowed: name == "photo_details" ? ["id"] : name == "list_photo_albums" ? ["limit", "cursor"] : ["after", "before", "mediaType", "favorite", "albumId", "limit", "cursor"])
        let status = PHPhotoLibrary.authorizationStatus(for: .readWrite)
        guard status == .authorized || status == .limited else { throw HandFailure.contextAccess("Photos read permission is not granted. Enable it in Settings before searching photos.") }
        if name == "list_photo_albums" {
            let limit = try request.integer("limit", default: 20, range: 1...50), offset = try request.offset()
            let user = PHAssetCollection.fetchAssetCollections(with: .album, subtype: .any, options: nil)
            let smart = PHAssetCollection.fetchAssetCollections(with: .smartAlbum, subtype: .any, options: nil)
            let total = user.count + smart.count
            guard offset <= total else { throw HandFailure.contextAccess("Photo albums changed; restart the search.") }
            let end = min(total, offset + limit)
            let albums: [JSON] = (offset..<end).map { index in
                let album = index < user.count ? user.object(at: index) : smart.object(at: index - user.count)
                return .object(["id": .string(album.localIdentifier), "title": .string(String((album.localizedTitle ?? "").prefix(1024))),
                                "kind": .string(index < user.count ? "album" : "smartAlbum")])
            }
            var result: [String: JSON] = ["albums": .array(albums), "has_more": .bool(end < total), "limited_access": .bool(status == .limited)]
            if end < total { result["nextCursor"] = try request.cursor(end) }
            return .object(result)
        }
        if name == "photo_details" {
            let id = try request.text("id", required: true)!
            guard let asset = PHAsset.fetchAssets(withLocalIdentifiers: [id], options: nil).firstObject else { throw HandFailure.contextAccess("Photo is unavailable or outside the currently accessible library.") }
            return metadata(asset)
        }
        let after = try request.date("after"), before = try request.date("before")
        if let after, let before, after > before { throw HandFailure.invalidInput }
        let media = try request.text("mediaType"), favorite = try request.boolean("favorite"), album = try request.text("albumId")
        guard media == nil || media == "image" || media == "video" else { throw HandFailure.invalidInput }
        let limit = try request.integer("limit", default: 20, range: 1...50), offset = try request.offset()
        let options = PHFetchOptions()
        var predicates: [NSPredicate] = []
        if let after { predicates.append(NSPredicate(format: "creationDate >= %@", after as NSDate)) }
        if let before { predicates.append(NSPredicate(format: "creationDate <= %@", before as NSDate)) }
        if let media { predicates.append(NSPredicate(format: "mediaType == %d", media == "image" ? PHAssetMediaType.image.rawValue : PHAssetMediaType.video.rawValue)) }
        if let favorite { predicates.append(NSPredicate(format: "favorite == %@", NSNumber(value: favorite))) }
        options.predicate = NSCompoundPredicate(andPredicateWithSubpredicates: predicates)
        options.sortDescriptors = [NSSortDescriptor(key: "creationDate", ascending: false)]
        let assets: PHFetchResult<PHAsset>
        if let album {
            guard let collection = PHAssetCollection.fetchAssetCollections(withLocalIdentifiers: [album], options: nil).firstObject else { throw HandFailure.contextAccess("Album is unavailable or inaccessible.") }
            assets = PHAsset.fetchAssets(in: collection, options: options)
        } else { assets = PHAsset.fetchAssets(with: options) }
        guard offset <= assets.count else { throw HandFailure.contextAccess("Photo library changed; restart the search.") }
        let end = min(offset + limit, assets.count)
        let results = (offset..<end).map { metadata(assets.object(at: $0)) }
        var result: [String: JSON] = ["photos": .array(results), "has_more": .bool(end < assets.count), "limited_access": .bool(status == .limited)]
        if end < assets.count { result["nextCursor"] = try request.cursor(end) }
        return .object(result)
    }
    private static func metadata(_ asset: PHAsset) -> JSON {
        var result: [String: JSON] = ["id": .string(asset.localIdentifier), "mediaType": .string(asset.mediaType == .image ? "image" : asset.mediaType == .video ? "video" : "other"), "width": .number(Double(asset.pixelWidth)), "height": .number(Double(asset.pixelHeight)), "durationSeconds": .number(asset.duration), "favorite": .bool(asset.isFavorite)]
        if let date = asset.creationDate { result["createdAt"] = .string(ISO8601DateFormatter().string(from: date)) }
        if let date = asset.modificationDate { result["modifiedAt"] = .string(ISO8601DateFormatter().string(from: date)) }
        return .object(result)
    }
}
#endif
