//! Sidebar folder places (Swift `FolderPlaces`). Persistence is an effect; validation of dropped
//! folders (exists, is a directory, symlinks resolved) is a backend request.

use orbit_core::paths::{SEP, is_within, join};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Places {
    folders: Vec<Vec<u8>>,
    builtins: Vec<Vec<u8>>,
}

impl Places {
    /// Loads saved places: absolute paths only, without duplicates. `home` defines the built-in
    /// places (Home, Downloads, Documents, Applications) that are never added again.
    pub fn new(saved: impl IntoIterator<Item = Vec<u8>>, home: &[u8]) -> Self {
        let mut folders: Vec<Vec<u8>> = Vec::new();
        for path in saved {
            let path = standardized(&path);
            if path.first() == Some(&SEP) && !folders.contains(&path) {
                folders.push(path);
            }
        }
        let builtins =
            vec![home.to_vec(), join(home, b"Downloads"), join(home, b"Documents"), b"/Applications".to_vec()];
        Self { folders, builtins }
    }

    pub fn folders(&self) -> &[Vec<u8>] {
        &self.folders
    }

    /// Adds resolved directories not already listed (nor built in). Returns whether anything changed.
    pub fn add(&mut self, directories: impl IntoIterator<Item = Vec<u8>>) -> bool {
        let mut changed = false;
        for directory in directories {
            let directory = standardized(&directory);
            if directory.first() != Some(&SEP)
                || self.folders.contains(&directory)
                || self.builtins.contains(&directory)
            {
                continue;
            }
            self.folders.push(directory);
            changed = true;
        }
        changed
    }

    pub fn remove(&mut self, path: &[u8]) -> bool {
        let before = self.folders.len();
        self.folders.retain(|folder| folder != path);
        before != self.folders.len()
    }
}

/// The place to highlight for `current`: the deepest place containing it, compared by whole
/// components (`/a/bc` is not inside `/a/b`).
pub fn selected_place<'a>(current: Option<&[u8]>, places: impl IntoIterator<Item = &'a [u8]>) -> Option<&'a [u8]> {
    let current = current?;
    places.into_iter().filter(|place| is_within(current, place)).max_by_key(|place| place.len())
}

fn standardized(path: &[u8]) -> Vec<u8> {
    crate::paths::normalized(path).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selected_place_is_the_deepest_component_wise_container() {
        let places: Vec<&[u8]> = vec![b"/", b"/Users/me", b"/Users/me/Projects", b"/Users/me/Proj"];
        assert_eq!(
            selected_place(Some(b"/Users/me/Projects/x"), places.iter().copied()),
            Some(&b"/Users/me/Projects"[..])
        );
        assert_eq!(selected_place(Some(b"/Users/me/Projectsx"), places.iter().copied()), Some(&b"/Users/me"[..]));
        assert_eq!(selected_place(Some(b"/Volumes/x"), places.iter().copied()), Some(&b"/"[..]));
        assert_eq!(selected_place(None, places.iter().copied()), None);
    }

    #[test]
    fn places_are_unique_absolute_and_exclude_builtins() {
        let mut places = Places::new([b"/a".to_vec(), b"/a/".to_vec(), b"rel".to_vec(), b"/b".to_vec()], b"/Users/me");
        assert_eq!(places.folders(), [b"/a".to_vec(), b"/b".to_vec()]);
        assert!(!places.add([b"/a".to_vec(), b"/Users/me/Downloads".to_vec(), b"/Applications".to_vec()]));
        assert!(places.add([b"/c".to_vec(), b"/c".to_vec()]));
        assert_eq!(places.folders().len(), 3);
        assert!(places.remove(b"/a"));
        assert!(!places.remove(b"/a"));
        assert_eq!(places.folders(), [b"/b".to_vec(), b"/c".to_vec()]);
    }
}
