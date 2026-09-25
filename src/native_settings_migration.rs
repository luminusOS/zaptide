//! One-way mirror of allowlisted JSON preferences into GSettings.
//!
//! The JSON settings file stays authoritative: migration is additive, so the
//! native shell keeps reading [`crate::settings::Settings`] exactly as before.
//! Only keys declared in `data/dev.luminusos.ZapTide.gschema.xml` are copied,
//! only from ZapTide's own settings file (never from ZapFast or FastWhatsApp
//! paths), and the JSON file is never modified or deleted so it remains usable
//! as rollback input.
//!
//! Completion is recorded with an atomic marker file next to the settings
//! file. A missing settings file leaves no marker, so a file that appears
//! later (a restored backup, a fresh account) is still migrated on a later
//! start; the check costs one `stat` per launch.

use gtk4::gio;
use gtk4::gio::prelude::SettingsExt;
use gtk4::glib;
use serde_json::Value;

use crate::paths::AppDirs;

/// GSettings schema shipped with the native shell.
const SCHEMA_ID: &str = "dev.luminusos.ZapTide";
/// Highest JSON settings `version` this migration understands. Files written
/// by a newer ZapTide are refused rather than partially copied.
const SUPPORTED_VERSION: u64 = 1;
/// Completion marker written atomically after a successful migration.
const MARKER_NAME: &str = "gsettings-migrated-v1";

/// Result of one migration attempt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MigrationOutcome {
    /// No settings file to read; nothing was written and no marker exists.
    NothingToMigrate,
    /// The marker already existed; this run changed nothing.
    AlreadyMigrated,
    /// Allowlisted keys present in the JSON file were copied and the marker
    /// was written. Absent keys keep their schema defaults.
    Migrated { keys: usize },
    /// The file is not valid JSON or its `version` is not an integer. The
    /// file is left intact as rollback input and no marker was written.
    Malformed,
    /// The file declares a `version` newer than `SUPPORTED_VERSION`; no
    /// keys were copied and no marker was written.
    NewerSchema,
    /// The compiled GSettings schema is not installed on this system.
    SchemaUnavailable,
    /// A GSettings write failed; no marker was written, so the next start
    /// retries.
    WriteFailed,
}

#[derive(Clone, Copy)]
enum KeyKind {
    Flag,
    Number,
    ThemeChoice,
}

/// Allowlist: every key of the GSettings schema, mapped to its JSON field.
/// Keys outside this table (secrets, caches, window state) are never read.
const ALLOWLIST: [(&str, &str, KeyKind); 16] = [
    ("theme", "theme-choice", KeyKind::ThemeChoice),
    ("zoom", "zoom", KeyKind::Number),
    ("sidebar_width", "sidebar-width", KeyKind::Number),
    ("voice_speed", "voice-speed", KeyKind::Number),
    ("enter_sends", "enter-sends", KeyKind::Flag),
    ("send_read_receipts", "send-read-receipts", KeyKind::Flag),
    ("send_typing", "send-typing", KeyKind::Flag),
    ("auto_download", "auto-download", KeyKind::Flag),
    (
        "show_sender_pictures",
        "show-sender-pictures",
        KeyKind::Flag,
    ),
    ("show_shortcut_hints", "show-shortcut-hints", KeyKind::Flag),
    ("notifications", "notifications", KeyKind::Flag),
    (
        "keep_running_in_background",
        "keep-running-in-background",
        KeyKind::Flag,
    ),
    ("check_for_updates", "check-for-updates", KeyKind::Flag),
    (
        "download_updates_automatically",
        "download-updates-automatically",
        KeyKind::Flag,
    ),
    ("names_from_contacts", "names-from-contacts", KeyKind::Flag),
    (
        "save_contacts_to_phone",
        "save-contacts-to-phone",
        KeyKind::Flag,
    ),
];

/// The only theme values the schema's `choices` accepts.
const THEME_CHOICES: [&str; 3] = ["system", "light", "dark"];

/// Mirrors allowlisted preferences from the JSON settings file into the
/// system GSettings database. Safe to call on every start; idempotent once
/// the marker exists.
pub fn migrate_json_to_gsettings(dirs: &AppDirs) -> MigrationOutcome {
    if marker_path(dirs).exists() {
        return MigrationOutcome::AlreadyMigrated;
    }
    let Some(schema) =
        gio::SettingsSchemaSource::default().and_then(|source| source.lookup(SCHEMA_ID, true))
    else {
        log::error!("GSettings schema {SCHEMA_ID} is not installed; skipping migration");
        return MigrationOutcome::SchemaUnavailable;
    };
    let settings = gio::Settings::new_full(&schema, None::<&gio::SettingsBackend>, None);
    migrate_into(&settings, dirs)
}

fn migrate_into(settings: &gio::Settings, dirs: &AppDirs) -> MigrationOutcome {
    if marker_path(dirs).exists() {
        return MigrationOutcome::AlreadyMigrated;
    }
    let contents = match std::fs::read(dirs.settings_file()) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return MigrationOutcome::NothingToMigrate;
        }
        Err(error) => {
            log::warn!("could not read the settings file for migration: {error}");
            return MigrationOutcome::NothingToMigrate;
        }
    };
    let Ok(value) = serde_json::from_slice::<Value>(&contents) else {
        // Never log the contents: the file can hold the GIPHY key.
        log::warn!("settings file is malformed; kept untouched as rollback input");
        return MigrationOutcome::Malformed;
    };
    let Some(json) = value.as_object() else {
        log::warn!("settings file is not a JSON object; kept as rollback input");
        return MigrationOutcome::Malformed;
    };
    if let Some(version) = json.get("version") {
        match version.as_u64() {
            Some(version) if version > SUPPORTED_VERSION => {
                log::info!(
                    "settings file version {version} is newer than supported; not migrating"
                );
                return MigrationOutcome::NewerSchema;
            }
            Some(_) => {}
            None => {
                log::warn!("settings file version is not an integer; kept as rollback input");
                return MigrationOutcome::Malformed;
            }
        }
    }
    let keys = match write_allowlisted(json, settings) {
        Ok(keys) => keys,
        Err(error) => {
            log::error!("GSettings write failed during migration: {error}");
            return MigrationOutcome::WriteFailed;
        }
    };
    if let Err(error) = write_marker(dirs) {
        // Values are already in place and the rerun is idempotent, so this
        // only costs one extra pass on the next start.
        log::warn!("migration marker could not be written: {error}");
    }
    log::info!("migrated {keys} allowlisted settings into GSettings");
    MigrationOutcome::Migrated { keys }
}

/// Copies each allowlisted key that is present in the JSON object. Keys with
/// an absent or wrongly typed value are skipped and keep their schema
/// default. Returns the number of keys written.
fn write_allowlisted(
    json: &serde_json::Map<String, Value>,
    settings: &gio::Settings,
) -> Result<usize, glib::BoolError> {
    let mut written = 0;
    for (json_key, schema_key, kind) in ALLOWLIST {
        let Some(value) = json.get(json_key) else {
            continue;
        };
        let result = match (kind, value) {
            (KeyKind::Flag, Value::Bool(flag)) => settings.set_boolean(schema_key, *flag),
            (KeyKind::Number, Value::Number(number)) => match number.as_f64() {
                Some(number) => settings.set_double(schema_key, number),
                None => continue,
            },
            (KeyKind::ThemeChoice, Value::String(choice)) => {
                if THEME_CHOICES.contains(&choice.as_str()) {
                    settings.set_string(schema_key, choice)
                } else {
                    log::debug!("skipping unknown theme choice in migration");
                    continue;
                }
            }
            _ => {
                log::debug!("skipping {json_key} in migration: unexpected JSON type");
                continue;
            }
        };
        result?;
        written += 1;
    }
    Ok(written)
}

fn marker_path(dirs: &AppDirs) -> std::path::PathBuf {
    dirs.config.join(MARKER_NAME)
}

/// Writes the completion marker through a temporary file plus rename, so a
/// crash mid-write leaves either no marker or a complete one.
fn write_marker(dirs: &AppDirs) -> std::io::Result<()> {
    let path = marker_path(dirs);
    let temp = path.with_extension("tmp");
    std::fs::write(&temp, format!("settings-version {SUPPORTED_VERSION}\n"))?;
    std::fs::rename(&temp, &path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gtk4::gio::prelude::SettingsExt;
    use std::path::Path;

    /// The memory backend keeps tests off dconf. It is a process-wide
    /// singleton created on first use, so the variable is set before any
    /// GSettings object exists and each test writes to its own schema path.
    fn memory_settings(schema_dir: &Path, case: &str) -> gio::Settings {
        static ENV_ONCE: std::sync::Once = std::sync::Once::new();
        static CASE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        ENV_ONCE.call_once(|| {
            // SAFETY: set before any GSettings backend is created, and no
            // other test in this crate reads this variable.
            unsafe { std::env::set_var("GSETTINGS_BACKEND", "memory") };
        });
        let source = gio::SettingsSchemaSource::from_directory(schema_dir, None, false)
            .expect("compiled schema directory");
        let schema = source
            .lookup(SCHEMA_ID, false)
            .expect("schema in directory");
        let id = CASE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = format!("/dev/luminusos/zaptide/tests/{case}-{id}/");
        gio::Settings::new_full(&schema, None::<&gio::SettingsBackend>, Some(&path))
    }

    /// Compiles the shipped schema into a fresh temporary directory with its
    /// fixed `path` removed, so each test can bind the same keys to an
    /// isolated memory-backend path. Keys, types, defaults, and choices are
    /// untouched.
    fn compile_schema() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let xml = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/data/dev.luminusos.ZapTide.gschema.xml"
        ))
        .unwrap();
        let relocatable = xml.replace(r#" path="/dev/luminusos/zaptide/""#, "");
        assert!(relocatable.contains("<schema id=\"dev.luminusos.ZapTide\">"));
        std::fs::write(
            dir.path().join("dev.luminusos.ZapTide.gschema.xml"),
            relocatable,
        )
        .unwrap();
        let status = std::process::Command::new("glib-compile-schemas")
            .arg(dir.path())
            .status()
            .expect("glib-compile-schemas is installed");
        assert!(status.success(), "schema compiles");
        dir
    }

    fn dirs_with_settings(root: &Path, contents: Option<&str>) -> AppDirs {
        let dirs = AppDirs::under(root);
        std::fs::create_dir_all(&dirs.config).unwrap();
        if let Some(contents) = contents {
            std::fs::write(dirs.settings_file(), contents).unwrap();
        }
        dirs
    }

    #[test]
    fn every_allowlisted_key_migrates_and_the_marker_is_written() {
        let schema_dir = compile_schema();
        let settings = memory_settings(schema_dir.path(), "full");
        let root = tempfile::tempdir().unwrap();
        let json = r#"{
            "theme": "light",
            "zoom": 1.25,
            "sidebar_width": 350.0,
            "voice_speed": 1.5,
            "enter_sends": false,
            "send_read_receipts": false,
            "send_typing": false,
            "auto_download": false,
            "show_sender_pictures": true,
            "show_shortcut_hints": false,
            "notifications": false,
            "keep_running_in_background": false,
            "check_for_updates": false,
            "download_updates_automatically": true,
            "names_from_contacts": false,
            "save_contacts_to_phone": false,
            "giphy_key": "secret-key",
            "chat_lock_code_hash": "salt$hash",
            "last_chat": "someone@s.whatsapp.net"
        }"#;
        let dirs = dirs_with_settings(root.path(), Some(json));
        let before = std::fs::read(dirs.settings_file()).unwrap();

        assert_eq!(
            migrate_into(&settings, &dirs),
            MigrationOutcome::Migrated { keys: 16 }
        );
        assert_eq!(settings.string("theme-choice"), "light");
        assert_eq!(settings.double("zoom"), 1.25);
        assert_eq!(settings.double("sidebar-width"), 350.0);
        assert_eq!(settings.double("voice-speed"), 1.5);
        for key in [
            "enter-sends",
            "send-read-receipts",
            "send-typing",
            "auto-download",
            "show-shortcut-hints",
            "notifications",
            "keep-running-in-background",
            "check-for-updates",
            "names-from-contacts",
            "save-contacts-to-phone",
        ] {
            assert!(!settings.boolean(key), "{key} mirrors the JSON value");
        }
        for key in ["show-sender-pictures", "download-updates-automatically"] {
            assert!(settings.boolean(key), "{key} mirrors the JSON value");
        }
        // Rollback input: the JSON file is byte-identical after migration.
        assert_eq!(std::fs::read(dirs.settings_file()).unwrap(), before);
        assert!(marker_path(&dirs).exists());
        assert!(!marker_path(&dirs).with_extension("tmp").exists());
    }

    #[test]
    fn malformed_json_keeps_the_rollback_input_and_writes_no_marker() {
        let schema_dir = compile_schema();
        let settings = memory_settings(schema_dir.path(), "malformed");
        let root = tempfile::tempdir().unwrap();
        let json = r#"{"theme": "light", broken"#;
        let dirs = dirs_with_settings(root.path(), Some(json));

        assert_eq!(migrate_into(&settings, &dirs), MigrationOutcome::Malformed);
        assert_eq!(std::fs::read_to_string(dirs.settings_file()).unwrap(), json);
        assert!(!marker_path(&dirs).exists());
        assert_eq!(settings.string("theme-choice"), "dark");
    }

    #[test]
    fn a_json_array_instead_of_an_object_is_malformed() {
        let schema_dir = compile_schema();
        let settings = memory_settings(schema_dir.path(), "array");
        let root = tempfile::tempdir().unwrap();
        let dirs = dirs_with_settings(root.path(), Some("[1, 2]"));

        assert_eq!(migrate_into(&settings, &dirs), MigrationOutcome::Malformed);
        assert!(!marker_path(&dirs).exists());
    }

    #[test]
    fn partial_json_migrates_present_keys_and_leaves_absent_ones_defaulted() {
        let schema_dir = compile_schema();
        let settings = memory_settings(schema_dir.path(), "partial");
        let root = tempfile::tempdir().unwrap();
        let dirs = dirs_with_settings(
            root.path(),
            Some(r#"{"theme": "system", "enter_sends": false}"#),
        );

        assert_eq!(
            migrate_into(&settings, &dirs),
            MigrationOutcome::Migrated { keys: 2 }
        );
        assert_eq!(settings.string("theme-choice"), "system");
        assert!(!settings.boolean("enter-sends"));
        assert_eq!(settings.double("zoom"), 1.0);
        assert!(settings.boolean("notifications"));
        assert!(marker_path(&dirs).exists());
    }

    #[test]
    fn a_missing_settings_file_migrates_nothing_and_leaves_no_marker() {
        let schema_dir = compile_schema();
        let settings = memory_settings(schema_dir.path(), "missing");
        let root = tempfile::tempdir().unwrap();
        let dirs = dirs_with_settings(root.path(), None);

        assert_eq!(
            migrate_into(&settings, &dirs),
            MigrationOutcome::NothingToMigrate
        );
        assert!(!marker_path(&dirs).exists());
    }

    #[test]
    fn an_interrupted_migration_reruns_to_the_same_values_and_then_marks() {
        let schema_dir = compile_schema();
        let settings = memory_settings(schema_dir.path(), "interrupted");
        let root = tempfile::tempdir().unwrap();
        let json = r#"{"zoom": 1.5, "send_typing": false}"#;
        let dirs = dirs_with_settings(root.path(), Some(json));

        assert_eq!(
            migrate_into(&settings, &dirs),
            MigrationOutcome::Migrated { keys: 2 }
        );
        // Simulate a crash after some keys were written but before the
        // marker landed: the rerun repeats the same writes idempotently.
        std::fs::remove_file(marker_path(&dirs)).unwrap();
        assert_eq!(settings.double("zoom"), 1.5);
        assert_eq!(
            migrate_into(&settings, &dirs),
            MigrationOutcome::Migrated { keys: 2 }
        );
        assert_eq!(settings.double("zoom"), 1.5);
        assert!(!settings.boolean("send-typing"));
        assert!(marker_path(&dirs).exists());
    }

    #[test]
    fn repeated_migration_is_a_noop_that_does_not_rewrite_values() {
        let schema_dir = compile_schema();
        let settings = memory_settings(schema_dir.path(), "repeated");
        let root = tempfile::tempdir().unwrap();
        let dirs = dirs_with_settings(root.path(), Some(r#"{"zoom": 1.75}"#));
        assert_eq!(
            migrate_into(&settings, &dirs),
            MigrationOutcome::Migrated { keys: 1 }
        );

        // A later edit of the JSON file must not reach GSettings once the
        // marker exists: the migration is a one-time mirror.
        std::fs::write(dirs.settings_file(), r#"{"zoom": 0.8}"#).unwrap();
        assert_eq!(
            migrate_into(&settings, &dirs),
            MigrationOutcome::AlreadyMigrated
        );
        assert_eq!(settings.double("zoom"), 1.75);
    }

    #[test]
    fn a_newer_settings_version_is_refused_without_writing_anything() {
        let schema_dir = compile_schema();
        let settings = memory_settings(schema_dir.path(), "newer");
        let root = tempfile::tempdir().unwrap();
        let dirs = dirs_with_settings(
            root.path(),
            Some(r#"{"version": 2, "zoom": 1.5, "enter_sends": false}"#),
        );

        assert_eq!(
            migrate_into(&settings, &dirs),
            MigrationOutcome::NewerSchema
        );
        assert!(!marker_path(&dirs).exists());
        assert_eq!(settings.double("zoom"), 1.0);
        assert!(settings.boolean("enter-sends"));
    }

    #[test]
    fn a_supported_version_field_does_not_block_migration() {
        let schema_dir = compile_schema();
        let settings = memory_settings(schema_dir.path(), "versioned");
        let root = tempfile::tempdir().unwrap();
        let dirs = dirs_with_settings(root.path(), Some(r#"{"version": 1, "zoom": 1.5}"#));

        assert_eq!(
            migrate_into(&settings, &dirs),
            MigrationOutcome::Migrated { keys: 1 }
        );
        assert_eq!(settings.double("zoom"), 1.5);
    }

    #[test]
    fn unknown_keys_and_wrongly_typed_values_are_skipped() {
        let schema_dir = compile_schema();
        let settings = memory_settings(schema_dir.path(), "skipped");
        let root = tempfile::tempdir().unwrap();
        let json = r#"{
            "theme": "neon",
            "zoom": "wide",
            "enter_sends": 1,
            "giphy_key": "secret-key",
            "last_chat": "someone@s.whatsapp.net",
            "recent_emoji": ["x"]
        }"#;
        let dirs = dirs_with_settings(root.path(), Some(json));

        assert_eq!(
            migrate_into(&settings, &dirs),
            MigrationOutcome::Migrated { keys: 0 }
        );
        assert_eq!(settings.string("theme-choice"), "dark");
        assert_eq!(settings.double("zoom"), 1.0);
        assert!(settings.boolean("enter-sends"));
        // The file with its secrets is untouched rollback input.
        assert_eq!(std::fs::read_to_string(dirs.settings_file()).unwrap(), json);
        assert!(marker_path(&dirs).exists());
    }

    #[test]
    fn migration_only_ever_reads_the_zaptide_settings_file() {
        let schema_dir = compile_schema();
        let settings = memory_settings(schema_dir.path(), "isolated");
        let root = tempfile::tempdir().unwrap();
        // A neighbouring ZapFast-style directory with its own settings must
        // not be inspected, adopted, or modified.
        let zapfast_config = root.path().join("zapfast-config");
        std::fs::create_dir_all(&zapfast_config).unwrap();
        let zapfast_settings = zapfast_config.join("settings.json");
        std::fs::write(&zapfast_settings, r#"{"zoom": 2.0}"#).unwrap();
        let dirs = dirs_with_settings(root.path().join("zaptide").as_path(), None);

        assert_eq!(
            migrate_into(&settings, &dirs),
            MigrationOutcome::NothingToMigrate
        );
        assert_eq!(
            std::fs::read_to_string(&zapfast_settings).unwrap(),
            r#"{"zoom": 2.0}"#
        );
        assert_eq!(settings.double("zoom"), 1.0);
    }

    #[test]
    fn the_marker_is_written_atomically_through_a_temporary_file() {
        let root = tempfile::tempdir().unwrap();
        let dirs = dirs_with_settings(root.path(), None);
        write_marker(&dirs).unwrap();
        let contents = std::fs::read_to_string(marker_path(&dirs)).unwrap();
        assert!(contents.contains("settings-version 1"));
        assert!(!marker_path(&dirs).with_extension("tmp").exists());
        // A second write replaces the marker without leaving temporaries.
        write_marker(&dirs).unwrap();
        assert!(marker_path(&dirs).exists());
    }
}
