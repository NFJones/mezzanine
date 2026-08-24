//! Shared snapshot encoding, parsing, time, and permission helpers.
//!
//! The snapshot formats are intentionally small and deterministic. This module
//! centralizes escaping, primitive parsing, timestamp formatting, id validation,
//! and private filesystem permissions.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use rustix::fs::{CWD, RenameFlags, renameat_with};
use tokio::io::AsyncWriteExt;

use crate::error::{MezError, Result};

const PUBLICATION_TEMP_PREFIX: &str = ".snapshot-publish.";
static NEXT_PUBLICATION_TEMP_ID: AtomicU64 = AtomicU64::new(0);

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PublicationFailurePhase {
    AfterWrite,
    AfterFileSync,
    BeforePublish,
    BeforeDirectorySync,
}

/// Runs the required operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn required<'a>(map: &'a BTreeMap<&str, &str>, key: &str) -> Result<&'a str> {
    map.get(key)
        .copied()
        .ok_or_else(|| MezError::invalid_args(format!("missing snapshot manifest field `{key}`")))
}

/// Runs the non empty optional operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn non_empty_optional(map: &BTreeMap<&str, &str>, key: &str) -> Option<String> {
    map.get(key)
        .copied()
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

/// Runs the parse bool operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn parse_bool(value: &str) -> Result<bool> {
    value
        .parse::<bool>()
        .map_err(|_| MezError::invalid_args("invalid boolean in snapshot manifest"))
}

/// Runs the parse usize operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn parse_usize(value: &str) -> Result<usize> {
    value
        .parse::<usize>()
        .map_err(|_| MezError::invalid_args("invalid integer in snapshot manifest"))
}

/// Runs the parse u32 operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn parse_u32(value: &str) -> Result<u32> {
    value
        .parse::<u32>()
        .map_err(|_| MezError::invalid_args("invalid integer in snapshot manifest"))
}

/// Runs the parse u64 operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn parse_u64(value: &str) -> Result<u64> {
    value
        .parse::<u64>()
        .map_err(|_| MezError::invalid_args("invalid integer in snapshot manifest"))
}

/// Runs the parse string array operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn parse_string_array(value: &str) -> Result<Vec<String>> {
    serde_json::from_str::<Vec<String>>(value)
        .map_err(|_| MezError::invalid_args("invalid string array in snapshot manifest"))
}

/// Runs the manifest string array operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn manifest_string_array(values: &[String]) -> String {
    format!(
        "[{}]",
        values
            .iter()
            .map(|value| format!(r#""{}""#, json_escape(value)))
            .collect::<Vec<_>>()
            .join(",")
    )
}

/// Runs the json escape operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn json_escape(value: &str) -> String {
    let mut escaped = String::new();
    for ch in value.chars() {
        match ch {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            ch if ch.is_control() => {
                use std::fmt::Write as _;
                let _ = write!(&mut escaped, "\\u{:04x}", ch as u32);
            }
            ch => escaped.push(ch),
        }
    }
    escaped
}

/// Runs the parse u16 operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn parse_u16(value: &str) -> Result<u16> {
    value
        .parse::<u16>()
        .map_err(|_| MezError::invalid_args("invalid integer in snapshot payload"))
}

/// Runs the validate snapshot id operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn validate_snapshot_id(snapshot_id: &str) -> Result<()> {
    if snapshot_id.is_empty()
        || snapshot_id.contains('/')
        || snapshot_id.contains('\\')
        || snapshot_id == "."
        || snapshot_id == ".."
    {
        return Err(MezError::invalid_args("invalid snapshot id"));
    }
    Ok(())
}

/// Runs the has manifest control character operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn has_manifest_control_character(value: &str) -> bool {
    value.contains('\n') || value.contains('\r')
}

/// Runs the current rfc3339 utc operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn current_rfc3339_utc() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    unix_seconds_to_rfc3339(seconds)
}

/// Runs the unix seconds to rfc3339 operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn unix_seconds_to_rfc3339(seconds: u64) -> String {
    let days = (seconds / 86_400) as i64;
    let seconds_of_day = seconds % 86_400;
    let hour = seconds_of_day / 3_600;
    let minute = (seconds_of_day % 3_600) / 60;
    let second = seconds_of_day % 60;
    let (year, month, day) = civil_from_days(days);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

/// Runs the civil from days operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn civil_from_days(days_since_unix_epoch: i64) -> (i64, i64, i64) {
    let z = days_since_unix_epoch + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    if month <= 2 {
        year += 1;
    }
    (year, month, day)
}

/// Runs the non empty string operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn non_empty_string(value: &str) -> Option<String> {
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

/// Runs the escape field operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn escape_field(value: &str) -> String {
    let mut escaped = String::new();
    for ch in value.chars() {
        match ch {
            '\\' => escaped.push_str("\\\\"),
            '\t' => escaped.push_str("\\t"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            _ => escaped.push(ch),
        }
    }
    escaped
}

/// Runs the split fields operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn split_fields(line: &str) -> Result<Vec<String>> {
    let mut fields = Vec::new();
    let mut field = String::new();
    let mut chars = line.chars();
    while let Some(ch) = chars.next() {
        match ch {
            '\t' => {
                fields.push(field);
                field = String::new();
            }
            '\\' => {
                let escaped = chars
                    .next()
                    .ok_or_else(|| MezError::invalid_args("trailing snapshot payload escape"))?;
                field.push(match escaped {
                    '\\' => '\\',
                    't' => '\t',
                    'n' => '\n',
                    'r' => '\r',
                    _ => {
                        return Err(MezError::invalid_args(
                            "unsupported snapshot payload escape",
                        ));
                    }
                });
            }
            _ => field.push(ch),
        }
    }
    fields.push(field);
    Ok(fields)
}

/// Runs the set private dir permissions operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn set_private_dir_permissions(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

/// Runs the set private dir permissions async operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) async fn set_private_dir_permissions_async(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).await?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

/// Runs the set private file permissions operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn set_private_file_permissions(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

/// Runs the set private file permissions async operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) async fn set_private_file_permissions_async(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).await?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

fn publication_temporary_path(path: &Path) -> Result<PathBuf> {
    let parent = path
        .parent()
        .ok_or_else(|| MezError::invalid_args("snapshot publication path has no parent"))?;
    let kind = path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or("file");
    Ok(parent.join(format!(
        "{PUBLICATION_TEMP_PREFIX}{}.{}.{}.tmp",
        std::process::id(),
        NEXT_PUBLICATION_TEMP_ID.fetch_add(1, Ordering::Relaxed),
        kind,
    )))
}

pub(super) fn sync_directory(path: &Path) -> Result<()> {
    fs::File::open(path)?.sync_all()?;
    Ok(())
}

pub(super) async fn sync_directory_async(path: &Path) -> Result<()> {
    tokio::fs::File::open(path).await?.sync_all().await?;
    Ok(())
}

fn ensure_private_directory_durable(path: &Path) -> Result<()> {
    let existed = path.exists();
    fs::create_dir_all(path)?;
    set_private_dir_permissions(path)?;
    if !existed && let Some(parent) = path.parent() {
        sync_directory(parent)?;
    }
    Ok(())
}

async fn ensure_private_directory_durable_async(path: &Path) -> Result<()> {
    let existed = tokio::fs::metadata(path).await.is_ok();
    tokio::fs::create_dir_all(path).await?;
    set_private_dir_permissions_async(path).await?;
    if !existed && let Some(parent) = path.parent() {
        sync_directory_async(parent).await?;
    }
    Ok(())
}

fn publish_new(temporary: &Path, path: &Path) -> Result<()> {
    renameat_with(CWD, temporary, CWD, path, RenameFlags::NOREPLACE)
        .map_err(std::io::Error::from)?;
    Ok(())
}

async fn publish_new_async(temporary: PathBuf, path: PathBuf) -> Result<()> {
    tokio::task::spawn_blocking(move || publish_new(&temporary, &path))
        .await
        .map_err(|error| {
            MezError::invalid_state(format!("snapshot publication rename task failed: {error}"))
        })??;
    Ok(())
}

#[cfg(test)]
fn fail_publication_at(
    configured: Option<PublicationFailurePhase>,
    phase: PublicationFailurePhase,
) -> Result<()> {
    if configured == Some(phase) {
        return Err(MezError::invalid_state(format!(
            "injected snapshot publication failure at {phase:?}"
        )));
    }
    Ok(())
}

/// Publishes one new private file without exposing partially written bytes or
/// replacing an existing destination.
pub(super) fn write_private_new_atomic(path: &Path, bytes: &[u8]) -> Result<PathBuf> {
    write_private_new_atomic_impl(path, bytes, None)
}

#[cfg(test)]
pub(super) fn write_private_new_atomic_failing(
    path: &Path,
    bytes: &[u8],
    phase: PublicationFailurePhase,
) -> Result<PathBuf> {
    write_private_new_atomic_impl(path, bytes, Some(phase))
}

#[cfg(test)]
type ConfiguredPublicationFailure = Option<PublicationFailurePhase>;
#[cfg(not(test))]
type ConfiguredPublicationFailure = Option<()>;

fn write_private_new_atomic_impl(
    path: &Path,
    bytes: &[u8],
    _failure: ConfiguredPublicationFailure,
) -> Result<PathBuf> {
    let parent = path
        .parent()
        .ok_or_else(|| MezError::invalid_args("snapshot publication path has no parent"))?;
    ensure_private_directory_durable(parent)?;
    let temporary = publication_temporary_path(path)?;
    let result = (|| -> Result<PathBuf> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        set_private_file_permissions(&temporary)?;
        file.write_all(bytes)?;
        file.flush()?;
        #[cfg(test)]
        fail_publication_at(_failure, PublicationFailurePhase::AfterWrite)?;
        file.sync_all()?;
        #[cfg(test)]
        fail_publication_at(_failure, PublicationFailurePhase::AfterFileSync)?;
        drop(file);
        #[cfg(test)]
        fail_publication_at(_failure, PublicationFailurePhase::BeforePublish)?;
        publish_new(&temporary, path)?;
        #[cfg(test)]
        fail_publication_at(_failure, PublicationFailurePhase::BeforeDirectorySync)?;
        sync_directory(parent)?;
        Ok(path.to_path_buf())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

/// Tokio counterpart to [`write_private_new_atomic`].
pub(super) async fn write_private_new_atomic_async(path: &Path, bytes: &[u8]) -> Result<PathBuf> {
    let parent = path
        .parent()
        .ok_or_else(|| MezError::invalid_args("snapshot publication path has no parent"))?;
    ensure_private_directory_durable_async(parent).await?;
    let temporary = publication_temporary_path(path)?;
    let result = async {
        let mut file = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .await?;
        set_private_file_permissions_async(&temporary).await?;
        file.write_all(bytes).await?;
        file.flush().await?;
        file.sync_all().await?;
        drop(file);
        publish_new_async(temporary.clone(), path.to_path_buf()).await?;
        sync_directory_async(parent).await?;
        Ok(path.to_path_buf())
    }
    .await;
    if result.is_err() {
        let _ = tokio::fs::remove_file(&temporary).await;
    }
    result
}

/// Atomically replaces a derived private file and requires its renamed
/// directory entry to reach stable storage before returning.
pub(super) fn write_private_replace_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| MezError::invalid_args("snapshot publication path has no parent"))?;
    ensure_private_directory_durable(parent)?;
    let temporary = publication_temporary_path(path)?;
    let result = (|| -> Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        set_private_file_permissions(&temporary)?;
        file.write_all(bytes)?;
        file.flush()?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, path)?;
        sync_directory(parent)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

/// Tokio counterpart to [`write_private_replace_atomic`].
pub(super) async fn write_private_replace_atomic_async(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| MezError::invalid_args("snapshot publication path has no parent"))?;
    ensure_private_directory_durable_async(parent).await?;
    let temporary = publication_temporary_path(path)?;
    let result = async {
        let mut file = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .await?;
        set_private_file_permissions_async(&temporary).await?;
        file.write_all(bytes).await?;
        file.flush().await?;
        file.sync_all().await?;
        drop(file);
        tokio::fs::rename(&temporary, path).await?;
        sync_directory_async(parent).await
    }
    .await;
    if result.is_err() {
        let _ = tokio::fs::remove_file(&temporary).await;
    }
    result
}

pub(super) fn reconcile_publication_temporaries(root: &Path) -> Result<usize> {
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error.into()),
    };
    let mut removed = 0usize;
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let publication_temporary =
            name.starts_with(PUBLICATION_TEMP_PREFIX) && name.ends_with(".tmp");
        let legacy_index_temporary = name.starts_with(".latest.index.") && name.ends_with(".tmp");
        let orphan_payload = name.ends_with(".payload")
            && !root
                .join(format!("{}.manifest", name.trim_end_matches(".payload")))
                .exists();
        if publication_temporary || legacy_index_temporary || orphan_payload {
            let file_type = entry.file_type()?;
            if file_type.is_file() || file_type.is_symlink() {
                fs::remove_file(entry.path())?;
                removed = removed.saturating_add(1);
            }
        }
    }
    if removed > 0 {
        sync_directory(root)?;
    }
    Ok(removed)
}
