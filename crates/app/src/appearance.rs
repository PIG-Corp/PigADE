//! Durable, revisioned appearance preferences shared by app windows.

use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

const VERSION: u32 = 1;
const MAX_BYTES: u64 = 4096;
const MAX_REVISION: u64 = i64::MAX as u64;
const LOCK_WAIT: Duration = Duration::from_secs(2);
pub(crate) const MIN_TERMINAL_FONT_SIZE: u8 = 10;
pub(crate) const MAX_TERMINAL_FONT_SIZE: u8 = 24;
pub(crate) const DEFAULT_TERMINAL_FONT_SIZE: u8 = 14;
static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Theme {
    #[default]
    Dark,
    Light,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TerminalTheme {
    #[default]
    Dark,
    Light,
    FollowApp,
}

impl TerminalTheme {
    pub(crate) fn resolved(self, app_theme: Theme) -> Theme {
        match self {
            Self::Dark => Theme::Dark,
            Self::Light => Theme::Light,
            Self::FollowApp => app_theme,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Config {
    pub(crate) version: u32,
    pub(crate) revision: u64,
    pub(crate) theme: Theme,
    pub(crate) reduced_motion: bool,
    #[serde(default = "default_terminal_font_size")]
    pub(crate) terminal_font_size: u8,
    #[serde(default)]
    pub(crate) terminal_theme: TerminalTheme,
}

fn default_terminal_font_size() -> u8 {
    DEFAULT_TERMINAL_FONT_SIZE
}

impl Default for Config {
    fn default() -> Self {
        Self {
            version: VERSION,
            revision: 0,
            theme: Theme::Dark,
            reduced_motion: false,
            terminal_font_size: DEFAULT_TERMINAL_FONT_SIZE,
            terminal_theme: TerminalTheme::Dark,
        }
    }
}

pub(crate) fn load() -> Result<Config, String> {
    load_from(&config_path()?)
}

pub(crate) fn load_from(path: &Path) -> Result<Config, String> {
    read_config(path)
}

pub(crate) fn save(
    expected_revision: u64,
    theme: Theme,
    reduced_motion: bool,
    terminal_font_size: u8,
    terminal_theme: TerminalTheme,
) -> Result<Config, String> {
    save_to(
        &config_path()?,
        expected_revision,
        theme,
        reduced_motion,
        terminal_font_size,
        terminal_theme,
    )
}

pub(crate) fn save_to(
    path: &Path,
    expected_revision: u64,
    theme: Theme,
    reduced_motion: bool,
    terminal_font_size: u8,
    terminal_theme: TerminalTheme,
) -> Result<Config, String> {
    if expected_revision > MAX_REVISION {
        return Err("appearance revision is out of range".into());
    }
    validate_terminal_font_size(terminal_font_size)?;
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut new_directories = Vec::new();
    let mut ancestor = parent;
    while !ancestor.exists() {
        new_directories.push(ancestor.to_owned());
        let Some(next) = ancestor.parent() else {
            break;
        };
        ancestor = next;
    }
    fs::create_dir_all(parent).map_err(|e| format!("create appearance directory: {e}"))?;

    let lock_path = parent.join(".appearance.lock");
    let lock = open_lock(&lock_path)?;
    acquire_lock(&lock)?;

    let current = read_config(path)?;
    if current.revision != expected_revision {
        return Err(format!(
            "appearance revision conflict: expected {expected_revision}, found {}",
            current.revision
        ));
    }
    let revision = expected_revision
        .checked_add(1)
        .filter(|value| *value <= MAX_REVISION)
        .ok_or_else(|| "appearance revision exhausted".to_string())?;
    let next = Config {
        version: VERSION,
        revision,
        theme,
        reduced_motion,
        terminal_font_size,
        terminal_theme,
    };
    let bytes = serde_json::to_vec(&next).map_err(|e| format!("encode appearance config: {e}"))?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err("appearance config exceeds size limit".into());
    }
    atomic_replace(path, parent, &new_directories, &bytes)?;
    Ok(next)
}

fn config_path() -> Result<PathBuf, String> {
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(xdg).join("pigade/appearance.json"));
    }
    let home = std::env::var_os("HOME")
        .filter(|v| !v.is_empty())
        .ok_or_else(|| "neither XDG_CONFIG_HOME nor HOME is set".to_string())?;
    Ok(PathBuf::from(home).join(".config/pigade/appearance.json"))
}

fn read_config(path: &Path) -> Result<Config, String> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Config::default()),
        Err(error) => return Err(format!("inspect appearance config: {error}")),
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err("appearance config must not be a symlink".into());
        }
        Ok(metadata) if !metadata.is_file() => {
            return Err("appearance config is not a regular file".into());
        }
        Ok(metadata) if metadata.len() > MAX_BYTES => {
            return Err("appearance config exceeds 4096 bytes".into());
        }
        Ok(_) => {}
    }
    let file = File::open(path).map_err(|e| format!("open appearance config: {e}"))?;
    let mut bytes = Vec::with_capacity(MAX_BYTES as usize);
    file.take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("read appearance config: {e}"))?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err("appearance config exceeds 4096 bytes".into());
    }
    let config: Config =
        serde_json::from_slice(&bytes).map_err(|e| format!("parse appearance config: {e}"))?;
    if config.version != VERSION {
        return Err(format!(
            "unsupported appearance config version {}",
            config.version
        ));
    }
    if config.revision == 0 || config.revision > MAX_REVISION {
        return Err("appearance config revision must be between 1 and i64::MAX".into());
    }
    validate_terminal_font_size(config.terminal_font_size)?;
    Ok(config)
}

fn validate_terminal_font_size(font_size: u8) -> Result<(), String> {
    if !(MIN_TERMINAL_FONT_SIZE..=MAX_TERMINAL_FONT_SIZE).contains(&font_size) {
        return Err(format!(
            "terminal font size must be between {MIN_TERMINAL_FONT_SIZE} and {MAX_TERMINAL_FONT_SIZE}"
        ));
    }
    Ok(())
}

fn open_lock(path: &Path) -> Result<File, String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err("appearance lock must not be a symlink".into());
        }
        Ok(metadata) if !metadata.is_file() => {
            return Err("appearance lock is not a regular file".into());
        }
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
            return Err(format!("inspect appearance lock: {error}"));
        }
        _ => {}
    }
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
        .open(path)
        .map_err(|e| format!("open appearance lock: {e}"))
}

fn acquire_lock(file: &File) -> Result<(), String> {
    let deadline = Instant::now() + LOCK_WAIT;
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(()),
            Err(TryLockError::WouldBlock) => {
                if Instant::now() >= deadline {
                    return Err("appearance settings are busy; retry shortly".into());
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(TryLockError::Error(error)) => {
                return Err(format!("lock appearance config: {error}"));
            }
        }
    }
}

fn atomic_replace(
    path: &Path,
    parent: &Path,
    new_directories: &[PathBuf],
    bytes: &[u8],
) -> Result<(), String> {
    let (temp_path, mut temp) = create_temp(parent)?;
    let result = (|| {
        temp.write_all(bytes)
            .map_err(|e| format!("write appearance config: {e}"))?;
        temp.sync_all()
            .map_err(|e| format!("sync appearance config: {e}"))?;
        drop(temp);
        fs::rename(&temp_path, path).map_err(|e| format!("replace appearance config: {e}"))?;
        File::open(parent)
            .and_then(|dir| dir.sync_all())
            .map_err(|e| format!("sync appearance directory: {e}"))?;
        for created in new_directories {
            if let Some(parent) = created.parent() {
                File::open(parent)
                    .and_then(|dir| dir.sync_all())
                    .map_err(|e| format!("sync new appearance directory entry: {e}"))?;
            }
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp_path);
    }
    result
}

fn create_temp(parent: &Path) -> Result<(PathBuf, File), String> {
    for _ in 0..32 {
        let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let name = format!(".appearance-{}-{counter}.tmp", std::process::id());
        let path = parent.join(name);
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        match options.open(&path) {
            Ok(file) => return Ok((path, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(format!("create appearance temp file: {error}")),
        }
    }
    Err("could not allocate an appearance temp file".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};

    struct TestDir(PathBuf);
    impl TestDir {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "pigade-appearance-{}-{}",
                std::process::id(),
                TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
        fn config(&self) -> PathBuf {
            self.0.join("appearance.json")
        }
    }
    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn absent_load_and_revision_cas_survive_reopen() {
        let dir = TestDir::new();
        let path = dir.config();
        let absent = load_from(&path).unwrap();
        assert_eq!(absent, Config::default());
        let saved = save_to(&path, 0, Theme::Light, true, 18, TerminalTheme::FollowApp).unwrap();
        assert_eq!(saved.revision, 1);
        assert_eq!(saved.terminal_font_size, 18);
        assert_eq!(saved.terminal_theme, TerminalTheme::FollowApp);
        assert_eq!(load_from(&path).unwrap(), saved);
        assert!(
            save_to(&path, 0, Theme::Dark, false, 12, TerminalTheme::Light)
                .unwrap_err()
                .contains("revision conflict")
        );
        // A stale writer cannot partially change any preference.
        assert_eq!(load_from(&path).unwrap(), saved);
    }

    #[test]
    fn legacy_config_defaults_terminal_font_size() {
        let dir = TestDir::new();
        let path = dir.config();
        fs::write(
            &path,
            br#"{"version":1,"revision":3,"theme":"light","reduced_motion":true}"#,
        )
        .unwrap();
        let config = load_from(&path).unwrap();
        assert_eq!(config.terminal_font_size, DEFAULT_TERMINAL_FONT_SIZE);
        assert_eq!(config.terminal_theme, TerminalTheme::Dark);
        assert_eq!(config.terminal_theme.resolved(Theme::Light), Theme::Dark);
        assert_eq!(config.theme, Theme::Light);
        assert!(config.reduced_motion);
    }

    #[test]
    fn terminal_theme_options_round_trip_and_follow_app_resolves() {
        let dir = TestDir::new();
        let path = dir.config();
        let mut revision = 0;
        for (option, resolved) in [
            (TerminalTheme::Dark, Theme::Dark),
            (TerminalTheme::Light, Theme::Light),
            (TerminalTheme::FollowApp, Theme::Light),
        ] {
            let saved = save_to(&path, revision, Theme::Light, true, 18, option).unwrap();
            revision = saved.revision;
            assert_eq!(saved.terminal_theme, option);
            assert_eq!(load_from(&path).unwrap(), saved);
            assert_eq!(option.resolved(Theme::Light), resolved);
        }
        let before = load_from(&path).unwrap();
        assert!(
            save_to(
                &path,
                revision - 1,
                Theme::Dark,
                false,
                12,
                TerminalTheme::Dark
            )
            .unwrap_err()
            .contains("revision conflict")
        );
        assert_eq!(load_from(&path).unwrap(), before);
    }

    #[test]
    fn malformed_terminal_theme_does_not_save_or_change_file() {
        let dir = TestDir::new();
        let path = dir.config();
        let malformed = br#"{"version":1,"revision":1,"theme":"light","reduced_motion":true,"terminal_theme":"system"}"#;
        fs::write(&path, malformed).unwrap();
        let original = fs::read(&path).unwrap();
        assert!(
            load_from(&path)
                .unwrap_err()
                .contains("parse appearance config")
        );
        assert!(save_to(&path, 1, Theme::Dark, false, 14, TerminalTheme::Dark).is_err());
        assert_eq!(fs::read(&path).unwrap(), original);
    }

    #[test]
    fn rejects_invalid_version_and_oversized_config_without_replacing_file() {
        let dir = TestDir::new();
        let path = dir.config();
        fs::write(
            &path,
            br#"{"version":2,"revision":1,"theme":"dark","reduced_motion":false}"#,
        )
        .unwrap();
        let original = fs::read(&path).unwrap();
        assert!(load_from(&path).unwrap_err().contains("unsupported"));
        assert!(save_to(&path, 0, Theme::Light, false, 14, TerminalTheme::Light).is_err());
        assert_eq!(fs::read(&path).unwrap(), original);
        fs::write(&path, vec![b' '; MAX_BYTES as usize + 1]).unwrap();
        assert!(load_from(&path).unwrap_err().contains("4096"));
    }

    #[test]
    fn invalid_font_sizes_are_rejected_without_changing_existing_config() {
        let dir = TestDir::new();
        let path = dir.config();
        let saved = save_to(&path, 0, Theme::Light, true, 16, TerminalTheme::Dark).unwrap();
        let original = fs::read(&path).unwrap();

        for invalid in [9, 25] {
            assert!(load_font_size_fixture(&dir, invalid).contains("between 10 and 24"));
            assert!(
                save_to(
                    &path,
                    saved.revision,
                    Theme::Dark,
                    false,
                    invalid,
                    TerminalTheme::Light
                )
                .is_err()
            );
            assert_eq!(fs::read(&path).unwrap(), original);
        }
        assert!(load_font_size_fraction_fixture(&dir).contains("parse appearance config"));
        assert_eq!(fs::read(&path).unwrap(), original);
    }

    fn load_font_size_fixture(dir: &TestDir, font_size: u8) -> String {
        let path = dir.0.join(format!("invalid-{font_size}.json"));
        fs::write(
            &path,
            format!(
                r#"{{"version":1,"revision":1,"theme":"dark","reduced_motion":false,"terminal_font_size":{font_size}}}"#
            ),
        )
        .unwrap();
        load_from(&path).unwrap_err()
    }

    fn load_font_size_fraction_fixture(dir: &TestDir) -> String {
        let path = dir.0.join("fractional-font-size.json");
        fs::write(
            &path,
            br#"{"version":1,"revision":1,"theme":"dark","reduced_motion":false,"terminal_font_size":14.5}"#,
        )
        .unwrap();
        load_from(&path).unwrap_err()
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlink_config() {
        use std::os::unix::fs::symlink;
        let dir = TestDir::new();
        let target = dir.0.join("target.json");
        fs::write(
            &target,
            br#"{"version":1,"revision":1,"theme":"dark","reduced_motion":false}"#,
        )
        .unwrap();
        symlink(&target, dir.config()).unwrap();
        assert!(load_from(&dir.config()).unwrap_err().contains("symlink"));
        assert!(
            save_to(
                &dir.config(),
                0,
                Theme::Light,
                false,
                14,
                TerminalTheme::Dark
            )
            .unwrap_err()
            .contains("symlink")
        );
    }

    #[test]
    fn concurrent_same_revision_writes_have_one_winner() {
        let dir = TestDir::new();
        let path = Arc::new(dir.config());
        let barrier = Arc::new(Barrier::new(3));
        let mut threads = Vec::new();
        for theme in [Theme::Dark, Theme::Light] {
            let path = Arc::clone(&path);
            let barrier = Arc::clone(&barrier);
            threads.push(std::thread::spawn(move || {
                barrier.wait();
                save_to(&path, 0, theme, false, 14, TerminalTheme::Dark)
            }));
        }
        barrier.wait();
        let results: Vec<_> = threads
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .collect();
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(results.iter().filter(|result| result.is_err()).count(), 1);
        assert_eq!(load_from(&path).unwrap().revision, 1);
    }
}
