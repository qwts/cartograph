//! Persistent tier/provider settings and standing, revocable cloud consent
//! on the state spine (#118).
//!
//! Fail-closed layering (ADR-0004): a **standing tier consent** recorded
//! here only *permits* cloud for that tier — every actual call still passes
//! the egress firewall's per-payload grant. Revocation is immediate; the
//! default is local-only; and T0 is not a row in this store at all — the
//! deterministic tier is always on and never invokes an LLM, so there is
//! nothing to configure or disable.

use rusqlite::{Connection, params};
use serde::Serialize;
use std::path::Path;

/// Configurable recovery tiers (T0 is intentionally absent: always-on,
/// never an LLM).
pub const TIERS: &[&str] = &["T1", "T2", "T3"];
/// Tiers that use a model provider and may opt into cloud.
const LLM_TIERS: &[&str] = &["T2", "T3"];

/// One tier's persisted configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TierSettings {
    /// `T1` | `T2` | `T3`.
    pub tier: String,
    /// Whether the tier participates in recovery.
    pub enabled: bool,
    /// `local` | `cloud` — meaningful for T2/T3 only (`local` for T1).
    pub provider: String,
    /// Standing cloud consent (only possible with `provider == cloud`).
    pub consented: bool,
    /// The disclosure shown when consent was granted (audit trail), JSON.
    pub consent_disclosure: Option<String>,
    /// When consent was granted (UTC, ISO-8601).
    pub consented_at: Option<String>,
}

/// Live egress summary for the status bar (handoff §App Shell).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EgressSummary {
    /// Tiers currently consented to cloud (empty ⇒ local-only).
    pub cloud_tiers: Vec<String>,
    /// Total observed egress bytes across all cloud calls.
    pub bytes_sent: u64,
    /// Rendered line, e.g. `Local-only · 0 bytes egress`.
    pub label: String,
}

/// A settings transition that violates an invariant, or a storage failure.
#[derive(Debug)]
pub enum SettingsError {
    UnknownTier(String),
    NoProvider(String),
    NotCloudProvider,
    InvalidProvider(String),
    Store(rusqlite::Error),
}

impl std::fmt::Display for SettingsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownTier(tier) => write!(
                f,
                "unknown tier '{tier}' — configurable tiers are T1, T2, T3 (T0 is always on)"
            ),
            Self::NoProvider(tier) => write!(f, "tier {tier} does not use a model provider"),
            Self::NotCloudProvider => write!(
                f,
                "cloud consent requires the tier's provider to be 'cloud' (fail closed)"
            ),
            Self::InvalidProvider(provider) => {
                write!(f, "provider must be 'local' or 'cloud', got '{provider}'")
            }
            Self::Store(error) => write!(f, "settings store: {error}"),
        }
    }
}

impl std::error::Error for SettingsError {}

impl From<rusqlite::Error> for SettingsError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Store(error)
    }
}

/// Store for tier settings and the egress log, on the state spine.
pub struct SettingsStore {
    conn: Connection,
}

impl SettingsStore {
    /// Open (creating if absent) the settings tables at `path`. Missing
    /// tier rows are seeded with the local-only defaults.
    pub fn open(path: impl AsRef<Path>) -> rusqlite::Result<Self> {
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS tier_settings (
                 tier               TEXT PRIMARY KEY CHECK (tier IN ('T1', 'T2', 'T3')),
                 enabled            INTEGER NOT NULL CHECK (enabled IN (0, 1)),
                 provider           TEXT NOT NULL CHECK (provider IN ('local', 'cloud')),
                 consented          INTEGER NOT NULL CHECK (consented IN (0, 1)),
                 consent_disclosure TEXT,
                 consented_at       TEXT
             ) STRICT;
             CREATE TABLE IF NOT EXISTS egress_log (
                 id         INTEGER PRIMARY KEY AUTOINCREMENT,
                 provider   TEXT NOT NULL,
                 bytes      INTEGER NOT NULL,
                 created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ','now'))
             ) STRICT;",
        )?;
        for tier in TIERS {
            conn.execute(
                "INSERT OR IGNORE INTO tier_settings (tier, enabled, provider, consented)
                 VALUES (?1, 1, 'local', 0)",
                params![tier],
            )?;
        }
        Ok(Self { conn })
    }

    /// All tier settings, T1 → T3.
    pub fn all(&self) -> rusqlite::Result<Vec<TierSettings>> {
        let mut stmt = self.conn.prepare(
            "SELECT tier, enabled, provider, consented, consent_disclosure, consented_at
             FROM tier_settings ORDER BY tier",
        )?;
        stmt.query_map([], |row| {
            Ok(TierSettings {
                tier: row.get(0)?,
                enabled: row.get(1)?,
                provider: row.get(2)?,
                consented: row.get(3)?,
                consent_disclosure: row.get(4)?,
                consented_at: row.get(5)?,
            })
        })?
        .collect()
    }

    fn require_tier(&self, tier: &str) -> Result<(), SettingsError> {
        if TIERS.contains(&tier) {
            Ok(())
        } else {
            Err(SettingsError::UnknownTier(tier.to_string()))
        }
    }

    /// Enable/disable a configurable tier. T0 is rejected by the tier
    /// vocabulary itself — it is always on.
    pub fn set_enabled(&mut self, tier: &str, enabled: bool) -> Result<(), SettingsError> {
        self.require_tier(tier)?;
        self.conn.execute(
            "UPDATE tier_settings SET enabled = ?2 WHERE tier = ?1",
            params![tier, enabled],
        )?;
        Ok(())
    }

    /// Choose `local` or `cloud` for an LLM tier (T2/T3). Switching away
    /// from cloud always revokes any standing consent (fail closed).
    pub fn set_provider(&mut self, tier: &str, provider: &str) -> Result<(), SettingsError> {
        self.require_tier(tier)?;
        if !LLM_TIERS.contains(&tier) {
            return Err(SettingsError::NoProvider(tier.to_string()));
        }
        if provider != "local" && provider != "cloud" {
            return Err(SettingsError::InvalidProvider(provider.to_string()));
        }
        self.conn.execute(
            "UPDATE tier_settings SET provider = ?2,
                 consented = CASE WHEN ?2 = 'cloud' THEN consented ELSE 0 END,
                 consent_disclosure = CASE WHEN ?2 = 'cloud' THEN consent_disclosure ELSE NULL END,
                 consented_at = CASE WHEN ?2 = 'cloud' THEN consented_at ELSE NULL END
             WHERE tier = ?1",
            params![tier, provider],
        )?;
        Ok(())
    }

    /// Record standing cloud consent for a tier, keeping the disclosure the
    /// user saw as the audit trail. Only possible when the tier's provider
    /// is already `cloud`.
    pub fn grant_consent(&mut self, tier: &str, disclosure: &str) -> Result<(), SettingsError> {
        self.require_tier(tier)?;
        let provider: String = self.conn.query_row(
            "SELECT provider FROM tier_settings WHERE tier = ?1",
            params![tier],
            |row| row.get(0),
        )?;
        if provider != "cloud" {
            return Err(SettingsError::NotCloudProvider);
        }
        self.conn.execute(
            "UPDATE tier_settings SET consented = 1, consent_disclosure = ?2,
                 consented_at = strftime('%Y-%m-%dT%H:%M:%SZ','now')
             WHERE tier = ?1",
            params![tier, disclosure],
        )?;
        Ok(())
    }

    /// Revoke a tier's standing consent — effective immediately.
    pub fn revoke_consent(&mut self, tier: &str) -> Result<(), SettingsError> {
        self.require_tier(tier)?;
        self.conn.execute(
            "UPDATE tier_settings SET consented = 0, consent_disclosure = NULL,
                 consented_at = NULL
             WHERE tier = ?1",
            params![tier],
        )?;
        Ok(())
    }

    /// Record observed cloud egress (called where a consented cloud call
    /// actually completes; the escalation orchestration wires this — #120).
    #[allow(dead_code)] // consumed by the #120 escalation orchestration; invariants tested below
    pub fn record_egress(&mut self, provider: &str, bytes: u64) -> rusqlite::Result<()> {
        self.conn.execute(
            "INSERT INTO egress_log (provider, bytes) VALUES (?1, ?2)",
            params![provider, bytes as i64],
        )?;
        Ok(())
    }

    /// The status bar's live egress line (handoff: `Local-only · 0 bytes
    /// egress` unless a tier is consented to cloud).
    pub fn egress_summary(&self) -> rusqlite::Result<EgressSummary> {
        let cloud_tiers: Vec<String> = {
            let mut stmt = self.conn.prepare(
                "SELECT tier FROM tier_settings
                 WHERE enabled = 1 AND provider = 'cloud' AND consented = 1
                 ORDER BY tier",
            )?;
            stmt.query_map([], |row| row.get(0))?
                .collect::<Result<Vec<_>, _>>()?
        };
        let bytes_sent: i64 = self.conn.query_row(
            "SELECT COALESCE(SUM(bytes), 0) FROM egress_log",
            [],
            |row| row.get(0),
        )?;
        let bytes_sent = bytes_sent.max(0) as u64;
        let label = if cloud_tiers.is_empty() {
            format!("Local-only · {bytes_sent} bytes egress")
        } else {
            format!(
                "Cloud enabled ({}) · {bytes_sent} bytes egress",
                cloud_tiers.join(", ")
            )
        };
        Ok(EgressSummary {
            cloud_tiers,
            bytes_sent,
            label,
        })
    }

    /// Derive the egress firewall policy from persisted settings: cloud is
    /// allowed only for enabled LLM tiers whose provider is `cloud` **and**
    /// which hold standing consent. Everything else stays local-only.
    #[allow(dead_code)] // consumed by the #120 escalation orchestration; invariants tested below
    pub fn egress_policy(&self) -> rusqlite::Result<llm::EgressPolicy> {
        let mut stmt = self.conn.prepare(
            "SELECT tier FROM tier_settings
             WHERE enabled = 1 AND provider = 'cloud' AND consented = 1",
        )?;
        let tiers = stmt
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .filter_map(|tier| match tier.as_str() {
                "T2" => Some(llm::AnalysisTier::Semantic),
                "T3" => Some(llm::AnalysisTier::Agentic),
                _ => None,
            });
        Ok(llm::EgressPolicy::allow_cloud_for(tiers))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_local_only_and_durable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.db");
        {
            let store = SettingsStore::open(&path).unwrap();
            let all = store.all().unwrap();
            assert_eq!(all.len(), 3);
            assert!(all.iter().all(|tier| tier.enabled));
            assert!(all.iter().all(|tier| tier.provider == "local"));
            assert!(all.iter().all(|tier| !tier.consented));
            assert_eq!(
                store.egress_summary().unwrap().label,
                "Local-only · 0 bytes egress"
            );
        }
        // Reopen: settings survive and are not re-seeded over.
        let mut store = SettingsStore::open(&path).unwrap();
        store.set_enabled("T3", false).unwrap();
        drop(store);
        let store = SettingsStore::open(&path).unwrap();
        let t3 = &store.all().unwrap()[2];
        assert!(!t3.enabled);
    }

    #[test]
    fn t0_is_not_configurable() {
        // The deterministic tier is always on: it is not even part of the
        // settings vocabulary, so no code path can disable it.
        let dir = tempfile::tempdir().unwrap();
        let mut store = SettingsStore::open(dir.path().join("state.db")).unwrap();
        assert!(matches!(
            store.set_enabled("T0", false),
            Err(SettingsError::UnknownTier(_))
        ));
        assert!(matches!(
            store.set_provider("T0", "cloud"),
            Err(SettingsError::UnknownTier(_))
        ));
    }

    #[test]
    fn consent_is_fail_closed_and_revocation_immediate() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = SettingsStore::open(dir.path().join("state.db")).unwrap();

        // Consent without a cloud provider is impossible.
        assert!(matches!(
            store.grant_consent("T2", "{}"),
            Err(SettingsError::NotCloudProvider)
        ));
        // T1 never gets a provider choice (no LLM in the dynamic tier).
        assert!(matches!(
            store.set_provider("T1", "cloud"),
            Err(SettingsError::NoProvider(_))
        ));

        // The legal path: cloud provider, then explicit consent.
        store.set_provider("T2", "cloud").unwrap();
        store
            .grant_consent(
                "T2",
                r#"{"provider":"Anthropic","model":"claude-opus-4-8"}"#,
            )
            .unwrap();
        let summary = store.egress_summary().unwrap();
        assert_eq!(summary.cloud_tiers, vec!["T2"]);
        assert!(summary.label.starts_with("Cloud enabled (T2)"));
        // The derived firewall policy opens exactly that tier.
        assert!(
            store
                .egress_policy()
                .unwrap()
                .cloud_allowed(llm::AnalysisTier::Semantic)
        );
        assert!(
            !store
                .egress_policy()
                .unwrap()
                .cloud_allowed(llm::AnalysisTier::Agentic)
        );

        // Revocation is immediate and total.
        store.revoke_consent("T2").unwrap();
        assert!(
            !store
                .egress_policy()
                .unwrap()
                .cloud_allowed(llm::AnalysisTier::Semantic)
        );
        assert_eq!(
            store.egress_summary().unwrap().label,
            "Local-only · 0 bytes egress"
        );
        let t2 = &store.all().unwrap()[1];
        assert_eq!(t2.consent_disclosure, None);
    }

    #[test]
    fn disabling_a_tier_drops_it_from_the_cloud_summary() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = SettingsStore::open(dir.path().join("state.db")).unwrap();
        store.set_provider("T2", "cloud").unwrap();
        store.grant_consent("T2", "{}").unwrap();
        assert_eq!(store.egress_summary().unwrap().cloud_tiers, vec!["T2"]);

        // A disabled tier can never egress, so the status bar must not
        // advertise it as cloud-enabled — summary and policy stay in lockstep.
        store.set_enabled("T2", false).unwrap();
        let summary = store.egress_summary().unwrap();
        assert!(summary.cloud_tiers.is_empty());
        assert!(summary.label.starts_with("Local-only"));
        assert!(
            !store
                .egress_policy()
                .unwrap()
                .cloud_allowed(llm::AnalysisTier::Semantic)
        );
    }

    #[test]
    fn switching_away_from_cloud_revokes_consent() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = SettingsStore::open(dir.path().join("state.db")).unwrap();
        store.set_provider("T3", "cloud").unwrap();
        store.grant_consent("T3", "{}").unwrap();
        // Back to local: standing consent cannot survive the switch.
        store.set_provider("T3", "local").unwrap();
        let t3 = &store.all().unwrap()[2];
        assert!(!t3.consented);
        assert!(
            !store
                .egress_policy()
                .unwrap()
                .cloud_allowed(llm::AnalysisTier::Agentic)
        );
    }

    #[test]
    fn egress_summary_accumulates_observed_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = SettingsStore::open(dir.path().join("state.db")).unwrap();
        store
            .record_egress("anthropic:claude-opus-4-8", 1200)
            .unwrap();
        store
            .record_egress("anthropic:claude-opus-4-8", 300)
            .unwrap();
        assert_eq!(store.egress_summary().unwrap().bytes_sent, 1500);
    }
}
