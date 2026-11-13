#![cfg(unix)]

use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

struct Home(PathBuf);

impl Home {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        loop {
            let id = NEXT.fetch_add(1, Ordering::Relaxed);
            let path =
                std::env::temp_dir().join(format!("herdr-permissions-{}-{id}", std::process::id()));
            match fs::create_dir(&path) {
                Ok(()) => {
                    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
                    return Self(path);
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("create test home: {error}"),
            }
        }
    }

    fn command(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_hd"))
            .arg("herdr")
            .args(args)
            .env("HOME", &self.0)
            .current_dir(&self.0)
            .output()
            .unwrap()
    }

    fn status(&self) -> Output {
        self.command(&["status"])
    }

    fn status_at(&self, home: &Path) -> Output {
        Command::new(env!("CARGO_BIN_EXE_hd"))
            .args(["herdr", "status"])
            .env("HOME", home)
            .current_dir(&self.0)
            .output()
            .unwrap()
    }

    fn status_with_umask(&self, mask: &str) -> Output {
        Command::new("sh")
            .args(["-c", "umask \"$1\"; shift; exec \"$@\"", "sh", mask])
            .arg(env!("CARGO_BIN_EXE_hd"))
            .args(["herdr", "status"])
            .env("HOME", &self.0)
            .current_dir(&self.0)
            .output()
            .unwrap()
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn mkdir(path: &Path, mode: u32) {
    fs::create_dir(path).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}

fn mode(path: &Path) -> u32 {
    fs::symlink_metadata(path).unwrap().mode() & 0o7777
}

fn assert_failure(output: Output, expected: &str) {
    assert!(!output.status.success(), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains(expected),
        "{output:?}"
    );
}

#[test]
fn validates_home_created_with_default_umask() {
    let root = Home::new();
    let home = root.0.join("home");
    fs::create_dir(&home).unwrap();
    let original = mode(&home);
    let output = root.status_at(&home);
    if original & 0o022 == 0 {
        assert!(output.status.success(), "{output:?}");
        assert_eq!(mode(&home.join(".herdr-devtunnel")), 0o700);
    } else {
        assert_failure(output, "group or world writable");
        assert!(!home.join(".herdr-devtunnel").exists());
    }
    assert_eq!(mode(&home), original);
}

#[test]
fn creates_private_directories_without_changing_home() {
    for mask in ["0000", "0002", "0022", "0077"] {
        let home = Home::new();
        fs::set_permissions(&home.0, fs::Permissions::from_mode(0o755)).unwrap();
        let output = home.status_with_umask(mask);
        assert!(output.status.success(), "umask {mask}: {output:?}");
        assert_eq!(mode(&home.0), 0o755);
        assert_eq!(mode(&home.0.join(".herdr-devtunnel")), 0o700);
        assert_eq!(mode(&home.0.join(".herdr-devtunnel/server-default")), 0o700);
    }
}

#[test]
fn refuses_permissive_managed_directories_without_changing_them() {
    let home = Home::new();
    let runtime = home.0.join(".herdr-devtunnel");
    let profile = runtime.join("server-default");
    mkdir(&runtime, 0o755);
    mkdir(&profile, 0o700);
    for args in [
        &["status"][..],
        &["stop"][..],
        &["reconnect", "--all", "--role", "server"][..],
    ] {
        assert_failure(home.command(args), "must have permissions 0700");
    }
    assert_eq!(mode(&runtime), 0o755);
    assert_eq!(mode(&profile), 0o700);
    assert!(!profile.join("management.lock").exists());
}

#[test]
fn allows_existing_profile_inside_private_runtime() {
    for permissions in [0o700, 0o750, 0o755, 0o770, 0o775, 0o777] {
        let home = Home::new();
        let runtime = home.0.join(".herdr-devtunnel");
        let profile = runtime.join("server-default");
        mkdir(&runtime, 0o700);
        mkdir(&profile, permissions);
        let output = home.status();
        assert!(output.status.success(), "{permissions:o}: {output:?}");
        assert_eq!(mode(&profile), permissions);
    }
}

#[test]
fn rejects_managed_symlinks_without_touching_targets() {
    for runtime_link in [true, false] {
        let home = Home::new();
        let target = home.0.join("target");
        mkdir(&target, 0o755);
        let runtime = home.0.join(".herdr-devtunnel");
        if runtime_link {
            symlink(&target, &runtime).unwrap();
        } else {
            mkdir(&runtime, 0o700);
            symlink(&target, runtime.join("server-default")).unwrap();
        }
        assert_failure(home.status(), "symbolic link");
        assert_eq!(mode(&target), 0o755);
        assert_eq!(fs::read_dir(&target).unwrap().count(), 0);
    }
}

#[test]
fn rejects_dangling_managed_symlinks() {
    let home = Home::new();
    let target = home.0.join("missing");
    symlink(&target, home.0.join(".herdr-devtunnel")).unwrap();
    assert_failure(home.status(), "symbolic link");
    assert!(!target.exists());
}

#[test]
fn allows_a_trusted_symlinked_home() {
    let home = Home::new();
    let actual = home.0.join("actual");
    let alias = home.0.join("alias");
    mkdir(&actual, 0o755);
    symlink(&actual, &alias).unwrap();
    let output = home.status_at(&alias);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(mode(&actual), 0o755);
    assert_eq!(mode(&actual.join(".herdr-devtunnel/server-default")), 0o700);
}

#[test]
fn does_not_change_home_or_its_ancestors() {
    let root = Home::new();
    let parent = root.0.join("parent");
    let home = parent.join("home");
    mkdir(&parent, 0o755);
    mkdir(&home, 0o755);
    let output = root.status_at(&home);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(mode(&parent), 0o755);
    assert_eq!(mode(&home), 0o755);
}

#[test]
fn rejects_writable_ancestors_without_modifying_them() {
    for permissions in [0o720, 0o702, 0o775, 0o777] {
        let root = Home::new();
        let parent = root.0.join("parent");
        let home = parent.join("home");
        mkdir(&parent, permissions);
        mkdir(&home, 0o755);
        assert_failure(root.status_at(&home), "writable home ancestor");
        assert_eq!(mode(&parent), permissions);
        assert_eq!(mode(&home), 0o755);
        assert!(!home.join(".herdr-devtunnel").exists());
    }
}

#[test]
fn rejects_non_root_sticky_writable_ancestor() {
    unsafe extern "C" {
        fn geteuid() -> u32;
    }
    if unsafe { geteuid() } == 0 {
        return;
    }
    let root = Home::new();
    let parent = root.0.join("sticky");
    let home = parent.join("home");
    mkdir(&parent, 0o1777);
    mkdir(&home, 0o755);
    assert_failure(root.status_at(&home), "writable home ancestor");
    assert_eq!(mode(&parent), 0o1777);
    assert!(!home.join(".herdr-devtunnel").exists());
}

#[test]
fn accepts_relative_symlinked_home() {
    let root = Home::new();
    let actual = root.0.join("actual");
    mkdir(&actual, 0o755);
    symlink("actual", root.0.join("alias")).unwrap();
    symlink("alias", root.0.join("chain")).unwrap();
    let output = root.status_at(Path::new("chain"));
    assert!(output.status.success(), "{output:?}");
    assert_eq!(mode(&actual), 0o755);
    assert_eq!(mode(&actual.join(".herdr-devtunnel")), 0o700);
}

#[test]
fn rejects_unsafe_symlink_targets_and_paths() {
    let root = Home::new();
    let unsafe_parent = root.0.join("unsafe");
    let safe_parent = root.0.join("safe");
    mkdir(&unsafe_parent, 0o777);
    mkdir(&safe_parent, 0o755);
    let safe_home = safe_parent.join("home");
    let unsafe_home = safe_parent.join("writable-home");
    mkdir(&safe_home, 0o755);
    mkdir(&unsafe_home, 0o775);
    let unsafe_target = unsafe_parent.join("home");
    mkdir(&unsafe_target, 0o755);
    let alias = safe_parent.join("alias");
    symlink(&unsafe_home, &alias).unwrap();
    assert_failure(root.status_at(&alias), "group or world writable");
    fs::remove_file(&alias).unwrap();
    symlink(&unsafe_target, &alias).unwrap();
    assert_failure(root.status_at(&alias), "writable home ancestor");
    let unsafe_alias = unsafe_parent.join("alias");
    symlink(&safe_home, &unsafe_alias).unwrap();
    assert_failure(root.status_at(&unsafe_alias), "writable home ancestor");
    assert_failure(
        root.status_at(&unsafe_parent.join("../safe/home")),
        "writable home ancestor",
    );
    assert_eq!(mode(&unsafe_parent), 0o777);
    assert_eq!(mode(&unsafe_home), 0o775);
    assert!(!safe_home.join(".herdr-devtunnel").exists());
    assert!(!unsafe_target.join(".herdr-devtunnel").exists());
    assert!(!unsafe_home.join(".herdr-devtunnel").exists());
}

#[test]
fn rejects_missing_and_cyclic_home_symlinks() {
    let root = Home::new();
    let alias = root.0.join("alias");
    symlink("missing", &alias).unwrap();
    assert_failure(root.status_at(&alias), "Failed to inspect");
    assert!(!root.0.join("missing").exists());
    fs::remove_file(&alias).unwrap();
    symlink("alias", &alias).unwrap();
    assert_failure(root.status_at(&alias), "Too many symbolic links");
}

#[test]
fn rejects_home_owned_by_another_user() {
    unsafe extern "C" {
        fn geteuid() -> u32;
        fn chown(path: *const std::ffi::c_char, owner: u32, group: u32) -> i32;
    }
    let root = Home::new();
    if unsafe { geteuid() } == 0 {
        use std::os::unix::ffi::OsStrExt;
        let home = root.0.join("other");
        mkdir(&home, 0o755);
        let path = std::ffi::CString::new(home.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { chown(path.as_ptr(), 12345, u32::MAX) }, 0);
        assert_failure(root.status_at(&home), "not owned by the current user");
        assert_eq!(fs::metadata(&home).unwrap().uid(), 12345);
        assert!(!home.join(".herdr-devtunnel").exists());
        let parent = root.0.join("parent");
        let nested = parent.join("home");
        mkdir(&parent, 0o755);
        mkdir(&nested, 0o755);
        let path = std::ffi::CString::new(parent.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { chown(path.as_ptr(), 12345, u32::MAX) }, 0);
        assert_failure(
            root.status_at(&nested),
            "not owned by the current user or root",
        );
        assert!(!nested.join(".herdr-devtunnel").exists());
        let alias = root.0.join("alias");
        symlink(&nested, &alias).unwrap();
        let path = std::ffi::CString::new(alias.as_os_str().as_bytes()).unwrap();
        unsafe extern "C" {
            fn lchown(path: *const std::ffi::c_char, owner: u32, group: u32) -> i32;
        }
        assert_eq!(unsafe { lchown(path.as_ptr(), 12345, u32::MAX) }, 0);
        assert_failure(root.status_at(&alias), "symbolic link not owned");
    }
}

#[test]
fn rejects_writable_home_without_modifying_it() {
    for permissions in [0o720, 0o702, 0o775, 0o777, 0o1777] {
        let home = Home::new();
        fs::set_permissions(&home.0, fs::Permissions::from_mode(permissions)).unwrap();
        for args in [&["status"][..], &["stop"][..]] {
            assert_failure(home.command(args), "group or world writable");
        }
        assert_eq!(mode(&home.0), permissions);
        assert!(!home.0.join(".herdr-devtunnel").exists());
        let runtime = home.0.join(".herdr-devtunnel");
        let profile = runtime.join("server-default");
        mkdir(&runtime, 0o700);
        mkdir(&profile, 0o700);
        assert_failure(
            home.command(&["reconnect", "--all", "--role", "server"]),
            "group or world writable",
        );
        assert!(!profile.join("management.lock").exists());
    }
}
