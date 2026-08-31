use chrono::Utc;
use ring::digest::{digest, SHA256};
use rusqlite::{params, Connection, OptionalExtension};

use familiar_ai_core::FamiliarError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BillingSource<'a> {
    pub name: &'a str,
    pub mode: &'a str,
    pub organization_id: &'a str,
    pub organization_name: &'a str,
    pub credential_reference: &'a str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderCostRow {
    pub bucket_start: String,
    pub bucket_end: String,
    pub workspace_id: String,
    pub description: String,
    pub charge_class: String,
    pub currency: String,
    pub amount_lexical: String,
    pub provider_payload: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BillingStatus {
    pub source_name: String,
    pub organization_name: String,
    pub last_success: Option<String>,
    pub window_start: Option<String>,
    pub window_end: Option<String>,
    pub last_failure: Option<String>,
}

pub struct BillingRepository<'a> {
    conn: &'a Connection,
}

impl<'a> BillingRepository<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    pub fn bind_source(&self, source: &BillingSource<'_>) -> familiar_ai_core::Result<()> {
        let duplicate: Option<String> = self
            .conn
            .query_row(
                "SELECT source_name FROM billing_sources WHERE organization_id=?1",
                [source.organization_id],
                |r| r.get(0),
            )
            .optional()
            .map_err(db)?;
        if let Some(name) = duplicate {
            if name != source.name {
                return Err(FamiliarError::Database(format!(
                    "organization '{}' is already bound to billing source '{name}'",
                    source.organization_id
                )));
            }
            return Ok(());
        }
        self.conn.execute("INSERT INTO billing_sources(source_name,mode,organization_id,organization_name,credential_reference,created_at) VALUES(?1,?2,?3,?4,?5,?6)",
            params![source.name,source.mode,source.organization_id,source.organization_name,source.credential_reference,Utc::now().to_rfc3339()]).map_err(db)?;
        Ok(())
    }

    pub fn source_for_organization(
        &self,
        organization_id: &str,
    ) -> familiar_ai_core::Result<Option<String>> {
        self.conn
            .query_row(
                "SELECT source_name FROM billing_sources WHERE organization_id=?1",
                [organization_id],
                |r| r.get(0),
            )
            .optional()
            .map_err(db)
    }

    pub fn record_failed(
        &self,
        source: &str,
        start: &str,
        end: &str,
        cursor: Option<&str>,
        remedy: &str,
    ) -> familiar_ai_core::Result<()> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute("INSERT INTO billing_collection_attempts(attempt_id,source_name,window_start,window_end,started_at,completed_at,status,cursor,remedy) VALUES(?1,?2,?3,?4,?5,?5,'failed',?6,?7)",
            params![id("bat",format!("{source}\0{start}\0{end}\0{now}").as_bytes()),source,start,end,now,cursor,remedy]).map_err(db)?;
        Ok(())
    }

    /// Commits a snapshot only after the caller has obtained every page.
    pub fn commit_complete(
        &self,
        source: &str,
        start: &str,
        end: &str,
        rows: &[ProviderCostRow],
    ) -> familiar_ai_core::Result<usize> {
        let now = Utc::now().to_rfc3339();
        let attempt = id("bat", format!("{source}\0{start}\0{end}\0{now}").as_bytes());
        let tx = self.conn.unchecked_transaction().map_err(db)?;
        tx.execute("INSERT INTO billing_collection_attempts(attempt_id,source_name,window_start,window_end,started_at,completed_at,status) VALUES(?1,?2,?3,?4,?5,?5,'complete')",params![attempt,source,start,end,now]).map_err(db)?;
        let mut added = 0;
        for row in rows {
            let amount = decimal_nanousd(&row.amount_lexical).ok_or_else(|| {
                FamiliarError::Database(format!(
                    "invalid provider USD lexical amount '{}'",
                    row.amount_lexical
                ))
            })?;
            let logical_material = format!(
                "{source}\0{}\0{}\0{}\0{}\0{}\0{}",
                row.workspace_id,
                row.description,
                row.charge_class,
                row.currency,
                row.bucket_start,
                row.bucket_end
            );
            let logical = hex_hash(logical_material.as_bytes());
            let payload = hex_hash(row.provider_payload.as_bytes());
            let prior: Option<String>=tx.query_row("SELECT revision_id FROM provider_cost_revisions WHERE source_name=?1 AND logical_identity_hash=?2 ORDER BY observed_at DESC, revision_id DESC LIMIT 1",params![source,logical],|r|r.get(0)).optional().map_err(db)?;
            let revision = id("bcr", format!("{source}\0{payload}").as_bytes());
            added += tx.execute("INSERT OR IGNORE INTO provider_cost_revisions(revision_id,attempt_id,source_name,logical_identity_hash,payload_hash,predecessor_revision_id,bucket_start,bucket_end,workspace_id,description,charge_class,currency,amount_lexical,amount_nanousd,provider_payload,observed_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16)",params![revision,attempt,source,logical,payload,prior,row.bucket_start,row.bucket_end,row.workspace_id,row.description,row.charge_class,row.currency,row.amount_lexical,amount,row.provider_payload,now]).map_err(db)?;
        }
        tx.commit().map_err(db)?;
        Ok(added)
    }

    pub fn statuses(&self) -> familiar_ai_core::Result<Vec<BillingStatus>> {
        let mut stmt=self.conn.prepare("SELECT s.source_name,s.organization_name,(SELECT completed_at FROM billing_collection_attempts a WHERE a.source_name=s.source_name AND a.status='complete' ORDER BY completed_at DESC LIMIT 1),(SELECT window_start FROM billing_collection_attempts a WHERE a.source_name=s.source_name AND a.status='complete' ORDER BY completed_at DESC LIMIT 1),(SELECT window_end FROM billing_collection_attempts a WHERE a.source_name=s.source_name AND a.status='complete' ORDER BY completed_at DESC LIMIT 1),(SELECT remedy FROM billing_collection_attempts a WHERE a.source_name=s.source_name AND a.status='failed' ORDER BY completed_at DESC LIMIT 1) FROM billing_sources s ORDER BY s.source_name").map_err(db)?;
        let rows = stmt
            .query_map([], |r| {
                Ok(BillingStatus {
                    source_name: r.get(0)?,
                    organization_name: r.get(1)?,
                    last_success: r.get(2)?,
                    window_start: r.get(3)?,
                    window_end: r.get(4)?,
                    last_failure: r.get(5)?,
                })
            })
            .map_err(db)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(db)?;
        Ok(rows)
    }

    pub fn effective_total_nanousd(&self) -> familiar_ai_core::Result<i64> {
        self.conn
            .query_row(
                "SELECT coalesce(sum(amount_nanousd),0) FROM current_provider_costs",
                [],
                |r| r.get(0),
            )
            .map_err(db)
    }
}

pub fn decimal_nanousd(value: &str) -> Option<i64> {
    let value = value.trim();
    let (negative, digits) = value
        .strip_prefix('-')
        .map_or((false, value), |v| (true, v));
    let (whole, fraction) = digits.split_once('.').unwrap_or((digits, ""));
    if whole.is_empty()
        || !whole.bytes().all(|b| b.is_ascii_digit())
        || !fraction.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    let base = whole.parse::<i128>().ok()?.checked_mul(1_000_000_000)?;
    let kept = &fraction[..fraction.len().min(9)];
    let mut nanos = if kept.is_empty() {
        0
    } else {
        kept.parse::<i128>().ok()?
    };
    nanos = nanos.checked_mul(10_i128.pow((9 - kept.len()) as u32))?;
    if fraction.len() > 9 {
        let rest = &fraction.as_bytes()[9..];
        let round = rest[0] > b'5'
            || (rest[0] == b'5' && (rest[1..].iter().any(|b| *b != b'0') || nanos % 2 == 1));
        if round {
            nanos += 1;
        }
    }
    let total = base.checked_add(nanos)?;
    let signed = if negative { -total } else { total };
    i64::try_from(signed).ok()
}

fn hex_hash(bytes: &[u8]) -> String {
    digest(&SHA256, bytes)
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
fn id(prefix: &str, bytes: &[u8]) -> String {
    format!("{prefix}_{}", hex_hash(bytes))
}
fn db(e: rusqlite::Error) -> FamiliarError {
    FamiliarError::Database(e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn row(amount: &str, payload: &str, class: &str) -> ProviderCostRow {
        ProviderCostRow {
            bucket_start: "2026-08-01T00:00:00Z".into(),
            bucket_end: "2026-08-02T00:00:00Z".into(),
            workspace_id: "wrk_1".into(),
            description: "usage".into(),
            charge_class: class.into(),
            currency: "USD".into(),
            amount_lexical: amount.into(),
            provider_payload: payload.into(),
        }
    }
    #[test]
    fn decimal_is_signed_exact_and_half_even() {
        assert_eq!(decimal_nanousd("0.0000000015"), Some(2));
        assert_eq!(decimal_nanousd("0.0000000025"), Some(2));
        assert_eq!(decimal_nanousd("-1.25"), Some(-1_250_000_000));
        assert_eq!(decimal_nanousd("1e-3"), None);
    }
    #[test]
    fn revisions_deduplicate_and_only_latest_is_effective() {
        let db = crate::Database::open_in_memory().unwrap();
        db.run_migrations().unwrap();
        let repo = BillingRepository::new(db.conn());
        repo.bind_source(&BillingSource {
            name: "org-a",
            mode: "anthropic-organization",
            organization_id: "org_a",
            organization_name: "A",
            credential_reference: "env: ADMIN_A",
        })
        .unwrap();
        assert_eq!(
            repo.commit_complete(
                "org-a",
                "2026-08-01",
                "2026-08-02",
                &[row("1.00", r#"{"amount":"1.00"}"#, "token-spend")]
            )
            .unwrap(),
            1
        );
        assert_eq!(
            repo.commit_complete(
                "org-a",
                "2026-08-01",
                "2026-08-02",
                &[row("1.00", r#"{"amount":"1.00"}"#, "token-spend")]
            )
            .unwrap(),
            0
        );
        assert_eq!(
            repo.commit_complete(
                "org-a",
                "2026-08-01",
                "2026-08-02",
                &[row("-0.25", r#"{"amount":"-0.25"}"#, "token-spend")]
            )
            .unwrap(),
            1
        );
        assert_eq!(repo.effective_total_nanousd().unwrap(), -250_000_000);
        let evidence: i64 = db
            .conn()
            .query_row("SELECT count(*) FROM provider_cost_revisions", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(evidence, 2);
    }
    #[test]
    fn sources_have_independent_failures_and_duplicate_org_is_closed() {
        let db = crate::Database::open_in_memory().unwrap();
        db.run_migrations().unwrap();
        let repo = BillingRepository::new(db.conn());
        repo.bind_source(&BillingSource {
            name: "a",
            mode: "anthropic-organization",
            organization_id: "org_a",
            organization_name: "A",
            credential_reference: "env: KEY_A",
        })
        .unwrap();
        repo.bind_source(&BillingSource {
            name: "b",
            mode: "anthropic-organization",
            organization_id: "org_b",
            organization_name: "B",
            credential_reference: "env: KEY_B",
        })
        .unwrap();
        assert!(repo
            .bind_source(&BillingSource {
                name: "duplicate",
                mode: "anthropic-organization",
                organization_id: "org_a",
                organization_name: "A",
                credential_reference: "env: OTHER"
            })
            .is_err());
        repo.record_failed("a", "s", "e", Some("cursor"), "expired credential")
            .unwrap();
        let statuses = repo.statuses().unwrap();
        assert_eq!(statuses.len(), 2);
        assert!(statuses
            .iter()
            .find(|s| s.source_name == "a")
            .unwrap()
            .last_failure
            .is_some());
        assert!(statuses
            .iter()
            .find(|s| s.source_name == "b")
            .unwrap()
            .last_failure
            .is_none());
    }
}
