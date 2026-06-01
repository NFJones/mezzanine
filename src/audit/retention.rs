//! Retention policy enforcement for JSON Lines audit files.
//!
//! Retention operates on complete records by line and preserves private file
//! permissions after compaction. Malformed timestamps are retained.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::SystemTime;

use tokio::io::AsyncWriteExt;

use crate::error::Result;

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
        if !path.exists() {
            return Ok(AuditRetentionReport::default());
        }

        let data = fs::read_to_string(path)?;
        let retained = self.retained_jsonl_lines(data.as_str(), now);
        let mut retained_data = retained.retained_data();
        let should_rewrite = retained.should_rewrite(retained_data.len() as u64);
        if should_rewrite {
            retained_data = rehash_retained_hash_chain_lines(&retained_data);
        }
        let report = retained.report(retained_data.len() as u64);

        if should_rewrite {
            let mut file = OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(path)?;
            file.write_all(retained_data.as_bytes())?;
            file.sync_all()?;
            fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        }

        Ok(report)
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

        let data = match tokio::fs::read_to_string(path).await {
            Ok(data) => data,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(AuditRetentionReport::default());
            }
            Err(error) => return Err(error.into()),
        };
        let retained = self.retained_jsonl_lines(data.as_str(), now);
        let mut retained_data = retained.retained_data();
        let should_rewrite = retained.should_rewrite(retained_data.len() as u64);
        if should_rewrite {
            retained_data = rehash_retained_hash_chain_lines(&retained_data);
        }
        let report = retained.report(retained_data.len() as u64);

        if should_rewrite {
            let mut file = tokio::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(path)
                .await?;
            file.write_all(retained_data.as_bytes()).await?;
            file.sync_all().await?;
            tokio::fs::set_permissions(path, fs::Permissions::from_mode(0o600)).await?;
        }

        Ok(report)
    }

    /// Runs the retained jsonl lines operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn retained_jsonl_lines(&self, data: &str, now: SystemTime) -> RetainedAuditJsonl {
        let original_bytes = data.len() as u64;
        let mut retained = data.lines().map(str::to_string).collect::<Vec<_>>();
        let original_records = retained.len();

        if let Some(max_age_days) = self.max_age_days {
            let now_seconds = unix_seconds(now);
            let max_age_seconds = max_age_days.saturating_mul(24 * 60 * 60);
            retained.retain(|line| {
                record_timestamp_seconds(line).is_none_or(|timestamp| {
                    timestamp >= now_seconds || now_seconds - timestamp <= max_age_seconds
                })
            });
        }

        if let Some(max_records) = self.max_records
            && retained.len() > max_records
        {
            retained = retained.split_off(retained.len() - max_records);
        }

        if let Some(max_bytes) = self.max_bytes {
            let mut newest_within_limit = Vec::new();
            let mut retained_bytes = 0_u64;
            for line in retained.into_iter().rev() {
                let line_bytes = line.len() as u64 + 1;
                if retained_bytes + line_bytes > max_bytes {
                    break;
                }
                retained_bytes += line_bytes;
                newest_within_limit.push(line);
            }
            newest_within_limit.reverse();
            retained = newest_within_limit;
        }

        RetainedAuditJsonl {
            original_records,
            original_bytes,
            lines: retained,
        }
    }
}

/// Carries Retained Audit Jsonl state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
struct RetainedAuditJsonl {
    /// Stores the original records value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    original_records: usize,
    /// Stores the original bytes value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    original_bytes: u64,
    /// Stores the lines value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    lines: Vec<String>,
}

impl RetainedAuditJsonl {
    /// Runs the retained data operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn retained_data(&self) -> String {
        if self.lines.is_empty() {
            String::new()
        } else {
            format!("{}\n", self.lines.join("\n"))
        }
    }

    /// Returns whether the retained data differs from the original file.
    fn should_rewrite(&self, retained_bytes: u64) -> bool {
        self.original_records.saturating_sub(self.lines.len()) > 0
            || retained_bytes != self.original_bytes
    }

    /// Runs the report operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn report(&self, retained_bytes: u64) -> AuditRetentionReport {
        AuditRetentionReport {
            original_records: self.original_records,
            retained_records: self.lines.len(),
            pruned_records: self.original_records.saturating_sub(self.lines.len()),
            original_bytes: self.original_bytes,
            retained_bytes,
        }
    }
}

/// Rebuilds retained hash chains from the first retained record.
fn rehash_retained_hash_chain_lines(data: &str) -> String {
    let mut previous_hash = None;
    let mut lines = Vec::new();

    for line in data.lines() {
        let Some(base_line) = audit_line_without_trailing_hash(line) else {
            previous_hash = None;
            lines.push(line.to_string());
            continue;
        };
        let hash = chained_hash(previous_hash.as_deref(), &base_line);
        lines.push(insert_hash_field(base_line, &hash));
        previous_hash = Some(hash);
    }

    if lines.is_empty() {
        String::new()
    } else {
        format!("{}\n", lines.join("\n"))
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
