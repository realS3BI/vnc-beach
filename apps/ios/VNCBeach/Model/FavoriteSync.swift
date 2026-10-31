import Foundation

/// A favourite as the coordination server keeps it (`beach_account::api::Favorite`).
/// Passwords stay on each device.
struct Favorite: Codable, Equatable {
    var id: String
    var name = ""
    var host = ""
    var alternatives: [String] = []
    var viewOnly = false
    /// Unix milliseconds of the last change, on whichever device.
    var updated: Int64
    /// Deleted somewhere; kept so the other viewers delete it too.
    var deleted = false

    enum CodingKeys: String, CodingKey {
        case id, name, host, alternatives, updated, deleted
        case viewOnly = "view_only"
    }

    init(id: String, name: String = "", host: String = "", alternatives: [String] = [], viewOnly: Bool = false,
         updated: Int64, deleted: Bool = false) {
        (self.id, self.name, self.host, self.alternatives) = (id, name, host, alternatives)
        (self.viewOnly, self.updated, self.deleted) = (viewOnly, updated, deleted)
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id)
        name = try c.decodeIfPresent(String.self, forKey: .name) ?? ""
        host = try c.decodeIfPresent(String.self, forKey: .host) ?? ""
        alternatives = try c.decodeIfPresent([String].self, forKey: .alternatives) ?? []
        viewOnly = try c.decodeIfPresent(Bool.self, forKey: .viewOnly) ?? false
        updated = try c.decode(Int64.self, forKey: .updated)
        deleted = try c.decodeIfPresent(Bool.self, forKey: .deleted) ?? false
    }
}

/// The sync rules of `beach_account::favorites`: the newer side wins,
/// deletions stay as tombstones, and a never-synced local favourite with the
/// same address as one on the server is taken to be that one.
enum FavoriteSync {
    struct Merged: Equatable {
        /// Newer on the server: take these over locally.
        var apply: [Favorite] = []
        /// Newer here: send these to the server.
        var push: [Favorite] = []
    }

    /// Never-synced local favourites without a match go out with `now` and
    /// come back in `apply`, so the store keeps that time.
    static func merge(local: [Favorite], remote: [Favorite], now: Int64 = FavoriteSync.now()) -> Merged {
        var merged = Merged()
        for r in remote {
            if let l = local.first(where: { $0.id == r.id }) {
                if l.updated > r.updated {
                    merged.push.append(l)
                } else if l != r {
                    merged.apply.append(r)
                }
            } else if !r.deleted {
                merged.apply.append(r)
            }
        }
        for l in local where !remote.contains(where: { $0.id == l.id }) {
            if l.updated == 0 && remote.contains(where: { sameHost($0, l) }) { continue }
            var l = l
            if l.updated == 0 {
                l.updated = now
                merged.apply.append(l)
            }
            merged.push.append(l)
        }
        return merged
    }

    static func sameHost(_ remote: Favorite, _ local: Favorite) -> Bool {
        !remote.deleted && !local.deleted && local.updated == 0
            && remote.host.caseInsensitiveCompare(local.host.trimmingCharacters(in: .whitespaces)) == .orderedSame
    }

    static func now() -> Int64 { Int64(Date().timeIntervalSince1970 * 1000) }
}
