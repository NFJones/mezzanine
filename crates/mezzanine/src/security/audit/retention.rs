//! Retention policy enforcement for JSON Lines audit files.
//!
//! Retention operates on complete records by line and preserves private file
//! permissions after compaction. Malformed timestamps are retained.

use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::error::{MezError, Result};

use super::json::insert_hash_field;
use super::log::chained_hash;
use super::time::{record_timestamp_seconds, unix_seconds};
use super::types::{AuditRetentionPolicy, AuditRetentionReport};

impl AuditRetentionPolicy {
    /// Runs the disabled operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn disabled() -> Self {
        Self::default()
    }

    /// Runs the retain days operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn retain_days(days: u64) -> Self {
        Self {
            max_age_days: Some(days),
            max_records: None,
            max_bytes: None,
        }
    }

    /// Runs the enforce jsonl operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn enforce_jsonl(&self, path: &Path) -> Result<AuditRetentionReport> {
        self.enforce_jsonl_at(path, SystemTime::now())
    }

    /// Runs the enforce jsonl async operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub async fn enforce_jsonl_async(&self, path: &Path) -> Result<AuditRetentionReport> {
        self.enforce_jsonl_at_async(path, SystemTime::now()).await
    }

    /// Runs the enforce jsonl at operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn enforce_jsonl_at(&self, path: &Path, now: SystemTime) -> Result<AuditRetentionReport> {
        if self.max_age_days.is_none() && self.max_records.is_none() && self.max_bytes.is_none() {
            return Ok(AuditRetentionReport::default());
        }
        let plan = match plan_audit_retention(self, path, now) {
            Ok(plan) => plan,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(AuditRetentionReport::default());
            }
            Err(error) => return Err(error.into()),
        };
        if !plan.should_rewrite() {
            return Ok(plan.report(plan.original_bytes));
        }

        let retained_bytes = compact_audit_jsonl(path, self, now, &plan)?;
        Ok(plan.report(retained_bytes))
    }

    /// Runs the enforce jsonl at async operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub async fn enforce_jsonl_at_async(
        &self,
        path: &Path,
        now: SystemTime,
    ) -> Result<AuditRetentionReport> {
        if self.max_age_days.is_none() && self.max_records.is_none() && self.max_bytes.is_none() {
            return Ok(AuditRetentionReport::default());
        }
        let policy = self.clone();
        let path = path.to_path_buf();
        tokio::task::spawn_blocking(move || policy.enforce_jsonl_at(path.as_path(), now))
            .await
            .map_err(|error| {
                MezError::invalid_state(format!("audit retention blocking worker failed: {error}"))
            })?
    }
}

/// Bounded multi-pass selection state for one audit retention operation.
struct AuditRetentionPlan {
    /// Number of records observed before applying retention.
    original_records: usize,
    /// Exact byte length observed before applying retention.
    original_bytes: u64,
    /// Number of age-eligible records skipped from the front of the file.
    retained_start: usize,
    /// Number of records selected after all retention limits.
    retained_records: usize,
    /// Normalized selected bytes before optional hash-chain rebuilding.
    retained_unhashed_bytes: u64,
}

impl AuditRetentionPlan {
    /// Returns whether the retained data differs from the original file.
    fn should_rewrite(&self) -> bool {
        self.original_records.saturating_sub(self.retained_records) > 0
            || self.retained_unhashed_bytes != self.original_bytes
    }

    /// Builds the public retention report after optional hash rebuilding.
    fn report(&self, retained_bytes: u64) -> AuditRetentionReport {
        AuditRetentionReport {
            original_records: self.original_records,
            retained_records: self.retained_records,
            pruned_records: self.original_records.saturating_sub(self.retained_records),
            original_bytes: self.original_bytes,
            retained_bytes,
        }
    }
}

/// Selects the retained suffix using bounded repeated scans of the source file.
fn plan_audit_retention(
    policy: &AuditRetentionPolicy,
    path: &Path,
    now: SystemTime,
) -> std::io::Result<AuditRetentionPlan> {
    let now_seconds = unix_seconds(now);
    let mut original_records = 0_usize;
    let mut original_bytes = 0_u64;
    let mut age_eligible_records = 0_usize;
    visit_audit_jsonl(path, |line, source_bytes| {
        original_records = original_records.saturating_add(1);
        original_bytes = original_bytes.saturating_add(source_bytes);
        if audit_line_is_age_eligible(policy, line, now_seconds) {
            age_eligible_records = age_eligible_records.saturating_add(1);
        }
        Ok(())
    })?;

    let mut retained_start = policy
        .max_records
        .map_or(0, |limit| age_eligible_records.saturating_sub(limit));
    let mut retained_unhashed_bytes = 0_u64;
    let mut eligible_index = 0_usize;
    visit_audit_jsonl(path, |line, _| {
        if audit_line_is_age_eligible(policy, line, now_seconds) {
            if eligible_index >= retained_start {
                retained_unhashed_bytes =
                    retained_unhashed_bytes.saturating_add(normalized_audit_line_bytes(line));
            }
            eligible_index = eligible_index.saturating_add(1);
        }
        Ok(())
    })?;

    if let Some(max_bytes) = policy.max_bytes
        && retained_unhashed_bytes > max_bytes
    {
        let mut eligible_index = 0_usize;
        visit_audit_jsonl(path, |line, _| {
            if audit_line_is_age_eligible(policy, line, now_seconds) {
                if eligible_index >= retained_start && retained_unhashed_bytes > max_bytes {
                    retained_unhashed_bytes =
                        retained_unhashed_bytes.saturating_sub(normalized_audit_line_bytes(line));
                    retained_start = eligible_index.saturating_add(1);
                }
                eligible_index = eligible_index.saturating_add(1);
            }
            Ok(())
        })?;
    }

    Ok(AuditRetentionPlan {
        original_records,
        original_bytes,
        retained_start,
        retained_records: age_eligible_records.saturating_sub(retained_start),
        retained_unhashed_bytes,
    })
}

/// Atomically streams selected records into a private sibling replacement.
fn compact_audit_jsonl(
    path: &Path,
    policy: &AuditRetentionPolicy,
    now: SystemTime,
    plan: &AuditRetentionPlan,
) -> Result<u64> {
    let temp_path = audit_retention_temp_path(path);
    let temp_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp_path)?;
    let mut writer = BufWriter::new(temp_file);
    let now_seconds = unix_seconds(now);
    let mut eligible_index = 0_usize;
    let mut retained_bytes = 0_u64;
    let mut previous_hash = None;

    let result = (|| -> Result<()> {
        visit_audit_jsonl(path, |line, _| {
            if !audit_line_is_age_eligible(policy, line, now_seconds) {
                return Ok(());
            }
            let should_write = eligible_index >= plan.retained_start;
            eligible_index = eligible_index.saturating_add(1);
            if !should_write {
                return Ok(());
            }

            let output = if let Some(base_line) = audit_line_without_trailing_hash(line) {
                let hash = chained_hash(previous_hash.as_deref(), &base_line);
                previous_hash = Some(hash.clone());
                insert_hash_field(base_line, &hash)
            } else {
                previous_hash = None;
                line.to_string()
            };
            writer.write_all(output.as_bytes())?;
            writer.write_all(b"\n")?;
            retained_bytes = retained_bytes.saturating_add(
                u64::try_from(output.len())
                    .unwrap_or(u64::MAX)
                    .saturating_add(1),
            );
            Ok(())
        })?;
        writer.flush()?;
        writer.get_ref().sync_all()?;
        fs::set_permissions(&temp_path, fs::Permissions::from_mode(0o600))?;
        fs::rename(&temp_path, path)?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        sync_audit_retention_parent(path);
        Ok(())
    })();

    if result.is_err() {
        let _ = fs::remove_file(&temp_path);
    }
    result.map(|()| retained_bytes)
}

/// Visits normalized JSONL records without retaining the complete file.
fn visit_audit_jsonl(
    path: &Path,
    mut visit: impl FnMut(&str, u64) -> std::io::Result<()>,
) -> std::io::Result<()> {
    let mut reader = BufReader::new(File::open(path)?);
    let mut line = String::new();
    loop {
        let source_bytes = reader.read_line(&mut line)?;
        if source_bytes == 0 {
            break;
        }
        let normalized = line
            .strip_suffix('\n')
            .unwrap_or(line.as_str())
            .strip_suffix('\r')
            .unwrap_or_else(|| line.strip_suffix('\n').unwrap_or(line.as_str()));
        visit(normalized, u64::try_from(source_bytes).unwrap_or(u64::MAX))?;
        line.clear();
    }
    Ok(())
}

/// Returns whether one record survives the configured age window.
fn audit_line_is_age_eligible(policy: &AuditRetentionPolicy, line: &str, now_seconds: u64) -> bool {
    let Some(max_age_days) = policy.max_age_days else {
        return true;
    };
    let max_age_seconds = max_age_days.saturating_mul(24 * 60 * 60);
    record_timestamp_seconds(line).is_none_or(|timestamp| {
        timestamp >= now_seconds || now_seconds - timestamp <= max_age_seconds
    })
}

/// Returns the normalized on-disk size for one retained JSONL record.
fn normalized_audit_line_bytes(line: &str) -> u64 {
    u64::try_from(line.len())
        .unwrap_or(u64::MAX)
        .saturating_add(1)
}

/// Builds a unique sibling path for a crash-safe audit replacement.
fn audit_retention_temp_path(path: &Path) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("audit.jsonl");
    path.with_file_name(format!(
        ".{file_name}.mez-retention-{}-{nonce}",
        std::process::id()
    ))
}

/// Best-effort directory synchronization after replacing the audit log.
fn sync_audit_retention_parent(path: &Path) {
    if let Some(parent) = path.parent()
        && let Ok(directory) = File::open(parent)
    {
        let _ = directory.sync_all();
    }
}

/// Removes the terminal audit hash field while preserving record field order.
fn audit_line_without_trailing_hash(line: &str) -> Option<String> {
    let marker = ",\"hash\":\"";
    let start = line.rfind(marker)?;
    if !line.ends_with("\"}") {
        return None;
    }
    let hash = &line[start + marker.len()..line.len().saturating_sub(2)];
    if !is_audit_hash_value(hash) {
        return None;
    }
    Some(format!("{}}}", &line[..start]))
}

/// Returns whether text is a non-empty hexadecimal audit hash.
fn is_audit_hash_value(value: &str) -> bool {
    !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}
