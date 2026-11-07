//! The `ufathom.toml` config file.
//!
//! The file is searched beside the game executable, then beside the DLL; the
//! first one found is the one in effect. A missing file means defaults. A file
//! that cannot be read, parsed or validated is a logged problem and also means
//! defaults: the DLL never fails to start because of its config.
//!
//! # Keys
//!
//! Every key is optional. An unknown key is an error, so a typo is not silent.
//!
//! | Key | Default | Meaning |
//! |---|---|---|
//! | `output` | `ufathom` beside the game executable | Where dumps and the control folder go. A relative path is relative to the config file. |
//! | `formats` | `["json", "usmap"]` | What a dump writes. |
//! | `auto_dump` | `false` | Dump once by itself, `trigger_delay_seconds` after the DLL started. |
//! | `trigger_delay_seconds` | `30` | The delay of `auto_dump`, 0 to 86400. |
//! | `hotkey_dump` | `"F8"` | Starts a dump while a window of the process is in the foreground. `"none"` switches it off. |
//! | `hotkey_unload` | `"F6"` | Unloads the DLL, under the same condition. |
//! | `auto_unload` | `false` | Unload after the automatic dump has ended, whatever its result. |
//! | `threads` | `2` | Threads of the dump pool, 1 to 4. |
//! | `module` | the first `.exe` module | File name of the module that holds the engine globals. |
//! | `pointer_transform` | none | Steps that decode the object array pointer, such as `[{ Xor = 255 }, "Not"]`. |

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use ufathom_model::layout::TransformOp;

use crate::error::ConfigError;
use crate::hotkey::Hotkey;

/// File name of the config file.
pub const CONFIG_FILE_NAME: &str = "ufathom.toml";
/// Name of the folder created beside the game executable when `output` is not set.
pub const DEFAULT_OUTPUT_FOLDER: &str = "ufathom";
/// Fewest threads the dump pool may have.
pub const MIN_THREADS: usize = 1;
/// Most threads the dump pool may have: a dump must not take over the game's CPU.
pub const MAX_THREADS: usize = 4;
/// Threads used when the config does not say.
pub const DEFAULT_THREADS: usize = 2;
/// Seconds `auto_dump` waits after the DLL started, when the config does not say.
/// A game needs time to load its objects before there is anything to dump.
pub const DEFAULT_TRIGGER_DELAY_SECONDS: f64 = 30.0;
/// Longest accepted `trigger_delay_seconds`: one day.
pub const MAX_TRIGGER_DELAY_SECONDS: f64 = 86_400.0;
/// Hotkey that starts a dump, when the config does not say.
pub const DEFAULT_HOTKEY_DUMP: &str = "F8";
/// Hotkey that unloads the DLL, when the config does not say.
pub const DEFAULT_HOTKEY_UNLOAD: &str = "F6";

/// An output format of a dump.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    /// `dump.json`, the full dump.
    Json,
    /// `Mappings.usmap`, the mappings file other Unreal tools read.
    Usmap,
}

/// The settings in effect. Build one with [`Config::load`] or
/// [`Config::defaults`].
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Config {
    /// Where dumps and the control folder go. Always absolute or relative to
    /// the working directory as the user wrote it; never empty.
    pub output: PathBuf,
    /// Formats written by a dump that does not name its own.
    pub formats: Vec<Format>,
    /// Dump once by itself, [`trigger_delay_seconds`](Self::trigger_delay_seconds)
    /// after the DLL started.
    pub auto_dump: bool,
    /// Seconds between the start of the DLL and the automatic dump.
    pub trigger_delay_seconds: f64,
    /// Key that starts a dump while a window of the process is in the
    /// foreground. `None` switches it off.
    pub hotkey_dump: Option<Hotkey>,
    /// Key that unloads the DLL, under the same condition.
    pub hotkey_unload: Option<Hotkey>,
    /// Unload after the automatic dump has finished, whatever its result.
    pub auto_unload: bool,
    /// Threads of the dump pool, from [`MIN_THREADS`] to [`MAX_THREADS`].
    pub threads: usize,
    /// File name of the module that holds the engine globals, as for
    /// [`DiscoverOptions::module`](ufathom_scan::DiscoverOptions::module).
    pub module: Option<String>,
    /// Steps that decode the object array pointer, stated by the user.
    pub pointer_transform: Vec<TransformOp>,
}

/// Where the settings in effect came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigSource {
    /// This file.
    File(PathBuf),
    /// No file was found, or the file found was unusable.
    Defaults,
}

/// The result of looking for a config file.
#[derive(Debug, Clone)]
pub struct LoadedConfig {
    /// The settings in effect.
    pub config: Config,
    /// Where they came from.
    pub source: ConfigSource,
    /// The reason a file was not used, if one was found and refused.
    pub problem: Option<String>,
}

/// The file as written: every key optional, unknown keys refused.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    output: Option<PathBuf>,
    formats: Option<Vec<Format>>,
    auto_dump: Option<bool>,
    trigger_delay_seconds: Option<f64>,
    hotkey_dump: Option<String>,
    hotkey_unload: Option<String>,
    auto_unload: Option<bool>,
    threads: Option<i64>,
    module: Option<String>,
    pointer_transform: Option<Vec<TransformOp>>,
}

impl Config {
    /// The settings used when no file applies. `exe_dir` is the folder of the
    /// game executable.
    #[must_use]
    pub fn defaults(exe_dir: &Path) -> Self {
        Self {
            output: exe_dir.join(DEFAULT_OUTPUT_FOLDER),
            formats: vec![Format::Json, Format::Usmap],
            auto_dump: false,
            trigger_delay_seconds: DEFAULT_TRIGGER_DELAY_SECONDS,
            hotkey_dump: Hotkey::parse(DEFAULT_HOTKEY_DUMP).ok().flatten(),
            hotkey_unload: Hotkey::parse(DEFAULT_HOTKEY_UNLOAD).ok().flatten(),
            auto_unload: false,
            threads: DEFAULT_THREADS,
            module: None,
            pointer_transform: Vec::new(),
        }
    }

    /// Looks for `ufathom.toml` in each of `search_dirs` in order, normally
    /// the game executable's folder and then the DLL's, and uses the first
    /// one found. Never fails: a problem with the file is returned in
    /// [`LoadedConfig::problem`] and the defaults are used.
    #[must_use]
    pub fn load(search_dirs: &[&Path], exe_dir: &Path) -> LoadedConfig {
        for dir in search_dirs {
            let path = dir.join(CONFIG_FILE_NAME);
            if !path.is_file() {
                continue;
            }
            return match Self::from_file(&path, exe_dir) {
                Ok(config) => LoadedConfig {
                    config,
                    source: ConfigSource::File(path),
                    problem: None,
                },
                Err(error) => LoadedConfig {
                    config: Self::defaults(exe_dir),
                    source: ConfigSource::Defaults,
                    problem: Some(error.to_string()),
                },
            };
        }
        LoadedConfig {
            config: Self::defaults(exe_dir),
            source: ConfigSource::Defaults,
            problem: None,
        }
    }

    /// Reads and validates one config file. A relative `output` is taken
    /// relative to the folder of the file.
    ///
    /// # Errors
    ///
    /// [`ConfigError`] when the file cannot be read or parsed, or a value is
    /// out of range.
    pub fn from_file(path: &Path, exe_dir: &Path) -> Result<Self, ConfigError> {
        let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Read {
            path: path.to_owned(),
            source,
        })?;
        let base = path.parent().unwrap_or(exe_dir);
        Self::from_toml(&text, path, base, exe_dir)
    }

    /// Parses and validates config text. `path` names the file in errors,
    /// `base` is what a relative `output` is joined to, and `exe_dir` is the
    /// folder of the default output.
    ///
    /// # Errors
    ///
    /// [`ConfigError::Parse`] for text that is not a valid config,
    /// [`ConfigError::Invalid`] for a value that is out of range.
    pub fn from_toml(
        text: &str,
        path: &Path,
        base: &Path,
        exe_dir: &Path,
    ) -> Result<Self, ConfigError> {
        let raw: RawConfig = toml::from_str(text).map_err(|error| ConfigError::Parse {
            path: path.to_owned(),
            message: error.to_string(),
        })?;
        let invalid = |key: &'static str, message: String| ConfigError::Invalid {
            path: path.to_owned(),
            key,
            message,
        };
        let mut config = Self::defaults(exe_dir);

        if let Some(output) = raw.output {
            if output.as_os_str().is_empty() {
                return Err(invalid("output", "must not be empty".to_owned()));
            }
            config.output = if output.is_absolute() {
                output
            } else {
                base.join(output)
            };
        }
        if let Some(mut formats) = raw.formats {
            formats.sort();
            formats.dedup();
            if formats.is_empty() {
                return Err(invalid(
                    "formats",
                    "must name at least one of json, usmap".to_owned(),
                ));
            }
            config.formats = formats;
        }
        if let Some(value) = raw.auto_dump {
            config.auto_dump = value;
        }
        if let Some(seconds) = raw.trigger_delay_seconds {
            if !seconds.is_finite() || !(0.0..=MAX_TRIGGER_DELAY_SECONDS).contains(&seconds) {
                return Err(invalid(
                    "trigger_delay_seconds",
                    format!("must be from 0 to {MAX_TRIGGER_DELAY_SECONDS}, got {seconds}"),
                ));
            }
            config.trigger_delay_seconds = seconds;
        }
        if let Some(name) = raw.hotkey_dump {
            config.hotkey_dump =
                Hotkey::parse(&name).map_err(|error| invalid("hotkey_dump", error.to_string()))?;
        }
        if let Some(name) = raw.hotkey_unload {
            config.hotkey_unload = Hotkey::parse(&name)
                .map_err(|error| invalid("hotkey_unload", error.to_string()))?;
        }
        if let Some(value) = raw.auto_unload {
            config.auto_unload = value;
        }
        if let Some(threads) = raw.threads {
            config.threads = usize::try_from(threads)
                .ok()
                .filter(|count| (MIN_THREADS..=MAX_THREADS).contains(count))
                .ok_or_else(|| {
                    invalid(
                        "threads",
                        format!("must be from {MIN_THREADS} to {MAX_THREADS}, got {threads}"),
                    )
                })?;
        }
        if let Some(module) = raw.module {
            let module = module.trim().to_owned();
            if module.is_empty() {
                return Err(invalid("module", "must not be empty".to_owned()));
            }
            config.module = Some(module);
        }
        if let Some(steps) = raw.pointer_transform {
            config.pointer_transform = steps;
        }
        Ok(config)
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test code may panic on failure"
)]
mod tests {
    use super::*;

    fn exe_dir() -> PathBuf {
        PathBuf::from("game")
    }

    fn parse(text: &str) -> Result<Config, ConfigError> {
        Config::from_toml(
            text,
            Path::new("ufathom.toml"),
            Path::new("cfgdir"),
            &exe_dir(),
        )
    }

    #[test]
    fn an_empty_file_gives_the_defaults() {
        let config = parse("").unwrap();
        assert_eq!(config, Config::defaults(&exe_dir()));
        assert_eq!(config.output, exe_dir().join("ufathom"));
        assert_eq!(config.formats, vec![Format::Json, Format::Usmap]);
        assert!(!config.auto_dump, "auto dump must be off by default");
        assert!(!config.auto_unload);
        assert_eq!(config.threads, DEFAULT_THREADS);
        assert_eq!(config.hotkey_dump.unwrap().to_string(), "F8");
        assert_eq!(config.hotkey_unload.unwrap().to_string(), "F6");
        assert!(config.module.is_none());
        assert!(config.pointer_transform.is_empty());
    }

    #[test]
    fn every_key_is_read() {
        let config = parse(
            r#"
            output = "dumps"
            formats = ["usmap"]
            auto_dump = true
            trigger_delay_seconds = 2.5
            hotkey_dump = "F9"
            hotkey_unload = "none"
            auto_unload = true
            threads = 4
            module = "Game.exe"
            pointer_transform = [{ Xor = 255 }, "Not", { RotateLeft = 3 }]
            "#,
        )
        .unwrap();
        assert_eq!(config.output, Path::new("cfgdir").join("dumps"));
        assert_eq!(config.formats, vec![Format::Usmap]);
        assert!(config.auto_dump);
        assert!((config.trigger_delay_seconds - 2.5).abs() < f64::EPSILON);
        assert_eq!(config.hotkey_dump.unwrap().to_string(), "F9");
        assert!(config.hotkey_unload.is_none());
        assert!(config.auto_unload);
        assert_eq!(config.threads, 4);
        assert_eq!(config.module.as_deref(), Some("Game.exe"));
        assert_eq!(
            config.pointer_transform,
            vec![
                TransformOp::Xor(255),
                TransformOp::Not,
                TransformOp::RotateLeft(3)
            ]
        );
    }

    #[test]
    fn an_absolute_output_is_kept() {
        let absolute = std::env::temp_dir().join("ufathom-out");
        let text = format!("output = '{}'", absolute.display());
        assert_eq!(parse(&text).unwrap().output, absolute);
    }

    #[test]
    fn formats_are_sorted_and_deduplicated() {
        let config = parse(r#"formats = ["usmap", "json", "usmap"]"#).unwrap();
        assert_eq!(config.formats, vec![Format::Json, Format::Usmap]);
    }

    #[test]
    fn broken_toml_is_a_parse_error() {
        assert!(matches!(
            parse("threads = = 2"),
            Err(ConfigError::Parse { .. })
        ));
    }

    #[test]
    fn an_unknown_key_is_refused_so_a_typo_is_not_silent() {
        let error = parse("auto_dmup = true").unwrap_err();
        assert!(matches!(error, ConfigError::Parse { .. }), "{error}");
        assert!(error.to_string().contains("auto_dmup"), "{error}");
    }

    #[test]
    fn a_value_of_the_wrong_type_is_a_parse_error() {
        assert!(matches!(
            parse(r#"auto_dump = "yes""#),
            Err(ConfigError::Parse { .. })
        ));
        assert!(matches!(
            parse(r#"formats = ["xml"]"#),
            Err(ConfigError::Parse { .. })
        ));
    }

    #[test]
    fn threads_outside_one_to_four_are_refused() {
        for bad in ["0", "5", "-1", "1000000000000"] {
            let error = parse(&format!("threads = {bad}")).unwrap_err();
            assert!(
                matches!(error, ConfigError::Invalid { key: "threads", .. }),
                "{bad}: {error}"
            );
        }
        for good in 1..=4 {
            assert_eq!(parse(&format!("threads = {good}")).unwrap().threads, good);
        }
    }

    #[test]
    fn a_bad_delay_is_refused() {
        for bad in ["-1.0", "100000.0", "nan", "inf"] {
            let error = parse(&format!("trigger_delay_seconds = {bad}")).unwrap_err();
            assert!(
                matches!(
                    error,
                    ConfigError::Invalid {
                        key: "trigger_delay_seconds",
                        ..
                    }
                ),
                "{bad}: {error}"
            );
        }
        assert!(parse("trigger_delay_seconds = 0").is_ok());
    }

    #[test]
    fn a_bad_hotkey_is_refused_with_the_key_named() {
        let error = parse(r#"hotkey_dump = "Ctrl+F8""#).unwrap_err();
        assert!(
            matches!(
                error,
                ConfigError::Invalid {
                    key: "hotkey_dump",
                    ..
                }
            ),
            "{error}"
        );
    }

    #[test]
    fn empty_formats_module_and_output_are_refused() {
        for (text, key) in [
            ("formats = []", "formats"),
            (r#"module = "  ""#, "module"),
            (r#"output = """#, "output"),
        ] {
            let error = parse(text).unwrap_err();
            assert!(
                matches!(&error, ConfigError::Invalid { key: found, .. } if *found == key),
                "{text}: {error}"
            );
        }
    }

    #[test]
    fn the_file_beside_the_game_wins_over_the_one_beside_the_dll() {
        let game = tempfile::tempdir().unwrap();
        let dll = tempfile::tempdir().unwrap();
        std::fs::write(game.path().join(CONFIG_FILE_NAME), "threads = 3").unwrap();
        std::fs::write(dll.path().join(CONFIG_FILE_NAME), "threads = 1").unwrap();
        let loaded = Config::load(&[game.path(), dll.path()], game.path());
        assert_eq!(loaded.config.threads, 3);
        assert_eq!(
            loaded.source,
            ConfigSource::File(game.path().join(CONFIG_FILE_NAME))
        );
        assert!(loaded.problem.is_none());
    }

    #[test]
    fn the_file_beside_the_dll_is_used_when_there_is_none_beside_the_game() {
        let game = tempfile::tempdir().unwrap();
        let dll = tempfile::tempdir().unwrap();
        std::fs::write(dll.path().join(CONFIG_FILE_NAME), "threads = 1").unwrap();
        let loaded = Config::load(&[game.path(), dll.path()], game.path());
        assert_eq!(loaded.config.threads, 1);
    }

    #[test]
    fn no_file_means_defaults_without_a_problem() {
        let game = tempfile::tempdir().unwrap();
        let loaded = Config::load(&[game.path()], game.path());
        assert_eq!(loaded.config, Config::defaults(game.path()));
        assert_eq!(loaded.source, ConfigSource::Defaults);
        assert!(loaded.problem.is_none());
    }

    #[test]
    fn an_invalid_file_gives_defaults_and_a_problem_and_does_not_fall_through() {
        let game = tempfile::tempdir().unwrap();
        let dll = tempfile::tempdir().unwrap();
        std::fs::write(game.path().join(CONFIG_FILE_NAME), "threads = 99").unwrap();
        std::fs::write(dll.path().join(CONFIG_FILE_NAME), "threads = 1").unwrap();
        let loaded = Config::load(&[game.path(), dll.path()], game.path());
        assert_eq!(loaded.config, Config::defaults(game.path()));
        assert_eq!(loaded.source, ConfigSource::Defaults);
        let problem = loaded.problem.unwrap();
        assert!(problem.contains("threads"), "{problem}");
    }

    #[test]
    fn a_file_that_is_not_text_gives_defaults_and_a_problem() {
        let game = tempfile::tempdir().unwrap();
        std::fs::write(game.path().join(CONFIG_FILE_NAME), [0xFF, 0xFE, 0x00, 0x80]).unwrap();
        let loaded = Config::load(&[game.path()], game.path());
        assert_eq!(loaded.config, Config::defaults(game.path()));
        assert!(loaded.problem.is_some());
    }

    #[test]
    fn a_relative_output_follows_the_folder_of_the_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(CONFIG_FILE_NAME), r#"output = "out""#).unwrap();
        let loaded = Config::load(&[dir.path()], Path::new("elsewhere"));
        assert_eq!(loaded.config.output, dir.path().join("out"));
    }
}
