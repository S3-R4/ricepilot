//! One real fixture per shape. `observe` is the only part of M1 that touches
//! a filesystem, so it is the only part worth testing against one.

mod common;

use std::path::{Path, PathBuf};

use common::Fixture;
use ricepilot::observe::{observe_one, LedgerEntry, Ownership, Shape};
use ricepilot::ops::read;

/// The ownership oracle a real `init` would produce for `dest`: the profile
/// root is registered *and* a ledger row matches the link's identity.
fn ownership_for(dest: &Path, root: &Path) -> Ownership {
    let target = read::readlink(dest).unwrap();
    let meta = read::lstat(dest).unwrap().unwrap();
    Ownership {
        profile_roots: vec![root.to_path_buf()],
        entries: vec![LedgerEntry {
            dest: dest.to_path_buf(),
            target,
            dev: meta.dev,
            ino: meta.ino,
        }],
    }
}

#[test]
fn shape_1_owned_link() {
    let f = Fixture::new("shape1");
    let root = f.dir("profiles/caelestia");
    f.dir("profiles/caelestia/hypr");
    let dest = f.link(".config/hypr", &root.join("hypr"));

    let own = ownership_for(&dest, &root);
    let o = observe_one(&dest, &own).unwrap();
    assert_eq!(
        o.shape,
        Shape::OwnedLink {
            target: root.join("hypr")
        }
    );
    assert!(!o.is_mountpoint);
}

#[test]
fn shape_2_foreign_link_when_ledger_disagrees() {
    let f = Fixture::new("shape2a");
    let root = f.dir("profiles/caelestia");
    f.dir("profiles/caelestia/hypr");
    let dest = f.link(".config/hypr", &root.join("hypr"));

    // Root registered, link in place, but no ledger row: fact 3 fails, so
    // the path is unowned even though it looks exactly right.
    let own = Ownership {
        profile_roots: vec![root.clone()],
        entries: vec![],
    };
    assert!(matches!(
        observe_one(&dest, &own).unwrap().shape,
        Shape::ForeignLink {
            dangling: false,
            ..
        }
    ));
}

#[test]
fn shape_2_foreign_link_when_target_is_outside_every_profile_root() {
    let f = Fixture::new("shape2b");
    let root = f.dir("profiles/caelestia");
    let elsewhere = f.dir("somewhere-else");
    let dest = f.link(".config/hypr", &elsewhere);

    // Even with a ledger row that matches perfectly, fact 2 fails.
    let mut own = ownership_for(&dest, &root);
    own.profile_roots = vec![root];
    assert!(matches!(
        observe_one(&dest, &own).unwrap().shape,
        Shape::ForeignLink {
            dangling: false,
            ..
        }
    ));
}

#[test]
fn shape_2_dangling_link_is_still_foreign() {
    let f = Fixture::new("shape2c");
    let root = f.dir("profiles/caelestia");
    let dest = f.link(".config/hypr", &root.join("does-not-exist"));

    let own = Ownership::default();
    let o = observe_one(&dest, &own).unwrap();
    assert!(
        matches!(o.shape, Shape::ForeignLink { dangling: true, .. }),
        "a dangling link is a link we did not create: {:?}",
        o.shape
    );
}

#[test]
fn shape_3_real_dir() {
    let f = Fixture::new("shape3");
    f.clear(".config/hypr");
    let dest = f.dir(".config/hypr");
    let o = observe_one(&dest, &Ownership::default()).unwrap();
    assert_eq!(o.shape, Shape::RealDir);
    assert!(
        !o.is_mountpoint,
        "a plain dir on the same device is not a mount point"
    );
}

#[test]
fn shape_4_real_file() {
    let f = Fixture::new("shape4");
    f.clear(".config/starship.toml");
    let dest = f.file(".config/starship.toml", "# hi\n");
    assert_eq!(
        observe_one(&dest, &Ownership::default()).unwrap().shape,
        Shape::RealFile
    );
}

#[test]
fn shape_5_absent() {
    let f = Fixture::new("shape5");
    f.clear(".config/hypr");
    let dest = f.path(".config/hypr");
    assert_eq!(
        observe_one(&dest, &Ownership::default()).unwrap().shape,
        Shape::Absent
    );
}

#[test]
fn observe_records_the_parents_device_and_filesystem_type() {
    let f = Fixture::new("parentfacts");
    f.clear(".config/hypr");
    let dest = f.path(".config/hypr");
    let o = observe_one(&dest, &Ownership::default()).unwrap();
    let (dev, fs_type) = read::dev_and_fs_type(&f.path(".config")).unwrap();
    assert_eq!(o.parent_dev, dev);
    assert_eq!(o.parent_fs_type, fs_type);
}

#[test]
fn a_symlinked_intermediate_component_is_refused_not_followed() {
    let f = Fixture::new("symlinked_parent");
    let real = f.dir("real-config");
    f.clear("shadow");
    f.link("shadow", &real);

    // `<home>/shadow` is a link; asking about `<home>/shadow/hypr` must not
    // quietly answer a question about `<home>/real-config/hypr`.
    let err = observe_one(&f.path("shadow/hypr"), &Ownership::default()).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("symlink"), "{msg}");
    assert!(msg.contains("shadow"), "{msg}");
}

#[test]
fn readlink_returns_the_raw_target_not_a_resolved_one() {
    let f = Fixture::new("rawtarget");
    f.clear(".config/foot");
    f.link(".config/foot", std::path::Path::new("../relative/foot"));
    assert_eq!(
        read::readlink(&f.path(".config/foot")).unwrap(),
        PathBuf::from("../relative/foot")
    );
}
