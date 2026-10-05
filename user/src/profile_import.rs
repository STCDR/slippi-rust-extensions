//! First-run import into an untouched portable profile, before Dolphin loads it.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use crate::account_files::{launcher_root, launcher_user_directory};

const MARKER: &str = "launcher-import.txt";
const DEFAULT_DOLPHIN: &str = include_str!("../defaults/Dolphin.ini");
const DEFAULT_TEAMS: &str = include_str!("../defaults/local-teams.ini");

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn linked(metadata: &fs::Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return true;
        }
    }
    false
}

fn ini_values(text: &str) -> Option<BTreeMap<(String, String), String>> {
    let mut values = BTreeMap::new();
    let mut section = String::new();
    for line in text.trim_start_matches('\u{feff}').lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with(';') || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            section = line[1..line.len() - 1].trim().to_ascii_lowercase();
        } else {
            let (key, value) = line.split_once('=')?;
            if section.is_empty() || key.trim().is_empty() {
                return None;
            }
            let key = (section.clone(), key.trim().to_ascii_lowercase());
            if values.insert(key, value.trim().to_owned()).is_some() {
                return None;
            }
        }
    }
    Some(values)
}

fn default_ini(path: &Path, defaults: &str) -> io::Result<bool> {
    let contents = fs::read_to_string(path)?;
    let Some(values) = ini_values(&contents) else {
        return Ok(false);
    };
    let defaults = ini_values(defaults).expect("valid compiled defaults");
    // Missing keys use defaults. Every present setting must be a shipped default.
    Ok(values.iter().all(|(key, value)| defaults.get(key) == Some(value)))
}

fn scan_default(directory: &Path, relative: &Path, ignore_marker: bool) -> io::Result<bool> {
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let metadata = fs::symlink_metadata(entry.path())?;
        if linked(&metadata) {
            return Ok(false);
        }
        let relative = relative.join(entry.file_name());
        if metadata.is_dir() {
            if !scan_default(&entry.path(), &relative, ignore_marker)? {
                return Ok(false);
            }
        } else if metadata.is_file() {
            let name = relative.to_string_lossy().replace('\\', "/").to_ascii_lowercase();
            let allowed = match name.as_str() {
                "config/dolphin.ini" => default_ini(&entry.path(), DEFAULT_DOLPHIN)?,
                "config/local-teams.ini" => default_ini(&entry.path(), DEFAULT_TEAMS)?,
                "slippi/readme.txt" => true,
                MARKER if ignore_marker => true,
                _ => false,
            };
            if !allowed {
                return Ok(false);
            }
        } else {
            return Ok(false);
        }
    }
    Ok(true)
}

fn is_default_profile(user: &Path, ignore_marker: bool) -> io::Result<bool> {
    match fs::symlink_metadata(user) {
        Ok(metadata) if metadata.is_dir() && !linked(&metadata) => scan_default(user, Path::new(""), ignore_marker),
        Ok(_) => Ok(false),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(true),
        Err(error) => Err(error),
    }
}

fn remember(user: &Path, choice: &str) -> io::Result<bool> {
    fs::create_dir_all(user)?;
    match OpenOptions::new().write(true).create_new(true).open(user.join(MARKER)) {
        Ok(mut file) => {
            file.write_all(choice.as_bytes())?;
            Ok(true)
        },
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(false),
        Err(error) => Err(error),
    }
}

struct ImportFile {
    relative: PathBuf,
    contents: Vec<u8>,
}

fn collect(directory: &Path, relative: &Path, files: &mut Vec<ImportFile>) -> io::Result<()> {
    let metadata = match fs::symlink_metadata(directory) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    if linked(&metadata) || !metadata.is_dir() {
        return Err(invalid("Launcher settings directory is linked or invalid"));
    }
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let metadata = fs::symlink_metadata(entry.path())?;
        if linked(&metadata) {
            return Err(invalid("Linked files cannot be imported"));
        }
        let relative = relative.join(entry.file_name());
        if metadata.is_dir() {
            collect(&entry.path(), &relative, files)?;
        } else if metadata.is_file() {
            let name = relative.to_string_lossy().replace('\\', "/").to_ascii_lowercase();
            // This build's local Teams configuration must keep its paired defaults.
            if name == "config/local-teams.ini" || name == "slippi/readme.txt" {
                continue;
            }
            files.push(ImportFile {
                relative,
                contents: fs::read(entry.path())?,
            });
        } else {
            return Err(invalid("Unsupported Launcher settings file"));
        }
    }
    Ok(())
}

fn source_files(source: &Path) -> io::Result<Vec<ImportFile>> {
    let mut files = Vec::new();
    for folder in ["Config", "GameSettings", "Slippi"] {
        collect(&source.join(folder), Path::new(folder), &mut files)?;
    }
    files.sort_by(|left, right| left.relative.cmp(&right.relative));
    Ok(files)
}

fn available(user: &Path, root: Option<&Path>) -> io::Result<bool> {
    if user.join(MARKER).try_exists()? {
        return Ok(false);
    }
    if !is_default_profile(user, false)? {
        remember(user, "existing-profile\n")?;
        return Ok(false);
    }
    let source = root.map(launcher_user_directory).transpose()?;
    if let Some(source) = source {
        if source.is_dir() && !source_files(&source)?.is_empty() {
            if user.exists() && fs::canonicalize(user)? == fs::canonicalize(&source)? {
                remember(user, "same-profile\n")?;
                return Ok(false);
            }
            return Ok(true);
        }
    }
    remember(user, "no-launcher-profile\n")?;
    Ok(false)
}

/// Called once by the GUI before config/controller initialization.
pub fn launcher_import_available(user: &Path) -> io::Result<bool> {
    available(user, launcher_root().as_deref())
}

fn apply_files(user: &Path, files: &[ImportFile]) -> io::Result<()> {
    let mut backups: Vec<(PathBuf, Option<Vec<u8>>)> = Vec::new();
    for file in files {
        let target = user.join(&file.relative);
        let result = (|| {
            let previous = match fs::read(&target) {
                Ok(contents) => Some(contents),
                Err(error) if error.kind() == io::ErrorKind::NotFound => None,
                Err(error) => return Err(error),
            };
            fs::create_dir_all(target.parent().expect("import file parent"))?;
            backups.push((target.clone(), previous));
            fs::write(&target, &file.contents)
        })();
        if let Err(error) = result {
            let mut rollback_error = None;
            for (target, previous) in backups.into_iter().rev() {
                let restored = match previous {
                    Some(contents) => fs::write(target, contents),
                    None => fs::remove_file(target).or_else(|error| {
                        if error.kind() == io::ErrorKind::NotFound {
                            Ok(())
                        } else {
                            Err(error)
                        }
                    }),
                };
                if let Err(error) = restored {
                    rollback_error = Some(error);
                }
            }
            return Err(rollback_error.unwrap_or(error));
        }
    }
    Ok(())
}

fn finish(user: &Path, root: Option<&Path>, import: bool) -> io::Result<bool> {
    if user.join(MARKER).try_exists()? {
        return Ok(false);
    }
    if !import {
        remember(user, "declined\n")?;
        return Ok(false);
    }
    // Recheck after the prompt and read the entire source before writing anything.
    if !is_default_profile(user, false)? {
        remember(user, "profile-changed\n")?;
        return Ok(false);
    }
    let root = root.ok_or_else(|| invalid("Launcher profile is unavailable"))?;
    let source = launcher_user_directory(root)?;
    if !source.is_dir() || (user.exists() && fs::canonicalize(user)? == fs::canonicalize(&source)?) {
        return Err(invalid("Launcher profile is unavailable"));
    }
    let files = source_files(&source)?;
    if files.is_empty() {
        return Err(invalid("Launcher profile has no settings"));
    }
    if !remember(user, "importing\n")? {
        return Ok(false);
    }
    if !is_default_profile(user, true)? {
        fs::write(user.join(MARKER), "profile-changed\n")?;
        return Ok(false);
    }
    match apply_files(user, &files) {
        Ok(()) => {
            fs::write(user.join(MARKER), "imported\n")?;
            Ok(true)
        },
        Err(error) => {
            let _ = fs::write(user.join(MARKER), "failed\n");
            Err(error)
        },
    }
}

/// Records either answer. Yes copies settings/account files into the local profile.
pub fn finish_launcher_import(user: &Path, import: bool) -> io::Result<bool> {
    finish(user, launcher_root().as_deref(), import)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static NEXT: AtomicUsize = AtomicUsize::new(0);
    struct Fixture {
        root: PathBuf,
        user: PathBuf,
        launcher: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "dubs-import-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            let user = root.join("Dubs/User");
            let launcher = root.join("Slippi Launcher");
            fs::create_dir_all(&launcher).unwrap();
            Self { root, user, launcher }
        }
        fn write(&self, relative: &str, contents: &str) {
            let path = self.user.join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, contents).unwrap();
        }
        fn source(&self, folder: &str, relative: &str, contents: &str) -> PathBuf {
            let path = self.launcher.join(folder).join("User").join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, contents).unwrap();
            path
        }
        fn defaults(&self) {
            self.write("Config/Dolphin.ini", DEFAULT_DOLPHIN);
            self.write("Config/local-teams.ini", DEFAULT_TEAMS);
            self.write("Slippi/README.txt", "portable account instructions");
        }
        fn available(&self) -> bool {
            available(&self.user, Some(&self.launcher)).unwrap()
        }
        fn finish(&self, import: bool) -> bool {
            finish(&self.user, Some(&self.launcher), import).unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.root).unwrap();
        }
    }

    #[test]
    fn imports_all_settings_and_account_before_first_config_load() {
        let fixture = Fixture::new();
        fixture.defaults();
        let dolphin = fixture.source("netplay", "Config/Dolphin.ini", "[Core]\nSIDevice0=12\n");
        let pads = fixture.source("netplay", "Config/GCPadNew.ini", "[GCPad1]\nDevice=test-controller\n");
        fixture.source("netplay", "Config/Profiles/GCPad/custom.ini", "custom-preset");
        fixture.source("netplay", "GameSettings/GALE01.ini", "[Video]\nVSync=False\n");
        let account = fixture.source("netplay", "Slippi/user.json", "account-fixture");
        fixture.source("netplay", "Slippi/teams-codes.json", "[\"ROOM\"]");
        fixture.source("netplay", "Cache/shaders.bin", "not-settings");
        fixture.source("netplay", "Config/local-teams.ini", "[LocalTeams]\nEnabled=False\n");
        assert!(fixture.available());
        assert!(fixture.finish(true));
        assert_eq!(
            fs::read(fixture.user.join("Config/Dolphin.ini")).unwrap(),
            fs::read(dolphin).unwrap()
        );
        assert_eq!(
            fs::read(fixture.user.join("Config/GCPadNew.ini")).unwrap(),
            fs::read(pads).unwrap()
        );
        assert_eq!(
            fs::read(fixture.user.join("Slippi/user.json")).unwrap(),
            fs::read(&account).unwrap()
        );
        assert_eq!(fs::read_to_string(account).unwrap(), "account-fixture");
        assert_eq!(
            fs::read_to_string(fixture.user.join("Config/local-teams.ini")).unwrap(),
            DEFAULT_TEAMS
        );
        assert!(fixture.user.join("Config/Profiles/GCPad/custom.ini").exists());
        assert!(fixture.user.join("GameSettings/GALE01.ini").exists());
        assert!(fixture.user.join("Slippi/teams-codes.json").exists());
        assert!(!fixture.user.join("Cache/shaders.bin").exists());
        assert!(!fixture.available());
    }

    #[test]
    fn no_preserves_defaults_and_never_prompts_again() {
        let fixture = Fixture::new();
        fixture.defaults();
        fixture.source("netplay", "Config/GCPadNew.ini", "configured-pad");
        assert!(fixture.available());
        assert!(!fixture.finish(false));
        assert!(!fixture.available());
        assert!(!fixture.user.join("Config/GCPadNew.ini").exists());
        assert_eq!(
            fs::read_to_string(fixture.user.join("Config/Dolphin.ini")).unwrap(),
            DEFAULT_DOLPHIN
        );
    }

    #[test]
    fn every_custom_setting_or_local_account_blocks_the_prompt() {
        for (relative, contents) in [
            ("Config/Dolphin.ini", "[Core]\nSIDevice0=12\n"),
            ("Config/local-teams.ini", "[LocalTeams]\nCount=2\n"),
            ("Config/GCPadNew.ini", "configured-pad"),
            ("GameSettings/GALE01.ini", "[Core]\nCPUThread=False\n"),
            ("Slippi/user.json", ""),
            ("Slippi/external-user-json.txt", "some-location"),
        ] {
            let fixture = Fixture::new();
            fixture.defaults();
            fixture.source("netplay", "Config/Dolphin.ini", "launcher-settings");
            fixture.write(relative, contents);
            assert!(!fixture.available(), "{relative}");
            assert!(!fixture.finish(true));
            assert_eq!(fs::read_to_string(fixture.user.join(relative)).unwrap(), contents);
        }
    }

    #[test]
    fn empty_profile_and_commented_defaults_are_eligible() {
        let fixture = Fixture::new();
        fixture.source("netplay", "Config/Dolphin.ini", "launcher-settings");
        assert!(fixture.available());
        fixture.defaults();
        fixture.write(
            "Config/Dolphin.ini",
            &format!("\u{feff}; comments\r\n{}", DEFAULT_DOLPHIN.replace('\n', "\r\n")),
        );
        assert!(fixture.available());
    }

    #[test]
    fn beta_and_promotion_follow_the_same_profile_as_login() {
        let fixture = Fixture::new();
        fixture.defaults();
        fixture.source("netplay", "Config/GCPadNew.ini", "stable-pad");
        fixture.source("netplay-beta", "Config/GCPadNew.ini", "beta-pad");
        fs::write(fixture.launcher.join("Settings"), r#"{"settings":{"useNetplayBeta":true}}"#).unwrap();
        assert!(fixture.available());
        assert!(fixture.finish(true));
        assert_eq!(
            fs::read_to_string(fixture.user.join("Config/GCPadNew.ini")).unwrap(),
            "beta-pad"
        );
        let promoted = Fixture::new();
        promoted.defaults();
        promoted.source("netplay", "Config/GCPadNew.ini", "promoted-pad");
        promoted.source("netplay-beta", "Config/GCPadNew.ini", "stale-pad");
        fs::write(
            promoted.launcher.join("Settings"),
            r#"{"settings":{"useNetplayBeta":true},"netplayPromotedToStable":true}"#,
        )
        .unwrap();
        assert!(promoted.finish(true));
        assert_eq!(
            fs::read_to_string(promoted.user.join("Config/GCPadNew.ini")).unwrap(),
            "promoted-pad"
        );
    }

    #[test]
    fn missing_selected_profile_does_not_borrow_an_inactive_installation() {
        let fixture = Fixture::new();
        fixture.defaults();
        fixture.source("netplay", "Config/Dolphin.ini", "inactive-settings");
        fs::write(fixture.launcher.join("Settings"), r#"{"settings":{"useNetplayBeta":true}}"#).unwrap();
        assert!(!fixture.available());
        fixture.source("netplay-beta", "Config/Dolphin.ini", "late-settings");
        assert!(!fixture.available());
    }

    #[test]
    fn settings_changed_while_prompt_is_open_are_preserved() {
        let fixture = Fixture::new();
        fixture.defaults();
        fixture.source("netplay", "Config/Dolphin.ini", "launcher-settings");
        assert!(fixture.available());
        fixture.write("Config/GCPadNew.ini", "new-local-settings");
        assert!(!fixture.finish(true));
        assert_eq!(
            fs::read_to_string(fixture.user.join("Config/GCPadNew.ini")).unwrap(),
            "new-local-settings"
        );
        assert_eq!(
            fs::read_to_string(fixture.user.join("Config/Dolphin.ini")).unwrap(),
            DEFAULT_DOLPHIN
        );
    }

    #[test]
    fn failed_import_rolls_back_files_already_written() {
        let fixture = Fixture::new();
        fixture.defaults();
        let files = vec![
            ImportFile {
                relative: "Config/Dolphin.ini".into(),
                contents: b"replacement".to_vec(),
            },
            ImportFile {
                relative: "Config/new.ini".into(),
                contents: b"new".to_vec(),
            },
            ImportFile {
                relative: "Config/new.ini/impossible.ini".into(),
                contents: b"failure".to_vec(),
            },
        ];
        assert!(apply_files(&fixture.user, &files).is_err());
        assert_eq!(
            fs::read_to_string(fixture.user.join("Config/Dolphin.ini")).unwrap(),
            DEFAULT_DOLPHIN
        );
        assert!(!fixture.user.join("Config/new.ini").exists());
    }
}
