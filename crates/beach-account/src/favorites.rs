//! Keeping favorites in step through the coordination server: every entry
//! carries the time of its last change, the newer side wins, and deletions
//! stay as tombstones. The iOS app's `FavoriteSync.swift` follows the same
//! rules.

use crate::api::Favorite;

/// What to do after comparing the local favorites with the server's.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Merged {
    /// Newer on the server (or unknown here): take these over locally.
    pub apply: Vec<Favorite>,
    /// Newer here (or unknown there): send these to the server.
    pub push: Vec<Favorite>,
}

/// Compares `local` (including tombstones) with `remote`. A local entry
/// that was never synced (`updated == 0`) and has the same address as a
/// server entry is taken to be that entry, so the first sync of two viewers
/// with the same computers doesn't double them; [`Merged::apply`] then
/// carries the server's entry, which the caller matches by address. Other
/// never-synced entries are sent with `now` (Unix milliseconds) and come
/// back in [`Merged::apply`] too, so the caller keeps that time.
pub fn merge(local: &[Favorite], remote: &[Favorite], now: i64) -> Merged {
    let mut merged = Merged::default();
    for r in remote {
        match local.iter().find(|l| l.id == r.id) {
            Some(l) if l.updated > r.updated => merged.push.push(l.clone()),
            Some(l) if l == r => {}
            Some(_) => merged.apply.push(r.clone()),
            None if r.deleted => {}
            None => merged.apply.push(r.clone()),
        }
    }
    for l in local {
        if remote.iter().any(|r| r.id == l.id) {
            continue;
        }
        if l.updated == 0 && remote.iter().any(|r| same_host(r, l)) {
            continue;
        }
        let mut l = l.clone();
        if l.updated == 0 {
            l.updated = now;
            merged.apply.push(l.clone());
        }
        merged.push.push(l);
    }
    merged
}

/// A never-synced local entry the server's `remote` stands for.
pub fn same_host(remote: &Favorite, local: &Favorite) -> bool {
    !remote.deleted && !local.deleted && local.updated == 0 && remote.host.eq_ignore_ascii_case(local.host.trim())
}

/// Checks a favorite from a client before the server stores it.
pub fn validate(f: &Favorite) -> Result<(), &'static str> {
    let short = |s: &str| s.len() <= 256 && !s.chars().any(char::is_control);
    if f.id.is_empty() || f.id.len() > 64 || !f.id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return Err("invalid favorite id");
    }
    if !short(&f.name) || !short(&f.host) || f.alternatives.len() > 16 || !f.alternatives.iter().all(|a| short(a)) {
        return Err("favorite too long");
    }
    if f.updated <= 0 {
        return Err("favorite without time");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fav(id: &str, host: &str, updated: i64) -> Favorite {
        Favorite {
            id: id.into(),
            name: host.into(),
            host: host.into(),
            alternatives: Vec::new(),
            view_only: false,
            updated,
            deleted: false,
        }
    }

    #[test]
    fn newer_side_wins() {
        let local = [fav("a", "mac", 20), fav("b", "pc", 10), fav("c", "nas", 5)];
        let mut b = fav("b", "pc.local", 30);
        b.name = "PC".into();
        let remote = [fav("a", "mac", 10), b.clone(), fav("d", "ipad", 7)];
        let m = merge(&local, &remote, 99);
        assert_eq!(m.apply, vec![b, fav("d", "ipad", 7)]);
        assert_eq!(m.push, vec![fav("a", "mac", 20), fav("c", "nas", 5)]);
    }

    #[test]
    fn deletions_travel_both_ways() {
        let mut gone = fav("a", "mac", 30);
        gone.deleted = true;
        let m = merge(&[fav("a", "mac", 20)], std::slice::from_ref(&gone), 99);
        assert_eq!(m.apply, vec![gone.clone()]);
        let m = merge(std::slice::from_ref(&gone), &[fav("a", "mac", 20)], 99);
        assert_eq!(m.push, vec![gone.clone()]);
        // A tombstone for something never seen here changes nothing.
        assert_eq!(merge(&[], std::slice::from_ref(&gone), 99), Merged::default());
    }

    #[test]
    fn first_sync_matches_by_address() {
        let local = [fav("local-id", "Sebastian-PC", 0), fav("new", "nas", 0)];
        let remote = [fav("remote-id", "sebastian-pc", 50)];
        let m = merge(&local, &remote, 99);
        // The unmatched one goes out with the current time, kept here too.
        assert_eq!(m.push, vec![fav("new", "nas", 99)]);
        assert_eq!(m.apply, vec![remote[0].clone(), fav("new", "nas", 99)]);
        assert!(same_host(&remote[0], &local[0]));
        assert!(!same_host(&remote[0], &fav("x", "sebastian-pc", 3)));
    }

    #[test]
    fn validation() {
        assert!(validate(&fav("0f6c-11", "mac", 1)).is_ok());
        assert!(validate(&fav("", "mac", 1)).is_err());
        assert!(validate(&fav("a/b", "mac", 1)).is_err());
        assert!(validate(&fav("a", "mac", 0)).is_err());
        assert!(validate(&fav("a", &"x".repeat(300), 1)).is_err());
    }
}
