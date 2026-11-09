//! Guards around cloud placeholders and file replacement.
#![cfg(target_os = "macos")]

use orbit_platform::{Platform, PlatformError};
use orbit_platform_macos::MacPlatform;

const OFF: i32 = 1;

#[test]
fn materialization_is_disabled_only_inside_and_restored_after() {
    let platform = MacPlatform::new();
    std::thread::spawn(move || {
        let before = platform.materialization_policy();
        assert_ne!(before, OFF, "fresh threads start with the default policy");
        let (inside, nested_after) = platform
            .without_materialization(|| {
                let inside = platform.materialization_policy();
                platform.without_materialization(|| ()).unwrap();
                (inside, platform.materialization_policy())
            })
            .unwrap();
        assert_eq!(inside, OFF);
        assert_eq!(nested_after, OFF, "a nested scope must not re-enable downloads");
        assert_eq!(platform.materialization_policy(), before);
        let result = std::panic::catch_unwind(|| platform.without_materialization(|| panic!("body failed")));
        assert!(result.is_err());
        assert_eq!(platform.materialization_policy(), before, "policy leaked past a panic");
    })
    .join()
    .unwrap();
}

#[test]
fn clone_never_replaces_an_existing_destination() {
    let temp = tempfile::tempdir().unwrap();
    let (src, dst) = (temp.path().join("src"), temp.path().join("dst"));
    std::fs::write(&src, b"original").unwrap();
    let platform = MacPlatform::new();
    platform.clone_file(&src, &dst).unwrap();
    assert_eq!(std::fs::read(&dst).unwrap(), b"original");
    std::fs::write(&src, b"changed").unwrap();
    let err = platform.clone_file(&src, &dst).unwrap_err();
    assert!(matches!(err, PlatformError::Os { code: libc::EEXIST, .. }), "{err:?}");
    assert_eq!(std::fs::read(&dst).unwrap(), b"original");
}
