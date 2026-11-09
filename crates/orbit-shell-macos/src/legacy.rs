//! One-time import of the Swift app's `UserDefaults`.
//!
//! The Swift app stores (see `FolderPlaces.swift` and `DiskModel.swift`):
//! - `folderPlaces`: `[String]` of absolute folder paths added to the Places sidebar;
//! - `lastIndexedRoot`: `String`, the last mapped root, reopened on launch.

use std::path::{Component, Path, PathBuf};

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_foundation::{NSArray, NSDictionary, NSString, NSUserDefaults};
use orbit_platform::LegacySettings;

/// Defaults domain (bundle identifier) of the Swift app.
pub const LEGACY_DEFAULTS_DOMAIN: &str = "local.orbitdisk.app";

const PLACES_KEY: &str = "folderPlaces";
const LAST_LOCATION_KEY: &str = "lastIndexedRoot";

/// Reads the persistent domain `domain` without registering it as a suite, so it works from
/// any process (including the Rust app, which shares the bundle identifier, and tests).
/// Returns `None` when the domain holds neither key.
pub(crate) fn read(domain: &str) -> Option<LegacySettings> {
    let values = NSUserDefaults::standardUserDefaults().persistentDomainForName(&NSString::from_str(domain))?;
    let settings = LegacySettings {
        places: places(string_array(&values, PLACES_KEY).unwrap_or_default()),
        last_location: string(&values, LAST_LOCATION_KEY).and_then(|path| standardized(&path)),
    };
    (!settings.places.is_empty() || settings.last_location.is_some()).then_some(settings)
}

fn value(values: &NSDictionary<NSString, AnyObject>, key: &str) -> Option<Retained<AnyObject>> {
    values.objectForKey(&NSString::from_str(key))
}

fn string(values: &NSDictionary<NSString, AnyObject>, key: &str) -> Option<String> {
    Some(value(values, key)?.downcast::<NSString>().ok()?.to_string())
}

/// Like `UserDefaults.stringArray(forKey:)`: `None` unless the value is an array of strings only.
fn string_array(values: &NSDictionary<NSString, AnyObject>, key: &str) -> Option<Vec<String>> {
    let array = value(values, key)?.downcast::<NSArray>().ok()?;
    array.iter().map(|item| item.downcast::<NSString>().ok().map(|item| item.to_string())).collect()
}

/// Mirrors `FolderPlaces.init`: keeps absolute paths only, standardizes them and drops
/// duplicates while preserving order.
fn places(paths: Vec<String>) -> Vec<PathBuf> {
    let mut result: Vec<PathBuf> = Vec::with_capacity(paths.len());
    for path in paths.iter().filter_map(|path| standardized(path)) {
        if !result.contains(&path) {
            result.push(path);
        }
    }
    result
}

/// `URL(fileURLWithPath:).standardizedFileURL.path` for absolute paths: removes `.` components,
/// empty components and trailing slashes, and resolves `..` lexically (never above `/`).
fn standardized(path: &str) -> Option<PathBuf> {
    if !path.starts_with('/') {
        return None;
    }
    let mut result = PathBuf::from("/");
    for component in Path::new(path).components() {
        match component {
            Component::Normal(name) => result.push(name),
            Component::ParentDir => {
                result.pop();
            }
            Component::RootDir | Component::CurDir | Component::Prefix(_) => {}
        }
    }
    Some(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use objc2_foundation::NSNumber;

    /// Writes a real persistent defaults domain and removes it again on drop.
    struct TemporaryDomain(Retained<NSString>);

    impl TemporaryDomain {
        /// `tag` keeps tests that run in parallel threads apart.
        fn new(tag: &str, values: &[(&str, Retained<AnyObject>)]) -> Self {
            let name = NSString::from_str(&domain_name(tag));
            let keys: Vec<Retained<NSString>> = values.iter().map(|(key, _)| NSString::from_str(key)).collect();
            let keys: Vec<&NSString> = keys.iter().map(|key| &**key).collect();
            let objects: Vec<&AnyObject> = values.iter().map(|(_, value)| &**value).collect();
            let dictionary = NSDictionary::from_slices(&keys, &objects);
            // SAFETY: the dictionary holds only property-list values (strings, arrays, numbers).
            unsafe { NSUserDefaults::standardUserDefaults().setPersistentDomain_forName(&dictionary, &name) };
            Self(name)
        }

        fn read(&self) -> Option<LegacySettings> {
            read(&self.0.to_string())
        }
    }

    impl Drop for TemporaryDomain {
        fn drop(&mut self) {
            NSUserDefaults::standardUserDefaults().removePersistentDomainForName(&self.0);
            // cfprefsd leaves an empty plist behind; best effort, it may still be writing it.
            if let Some(home) = std::env::var_os("HOME") {
                let file = Path::new(&home).join(format!("Library/Preferences/{}.plist", self.0));
                let _ = std::fs::remove_file(file);
            }
        }
    }

    fn domain_name(tag: &str) -> String {
        format!("local.orbitdisk.tests.legacy-{tag}-{}", std::process::id())
    }

    fn strings(items: &[&str]) -> Retained<AnyObject> {
        let items: Vec<Retained<NSString>> = items.iter().map(|item| NSString::from_str(item)).collect();
        let items: Vec<&NSString> = items.iter().map(|item| &**item).collect();
        NSArray::from_slice(&items).into()
    }

    #[test]
    fn imports_swift_places_and_last_location_with_swift_semantics() {
        let domain = TemporaryDomain::new(
            "swift",
            &[
                (
                    PLACES_KEY,
                    strings(&["/Users/a/Projects/", "relative/dir", "/Users/a/./Projects", "/Volumes/X/../Data"]),
                ),
                (LAST_LOCATION_KEY, NSString::from_str("/Users/a/Café").into()),
            ],
        );
        assert_eq!(
            domain.read(),
            Some(LegacySettings {
                places: vec![PathBuf::from("/Users/a/Projects"), PathBuf::from("/Volumes/Data")],
                last_location: Some(PathBuf::from("/Users/a/Café")),
            })
        );
    }

    #[test]
    fn mixed_type_places_are_ignored_like_string_array() {
        let mixed: Retained<AnyObject> = {
            let path = NSString::from_str("/Users/a");
            let number = NSNumber::new_i32(3);
            NSArray::from_slice(&[&*path as &AnyObject, &*number as &AnyObject]).into()
        };
        let domain = TemporaryDomain::new("mixed", &[(PLACES_KEY, mixed)]);
        assert_eq!(domain.read(), None);
        drop(domain);
        assert_eq!(read(&domain_name("mixed")), None);
    }
}
