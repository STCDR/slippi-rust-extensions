//! Credential lookup for the portable Dolphin Dubs profile.
//! Launcher credentials are borrowed read-only; local credentials take precedence.

use std::path::{Path, PathBuf};
use std::{fs, io};

#[derive(Debug)]
pub(super) struct AccountFiles {
    local: PathBuf,
    launcher_root: Option<PathBuf>,
}

impl AccountFiles {
    pub fn new(local: PathBuf) -> Self {
        Self {
            local,
            launcher_root: launcher_root(),
        }
    }

    pub fn local_path(&self) -> &Path {
        &self.local
    }

    pub fn read(&self) -> io::Result<String> {
        fs::read_to_string(self.resolve()?)
    }

    // Resolve on every login attempt, including the watcher, so signing into the
    // official Launcher while Dolphin is already open can complete login.
    fn resolve(&self) -> io::Result<PathBuf> {
        match fs::metadata(&self.local) {
            Ok(_) => return Ok(self.local.clone()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {},
            Err(error) => return Err(error),
        }

        // An optional explicit file location supports relocated/custom installs.
        // A broken explicit location must not silently select a different account.
        let location_file = self.local.with_file_name("external-user-json.txt");
        match fs::read_to_string(location_file) {
            Ok(location) if !location.trim().is_empty() => {
                let path = PathBuf::from(location.trim().trim_start_matches('\u{feff}'));
                if !path.is_absolute() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "external-user-json.txt must contain an absolute user.json path",
                    ));
                }
                return Ok(path);
            },
            Ok(_) => {},
            Err(error) if error.kind() == io::ErrorKind::NotFound => {},
            Err(error) => return Err(error),
        }

        let Some(root) = &self.launcher_root else {
            return Ok(self.local.clone());
        };
        Ok(launcher_user_directory(root)?.join("Slippi").join("user.json"))
    }

    pub fn logout(&self) -> io::Result<()> {
        // Never unlink the resolved external file: it belongs to the Launcher.
        match fs::remove_file(&self.local) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            result => result,
        }
    }
}

// Shared by credential lookup and the first-run settings import.
pub(super) fn launcher_user_directory(root: &Path) -> io::Result<PathBuf> {
    // electron-settings stores the beta preference under settings and the
    // promotion flag at the root of the file named Settings.
    let settings = match fs::read(root.join("Settings")) {
        Ok(contents) => serde_json::from_slice::<serde_json::Value>(&contents)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => serde_json::Value::Null,
        Err(error) => return Err(error),
    };
    let beta = settings["settings"]["useNetplayBeta"].as_bool().unwrap_or(false);
    let promoted = settings["netplayPromotedToStable"].as_bool().unwrap_or(false);
    let folder = if beta && !promoted { "netplay-beta" } else { "netplay" };
    Ok(root.join(folder).join("User"))
}

#[cfg(windows)]
pub(super) fn launcher_root() -> Option<PathBuf> {
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::System::Com::CoTaskMemFree;
    use windows_sys::Win32::UI::Shell::{FOLDERID_RoamingAppData, SHGetKnownFolderPath};

    // Ask Windows rather than constructing C:\Users paths; redirected profiles
    // and non-ASCII account names are supported.
    let mut path = std::ptr::null_mut();
    let root = unsafe {
        let result = SHGetKnownFolderPath(&FOLDERID_RoamingAppData, 0, std::ptr::null_mut(), &mut path);
        if result < 0 || path.is_null() {
            None
        } else {
            let mut length = 0;
            while *path.add(length) != 0 {
                length += 1;
            }
            Some(PathBuf::from(OsString::from_wide(std::slice::from_raw_parts(path, length))))
        }
    };
    if !path.is_null() {
        unsafe { CoTaskMemFree(path.cast()) };
    }
    root.or_else(|| std::env::var_os("APPDATA").map(PathBuf::from))
        .map(|root| root.join("Slippi Launcher"))
}

#[cfg(not(windows))]
pub(super) fn launcher_root() -> Option<PathBuf> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static NEXT: AtomicUsize = AtomicUsize::new(0);

    struct Fixture {
        root: PathBuf,
        accounts: AccountFiles,
    }

    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "dubs-login-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            let local = root.join("Dubs").join("User").join("Slippi").join("user.json");
            fs::create_dir_all(local.parent().unwrap()).unwrap();
            let launcher_root = root.join("Slippi Launcher");
            fs::create_dir_all(&launcher_root).unwrap();
            Self {
                root,
                accounts: AccountFiles {
                    local,
                    launcher_root: Some(launcher_root),
                },
            }
        }

        fn launcher(&self, folder: &str, contents: &str) -> PathBuf {
            let path = self
                .accounts
                .launcher_root
                .as_ref()
                .unwrap()
                .join(folder)
                .join("User")
                .join("Slippi")
                .join("user.json");
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, contents).unwrap();
            path
        }

        fn settings(&self, contents: &str) {
            fs::write(self.accounts.launcher_root.as_ref().unwrap().join("Settings"), contents).unwrap();
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.root).unwrap();
        }
    }

    #[test]
    fn portable_override_wins_and_does_not_modify_launcher() {
        let fixture = Fixture::new();
        let remote = fixture.launcher("netplay", "launcher-account");
        fs::write(&fixture.accounts.local, "portable-account").unwrap();
        assert_eq!(fixture.accounts.read().unwrap(), "portable-account");
        fixture.accounts.logout().unwrap();
        assert!(!fixture.accounts.local.exists());
        assert_eq!(fs::read_to_string(remote).unwrap(), "launcher-account");
        assert_eq!(fixture.accounts.read().unwrap(), "launcher-account");
    }

    #[test]
    fn borrowed_login_survives_logout_and_tracks_replaced_account() {
        let fixture = Fixture::new();
        let remote = fixture.launcher("netplay", "account-one");
        assert_eq!(fixture.accounts.read().unwrap(), "account-one");
        fixture.accounts.logout().unwrap();
        fs::write(&remote, "account-two").unwrap();
        assert_eq!(fixture.accounts.read().unwrap(), "account-two");
        fs::remove_file(remote).unwrap();
        assert_eq!(fixture.accounts.read().unwrap_err().kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn follows_beta_preference_and_promotion_without_using_stale_accounts() {
        let fixture = Fixture::new();
        fixture.launcher("netplay", "stable-account");
        let beta = fixture.launcher("netplay-beta", "beta-account");
        fixture.settings(r#"{"settings":{"useNetplayBeta":true}}"#);
        assert_eq!(fixture.accounts.read().unwrap(), "beta-account");
        fs::remove_file(&beta).unwrap();
        assert_eq!(fixture.accounts.read().unwrap_err().kind(), io::ErrorKind::NotFound);
        fixture.settings(r#"{"settings":{"useNetplayBeta":true},"netplayPromotedToStable":true}"#);
        assert_eq!(fixture.accounts.read().unwrap(), "stable-account");
    }

    #[test]
    fn local_file_is_authoritative_even_if_empty_or_invalid() {
        let fixture = Fixture::new();
        fixture.launcher("netplay", "launcher-account");
        fs::write(&fixture.accounts.local, "invalid-json").unwrap();
        assert_eq!(fixture.accounts.read().unwrap(), "invalid-json");
        fs::write(&fixture.accounts.local, "").unwrap();
        assert_eq!(fixture.accounts.read().unwrap(), "");
    }

    #[test]
    fn supports_explicit_unicode_location_and_preserves_external_file() {
        let fixture = Fixture::new();
        let external = fixture.root.join("another account é.json");
        fs::write(&external, "external-account").unwrap();
        fs::write(
            fixture.accounts.local.with_file_name("external-user-json.txt"),
            format!("\u{feff}{}\n", external.display()),
        )
        .unwrap();
        assert_eq!(fixture.accounts.read().unwrap(), "external-account");
        fixture.accounts.logout().unwrap();
        assert_eq!(fs::read_to_string(external).unwrap(), "external-account");
    }

    #[test]
    fn watcher_can_discover_credentials_that_appear_after_startup() {
        let fixture = Fixture::new();
        assert_eq!(fixture.accounts.read().unwrap_err().kind(), io::ErrorKind::NotFound);
        fixture.launcher("netplay", "new-login");
        assert_eq!(fixture.accounts.read().unwrap(), "new-login");
        fs::write(
            fixture.accounts.local.with_file_name("external-user-json.txt"),
            "missing.json",
        )
        .unwrap();
        assert_eq!(fixture.accounts.read().unwrap_err().kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn reads_launcher_credentials_without_optional_latest_version() {
        let fixture = Fixture::new();
        fixture.launcher(
            "netplay",
            r#"{"uid":"fixture-uid","playKey":"fixture-key","displayName":"Fixture","connectCode":"TEST#123"}"#,
        );
        let user: crate::UserInfo = serde_json::from_str(&fixture.accounts.read().unwrap()).unwrap();
        assert_eq!(user.uid, "fixture-uid");
        assert_eq!(user.play_key, "fixture-key");
        assert!(user.latest_version.is_empty());
    }

    #[test]
    #[cfg(windows)]
    fn windows_known_folder_matches_the_current_roaming_profile() {
        let expected = PathBuf::from(std::env::var_os("APPDATA").unwrap()).join("Slippi Launcher");
        assert_eq!(launcher_root().unwrap(), expected);
    }
}
