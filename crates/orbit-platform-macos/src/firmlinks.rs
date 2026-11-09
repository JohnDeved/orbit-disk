//! The boot volume group: the sealed System volume exposes Data-volume folders through firmlinks
//! listed in `/usr/share/firmlinks` (`/Users` is `/System/Volumes/Data/Users`). Only links from
//! that table are ever normalized, and each one is verified against the live filesystem.

use std::collections::HashMap;
use std::sync::OnceLock;

use orbit_platform::{BootLayout, FileIdentity};

use crate::paths;

pub const DATA_VOLUME: &str = "/System/Volumes/Data";
const FIRMLINKS_FILE: &str = "/usr/share/firmlinks";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Link {
    /// Absolute path on the System volume (`/Users`).
    pub logical: Vec<u8>,
    /// The same folder addressed through the Data volume mount.
    pub physical: Vec<u8>,
}

/// Parses the firmlink table. Lines must have exactly two whitespace-separated fields: an absolute
/// logical path and a relative Data-volume path, both made of safe components.
pub fn parse(text: &[u8]) -> Vec<Link> {
    text.split(|&b| b == b'\n' || b == b'\r')
        .filter_map(|line| {
            let mut fields = line.split(|b| b.is_ascii_whitespace()).filter(|f| !f.is_empty());
            let (logical, relative) = (fields.next()?, fields.next()?);
            if fields.next().is_some() || logical.first() != Some(&b'/') || relative.first() == Some(&b'/') {
                return None;
            }
            if !safe_components(&logical[1..]) || !safe_components(relative) {
                return None;
            }
            let mut physical = DATA_VOLUME.as_bytes().to_vec();
            physical.push(b'/');
            physical.extend_from_slice(relative);
            Some(Link { logical: logical.to_vec(), physical })
        })
        .collect()
}

fn safe_components(path: &[u8]) -> bool {
    !path.is_empty()
        && !path.contains(&0)
        && path.split(|&b| b == b'/').all(|c| !c.is_empty() && c != b"." && c != b"..")
}

/// The system firmlink table, read once per process.
pub fn system_links() -> &'static [Link] {
    static LINKS: OnceLock<Vec<Link>> = OnceLock::new();
    LINKS.get_or_init(|| std::fs::read(FIRMLINKS_FILE).map(|text| parse(&text)).unwrap_or_default())
}

/// The identity a firmlink resolves to, when both routes reach the same Data-volume folder.
pub fn verified_identity(link: &Link) -> Option<FileIdentity> {
    let logical = paths::directory_identity(&link.logical)?;
    let physical = paths::directory_identity(&link.physical)?;
    let data = DATA_VOLUME.as_bytes();
    (logical == physical
        && paths::mount_of(&link.logical).as_deref() == Some(data)
        && paths::mount_of(&link.physical).as_deref() == Some(data))
    .then_some(logical)
}

/// Captures the boot layout from `links`. `None` unless `/` and the Data volume are mounted as
/// the boot volume group and `/Users` verifies (a mounted folder named Data alone is no evidence).
pub fn capture(links: &[Link]) -> Option<BootLayout> {
    let data = DATA_VOLUME.as_bytes();
    if paths::mount_of(b"/").as_deref() != Some(b"/".as_slice()) || paths::mount_of(data).as_deref() != Some(data) {
        return None;
    }
    let data_identity = paths::directory_identity(data)?;
    let mut logical_aliases = Vec::new();
    let mut physical_aliases = Vec::new();
    for link in links {
        if let Some(identity) = verified_identity(link) {
            logical_aliases.push((link.logical.clone(), identity));
            physical_aliases.push(link.physical.clone());
        }
    }
    if !logical_aliases.iter().any(|(path, _)| path == b"/Users") {
        return None;
    }
    Some(BootLayout {
        data_path: data.to_vec(),
        data_mount: DATA_VOLUME.to_string(),
        data_identity,
        logical_aliases,
        physical_aliases,
    })
}

/// Firmlinks grouped by the directory that contains their logical path, for listing.
pub struct FirmlinkDirectories {
    by_parent: HashMap<Vec<u8>, HashMap<Vec<u8>, Link>>,
}

impl FirmlinkDirectories {
    pub fn new(links: &[Link]) -> Self {
        let mut by_parent: HashMap<Vec<u8>, HashMap<Vec<u8>, Link>> = HashMap::new();
        for link in links {
            let parent = paths::parent(&link.logical).to_vec();
            let name = link.logical[parent.len()..].strip_prefix(b"/").unwrap_or(&link.logical[parent.len()..]);
            by_parent.entry(parent).or_default().insert(name.to_vec(), link.clone());
        }
        Self { by_parent }
    }

    pub fn system() -> &'static Self {
        static DIRS: OnceLock<FirmlinkDirectories> = OnceLock::new();
        DIRS.get_or_init(|| Self::new(system_links()))
    }

    /// Links whose logical path is a direct child of `directory`.
    pub fn in_directory(&self, directory: &[u8]) -> Option<&HashMap<Vec<u8>, Link>> {
        self.by_parent.get(directory)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_only_safe_two_field_lines() {
        let text = b"/Users\tUsers\n\
            /System/Library/Caches\tSystem/Library/Caches\r\n\
            relative\tUsers\n\
            /Absolute\t/Absolute\n\
            /Three\tfields\textra\n\
            /Dots\t../escape\n\
            /Dot\t./here\n\
            /Empty\ta//b\n\
            /Nul\tna\0me\n\
            /\troot\n\
            \n\
            /Non\xffUtf8\tNon\xffUtf8\n";
        let links = parse(text);
        let pairs: Vec<(&[u8], &[u8])> = links.iter().map(|l| (l.logical.as_slice(), l.physical.as_slice())).collect();
        assert_eq!(
            pairs,
            vec![
                (b"/Users".as_slice(), b"/System/Volumes/Data/Users".as_slice()),
                (b"/System/Library/Caches", b"/System/Volumes/Data/System/Library/Caches"),
                (b"/Non\xffUtf8", b"/System/Volumes/Data/Non\xffUtf8"),
            ]
        );
        let dirs = FirmlinkDirectories::new(&links);
        assert!(dirs.in_directory(b"/").unwrap().contains_key(b"Users".as_slice()));
        assert!(dirs.in_directory(b"/System/Library").unwrap().contains_key(b"Caches".as_slice()));
        assert!(dirs.in_directory(b"/System").is_none());
    }

    #[test]
    fn capture_keeps_only_verified_links() {
        let links = parse(b"/Users\tUsers\n/orbit-not-a-firmlink\torbit-not-a-firmlink\n/usr\tusr\n");
        let Some(layout) = capture(&links) else {
            eprintln!("SKIP: no split System/Data boot volume group");
            return;
        };
        let users = paths::directory_identity(b"/Users").unwrap();
        assert_eq!(layout.logical_aliases, vec![(b"/Users".to_vec(), users)]);
        assert_eq!(layout.physical_aliases, vec![b"/System/Volumes/Data/Users".to_vec()]);
        assert_eq!(layout.data_identity, paths::directory_identity(b"/System/Volumes/Data").unwrap());
        // Without a verified /Users the group is not trusted at all.
        assert!(capture(&parse(b"/Applications\tApplications\n")).is_none());
    }
}
