//! Host-owned source resolution. Graph properties and webview paths never
//! authorize filesystem access. Guards coordinate participating app processes;
//! reads remain current working-tree reads, not captured producer evidence.

use crate::sources::{RegisteredSource, SourceRegistry};
use ingest::managed::{
    ManagedReadGuard, ManagedReadReservation, ManagedWriteGuard, ManagedWriteReservation,
};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Mutex;

const UNAVAILABLE: &str = "registered source unavailable; reconnect or re-run ingestion";

pub(crate) fn with_registered_read<T>(
    registry: &Mutex<SourceRegistry>,
    repo: &str,
    read: impl FnOnce(&Path) -> Result<T, String>,
) -> Result<T, String> {
    let source = registry
        .lock()
        .map_err(|e| e.to_string())?
        .get_by_repo(repo)?
        .ok_or(UNAVAILABLE)?;
    let _guard = source
        .managed()?
        .map(|managed| managed.try_read())
        .transpose()
        .map_err(|e| e.to_string())?;
    // Re-read availability after locking: a previously ready record may have
    // been invalidated by a failed replacement while this caller was resolving.
    validate_ready(registry, &source)?;
    read(source.root())
}

fn validate_ready(
    registry: &Mutex<SourceRegistry>,
    source: &RegisteredSource,
) -> Result<(), String> {
    let current = registry
        .lock()
        .map_err(|e| e.to_string())?
        .get_by_id(&source.source_id)?
        .ok_or(UNAVAILABLE)?;
    if current.repo_key != source.repo_key
        || current.root() != source.root()
        || current.is_managed() != source.is_managed()
        || !current.is_ready()
    {
        return Err(UNAVAILABLE.into());
    }
    Ok(())
}

enum Guard {
    Read(ManagedReadGuard),
    Write(ManagedWriteGuard),
}

enum Reservation {
    Read(ManagedReadReservation),
    Write(ManagedWriteReservation),
}

/// A sorted, try-only lock plan shared by intake, enrichment and ADR relinking.
/// All guards outlive the entire operation; nested readers reuse this plan.
pub(crate) struct SourceOperation {
    sources: BTreeMap<String, RegisteredSource>,
    guards: BTreeMap<String, Guard>,
}

impl SourceOperation {
    pub(crate) fn acquire(
        registry: &Mutex<SourceRegistry>,
        requested: impl IntoIterator<Item = (RegisteredSource, bool)>,
    ) -> Result<Self, String> {
        let mut plan: BTreeMap<String, (RegisteredSource, bool)> = BTreeMap::new();
        for (source, write) in requested {
            if write && !source.is_managed() {
                return Err("direct local sources cannot be replaced".into());
            }
            plan.entry(source.source_id.clone())
                .and_modify(|(_, existing_write)| *existing_write |= write)
                .or_insert((source, write));
        }
        let mut reservations = BTreeMap::new();
        let mut write_ids = Vec::new();
        for (id, (source, write)) in &plan {
            if let Some(managed) = source.managed()? {
                let reservation = if *write {
                    write_ids.push(id.clone());
                    Reservation::Write(managed.try_reserve_write().map_err(|e| e.to_string())?)
                } else {
                    Reservation::Read(managed.try_reserve_read().map_err(|e| e.to_string())?)
                };
                reservations.insert(id.clone(), reservation);
            }
        }
        // No DB mutex spans an OS lock attempt. A failure acquiring any planned
        // handle releases the reservations without invalidating ready sources.
        // Once every handle is held, persist all write sources unavailable BEFORE
        // any slot initialization or checkout validation can fail. Reservations
        // promote with the same handles, so there is no unlocked transition.
        if !write_ids.is_empty() {
            registry
                .lock()
                .map_err(|e| e.to_string())?
                .set_ready_batch(&write_ids, false)?;
        }
        let mut operation = Self {
            sources: BTreeMap::new(),
            guards: BTreeMap::new(),
        };
        for (id, (source, write)) in plan {
            if let Some(reservation) = reservations.remove(&id) {
                let guard = match reservation {
                    Reservation::Read(reservation) => {
                        Guard::Read(reservation.validate().map_err(|e| e.to_string())?)
                    }
                    Reservation::Write(reservation) => {
                        Guard::Write(reservation.initialize().map_err(|e| e.to_string())?)
                    }
                };
                operation.guards.insert(id, guard);
            }
            if !write {
                validate_ready(registry, &source)?;
            }
            operation.sources.insert(source.repo_key.clone(), source);
        }
        Ok(operation)
    }

    pub(crate) fn root(&self, repo: &str) -> Result<&Path, String> {
        let source = self
            .sources
            .get(repo)
            .ok_or("source context changed; retry recovery")?;
        match self.guards.get(&source.source_id) {
            Some(Guard::Read(guard)) => Ok(guard.root()),
            Some(Guard::Write(guard)) => Ok(guard.root()),
            None => Ok(source.root()),
        }
    }

    pub(crate) fn clone_source(
        &mut self,
        source: &RegisteredSource,
        token: Option<&str>,
    ) -> Result<ingest::ClonedRepo, String> {
        let origin = ingest::managed::parse_managed_origin(source.clone_url().ok_or(UNAVAILABLE)?)
            .map_err(|e| e.to_string())?;
        if Some(origin.clone_url.as_str()) != source.clone_url() {
            return Err("registered clone origin changed; reconnect the source".into());
        }
        match self.guards.get_mut(&source.source_id) {
            Some(Guard::Write(guard)) => {
                guard.clone_from(&origin, token).map_err(|e| e.to_string())
            }
            _ => Err("managed replacement requires the source operation guard".into()),
        }
    }

    pub(crate) fn set_writes_ready(
        &self,
        registry: &Mutex<SourceRegistry>,
        ready: bool,
    ) -> Result<(), String> {
        let ids = self
            .guards
            .iter()
            .filter(|(_, guard)| matches!(guard, Guard::Write(_)))
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        let mut registry = registry.lock().map_err(|e| e.to_string())?;
        match ids.as_slice() {
            [] => Ok(()),
            [id] => registry.set_ready(id, ready),
            _ => registry.set_ready_batch(&ids, ready),
        }
    }
}
