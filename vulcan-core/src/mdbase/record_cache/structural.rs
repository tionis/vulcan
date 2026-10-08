//! Structural write publication: creations, deletions, renames, and identity
//! changes, without whole-collection work.
//!
//! A change to some records' identities (types, basename, authored ID, unique
//! values) alters other records' overlays in exactly two ways: records whose
//! link resolution consults one of the old or new keys may resolve
//! differently, and records holding one of the old or new unique values may
//! gain or lose `duplicate_value` diagnostics. The reverse keys of schema v33
//! name those records, so only they are re-finished, from their published
//! local derivations. Everything else keeps its published overlay.

use super::super::links::{LinkLookupKeys, LinkTargetIndex};
use super::super::records::{
    finish_identity_stable_record, record_identity, uniqueness_diagnostics_from_identities,
    uniqueness_fields, uniqueness_key, MdbaseRecordIdentity,
};
use super::{
    cache_collection_root, cached_record, has_dynamic_local_membership, load_current_identities,
    read_record_source_stably, stat_fingerprint, store_cached_record, store_query_row,
    store_reverse_keys, verify_mdbase_control_snapshots, IdentityLookup, LocalOverlaySnapshot,
    LocalRecordSnapshot, MdbaseCachedRecord, MdbaseRecordCacheError, MdbaseRecordCacheRefresh,
    MdbaseStatFingerprint, MDBASE_RECORD_MODEL_VERSION, SCOPED_REFRESH_MAX_CHANGES,
};
use crate::mdbase::{
    is_mdbase_record_path, mdbase_query_input_evidence, MdbaseCollection, MdbaseContractRegistry,
    MdbaseRecordDocument, MdbaseTypeRegistry,
};
use crate::CacheDatabase;
use rusqlite::{params, Connection, OptionalExtension};
use std::collections::{BTreeMap, BTreeSet};

/// One record re-finished by a structural publication.
struct Refinished {
    local: MdbaseRecordDocument,
    body_facts: super::super::links::BodyLinkFacts,
    identity: MdbaseRecordIdentity,
    fingerprint: MdbaseStatFingerprint,
    /// Freshly derived from source (a written record), so its local payload
    /// is stored too; otherwise the published payload is reused.
    written: bool,
    /// The published input evidence of a record re-finished without its
    /// body: an upper bound that stays valid when only link resolution
    /// changes, as the full refresh keeps it for records it does not read.
    published_evidence: Option<super::super::MdbaseQueryInputEvidence>,
}

/// Publish a committed write that changed the collection's membership or
/// some record's identity, re-finishing only the records it can affect.
/// `None` when the reverse keys do not yet cover every row, some row is
/// stale, membership is dynamic, or the write is too large; the caller then
/// runs the full refresh.
#[allow(clippy::too_many_lines)]
pub fn publish_mdbase_structural_write(
    database: &mut CacheDatabase,
    collection: &MdbaseCollection,
    types: &MdbaseTypeRegistry,
    contracts: &MdbaseContractRegistry,
    written: &[String],
) -> Result<Option<MdbaseRecordCacheRefresh>, MdbaseRecordCacheError> {
    if written.is_empty()
        || written.len() > SCOPED_REFRESH_MAX_CHANGES
        || has_dynamic_local_membership(types)
    {
        return Ok(None);
    }
    let controls = verify_mdbase_control_snapshots(collection, types, contracts, None)?;
    let digest = controls.combined.clone();
    let root = cache_collection_root(collection)?;
    let connection = database.connection();
    if !reverse_keys_cover_every_row(connection, &root, &digest)? {
        return Ok(None);
    }

    // Old identities of the written paths, and their new state on disk.
    let previous = load_query_state(connection, &root, &digest, written)?;
    let unique_fields = uniqueness_fields(types);
    let clock = super::super::records::operation_clock(collection);
    let mut refinished = BTreeMap::<String, Refinished>::new();
    let mut deleted = Vec::new();
    for path in written {
        let governed = collection.root.join(path).is_file()
            && is_mdbase_record_path(collection, path).unwrap_or(false);
        if !governed {
            if previous.contains_key(path) {
                deleted.push(path.clone());
            }
            continue;
        }
        let (source, metadata) = read_record_source_stably(collection, path)?;
        let Some(fingerprint) = stat_fingerprint(&metadata) else {
            return Ok(None);
        };
        let record = super::super::records::build_mdbase_record(
            collection,
            types,
            path,
            source,
            Some(&metadata),
            false,
            &clock,
        );
        let identity = record_identity(collection, &unique_fields, &record);
        let body_facts = super::super::links::BodyLinkFacts::parse(&record.body);
        refinished.insert(
            path.clone(),
            Refinished {
                local: record,
                body_facts,
                identity,
                fingerprint,
                written: true,
                published_evidence: None,
            },
        );
    }

    // Keys whose holders may change: every old and new identity of a
    // written record.
    let mut changed_keys = LinkLookupKeys::default();
    let mut changed_values = BTreeMap::<String, BTreeSet<String>>::new();
    let identities = previous
        .iter()
        .map(|(path, state)| (path, &state.identity))
        .chain(
            refinished
                .iter()
                .map(|(path, record)| (path, &record.identity)),
        );
    for (path, identity) in identities {
        changed_keys.paths.insert(path.clone());
        changed_keys.basenames.insert(identity.basename.clone());
        if let Some(id) = &identity.id {
            changed_keys.ids.insert(id.clone());
        }
        for (field, values) in &identity.unique {
            changed_values
                .entry(field.clone())
                .or_default()
                .extend(values.iter().map(uniqueness_key));
        }
    }
    let written_set = written.iter().cloned().collect::<BTreeSet<_>>();
    let mut affected = reverse_holders(connection, &root, &changed_keys, &changed_values)?;
    affected.retain(|path| !written_set.contains(path));

    // Re-finish affected records from their published local derivations.
    let affected_state = load_query_state(
        connection,
        &root,
        &digest,
        &affected.iter().cloned().collect::<Vec<_>>(),
    )?;
    for path in &affected {
        let Some(state) = affected_state.get(path) else {
            return Ok(None);
        };
        let Some((local, body_facts)) = load_local_snapshot(connection, &root, &digest, path)?
            .map(LocalOverlaySnapshot::into_parts)
            .filter(|(local, _)| local.revision == state.revision)
        else {
            return Ok(None);
        };
        refinished.insert(
            path.clone(),
            Refinished {
                local,
                body_facts,
                identity: state.identity.clone(),
                fingerprint: state.fingerprint,
                written: false,
                published_evidence: Some(state.evidence),
            },
        );
    }

    // Uniqueness: each owner's diagnostics involve only records sharing one
    // of its values, so the group is the owners plus every such holder.
    let mut owner_values = BTreeMap::<String, BTreeSet<String>>::new();
    for record in refinished.values() {
        for (field, values) in &record.identity.unique {
            owner_values
                .entry(field.clone())
                .or_default()
                .extend(values.iter().map(uniqueness_key));
        }
    }
    let mut holders =
        reverse_holders(connection, &root, &LinkLookupKeys::default(), &owner_values)?;
    holders.retain(|path| !written_set.contains(path) && !refinished.contains_key(path));
    let holder_state = load_query_state(
        connection,
        &root,
        &digest,
        &holders.iter().cloned().collect::<Vec<_>>(),
    )?;
    if holder_state.len() != holders.len() {
        return Ok(None);
    }
    let mut group = holder_state
        .into_iter()
        .map(|(path, state)| (path, state.identity))
        .collect::<BTreeMap<_, _>>();
    group.extend(
        refinished
            .iter()
            .map(|(path, record)| (path.clone(), record.identity.clone())),
    );
    let owners = refinished.keys().cloned().collect::<BTreeSet<_>>();
    let mut uniqueness = uniqueness_diagnostics_from_identities(collection, types, &group, &owners);

    // Links: the records holding every key the re-finished records consult,
    // with written records at their new identities.
    let mut lookup = LinkLookupKeys::default();
    for record in refinished.values() {
        let keys =
            super::super::links::record_link_lookup_keys(types, &record.local, &record.body_facts);
        lookup.paths.extend(keys.paths);
        lookup.basenames.extend(keys.basenames);
        lookup.ids.extend(keys.ids);
    }
    let mut targets = BTreeMap::<String, MdbaseRecordIdentity>::new();
    for keys in [
        IdentityLookup::Paths(&lookup.paths.iter().cloned().collect::<Vec<_>>()),
        IdentityLookup::Basenames(&lookup.basenames.iter().cloned().collect::<Vec<_>>()),
        IdentityLookup::Ids(&lookup.ids.iter().cloned().collect::<Vec<_>>()),
    ] {
        let Some(found) = load_current_identities(connection, &root, &digest, &keys)? else {
            return Ok(None);
        };
        for (path, identity) in found {
            let Ok(identity) = serde_json::from_str::<MdbaseRecordIdentity>(&identity) else {
                return Ok(None);
            };
            targets.insert(path, identity);
        }
    }
    targets.retain(|path, _| !written_set.contains(path));
    targets.extend(
        refinished
            .iter()
            .filter(|(_, record)| record.written)
            .map(|(path, record)| (path.clone(), record.identity.clone())),
    );
    let mut index = LinkTargetIndex::default();
    for (path, identity) in &targets {
        index.insert(
            path,
            &identity.types,
            &identity.basename,
            identity.id.as_deref(),
        );
    }

    let mut next =
        BTreeMap::<String, (MdbaseCachedRecord, super::super::MdbaseQueryInputEvidence)>::new();
    for (path, record) in &refinished {
        let finished = finish_identity_stable_record(
            collection,
            types,
            contracts,
            record.local.clone(),
            uniqueness.remove(path).unwrap_or_default(),
            &record.body_facts,
            &index,
        );
        let evidence = record
            .published_evidence
            .unwrap_or_else(|| mdbase_query_input_evidence(&finished, types));
        next.insert(
            path.clone(),
            (cached_record(&root, &digest, finished), evidence),
        );
    }

    // Bind publication to what was read: written records by bytes, other
    // re-finished records by their published fingerprint, deletions by
    // absence, and the controls once more.
    for (path, record) in &refinished {
        if record.written {
            let (source, metadata) = read_record_source_stably(collection, path)
                .map_err(|_| MdbaseRecordCacheError::StaleRecords)?;
            if crate::mdbase::mdbase_content_revision(&source) != record.local.revision
                || stat_fingerprint(&metadata) != Some(record.fingerprint)
            {
                return Err(MdbaseRecordCacheError::StaleRecords);
            }
        } else {
            let metadata = std::fs::metadata(collection.root.join(path))
                .map_err(|_| MdbaseRecordCacheError::StaleRecords)?;
            if stat_fingerprint(&metadata) != Some(record.fingerprint) {
                return Err(MdbaseRecordCacheError::StaleRecords);
            }
        }
    }
    if deleted
        .iter()
        .any(|path| collection.root.join(path).is_file())
    {
        return Err(MdbaseRecordCacheError::StaleRecords);
    }
    if verify_mdbase_control_snapshots(collection, types, contracts, None)? != controls {
        return Err(MdbaseRecordCacheError::StaleControls);
    }

    let added = refinished
        .iter()
        .filter(|(path, record)| record.written && !previous.contains_key(*path))
        .count();
    let written_count = refinished.values().filter(|record| record.written).count();
    database.with_transaction(|transaction| {
        for path in &deleted {
            transaction.execute(
                "DELETE FROM mdbase_record_cache WHERE collection_root = ?1 AND path = ?2",
                params![root, path],
            )?;
        }
        for (path, (cached, evidence)) in &next {
            let record = &refinished[path];
            store_cached_record(transaction, cached)?;
            if record.written {
                let local = serde_json::to_string(&LocalRecordSnapshot {
                    record: record.local.clone(),
                    body_facts: record.body_facts.clone(),
                })?;
                transaction.execute(
                    "UPDATE mdbase_record_cache SET local_record_json = ?3
                     WHERE collection_root = ?1 AND path = ?2
                       AND local_record_json IS NOT ?3",
                    params![root, path, local],
                )?;
            }
            store_query_row(
                transaction,
                cached,
                Some(record.fingerprint),
                evidence,
                &serde_json::to_string(&record.identity)?,
            )?;
            let keys = super::super::links::record_link_lookup_keys(
                types,
                &record.local,
                &record.body_facts,
            );
            store_reverse_keys(transaction, &root, path, &keys, &record.identity)?;
        }
        Ok::<_, MdbaseRecordCacheError>(())
    })?;
    Ok(Some(MdbaseRecordCacheRefresh {
        dependency_digest: digest,
        dependency_changed: false,
        added,
        updated: next.len() - added,
        unchanged: 0,
        deleted: deleted.len(),
        local_records_derived: written_count,
        local_records_reused: next.len() - written_count,
        overlays_scoped: true,
    }))
}

/// Every query row of the collection is current and carries reverse keys.
fn reverse_keys_cover_every_row(
    connection: &Connection,
    root: &str,
    digest: &str,
) -> Result<bool, MdbaseRecordCacheError> {
    // Each probe is an index seek: the partial index of unmarked rows, and
    // ranges of the freshness index on either side of the current digest
    // and model version.
    let uncovered: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM mdbase_record_query
                WHERE collection_root = ?1 AND reverse_keys IS NOT 1)
             OR EXISTS(SELECT 1 FROM mdbase_record_query
                WHERE collection_root = ?1 AND dependency_digest < ?2)
             OR EXISTS(SELECT 1 FROM mdbase_record_query
                WHERE collection_root = ?1 AND dependency_digest > ?2)
             OR EXISTS(SELECT 1 FROM mdbase_record_query
                WHERE collection_root = ?1 AND dependency_digest = ?2
                  AND record_model_version <> ?3)",
        params![root, digest, MDBASE_RECORD_MODEL_VERSION],
        |row| row.get(0),
    )?;
    Ok(!uncovered)
}

struct QueryState {
    revision: String,
    fingerprint: MdbaseStatFingerprint,
    identity: MdbaseRecordIdentity,
    evidence: super::super::MdbaseQueryInputEvidence,
}

/// Current, fingerprinted query rows with identities for `paths`; rows that
/// are stale or lack identities are omitted.
fn load_query_state(
    connection: &Connection,
    root: &str,
    digest: &str,
    paths: &[String],
) -> Result<BTreeMap<String, QueryState>, MdbaseRecordCacheError> {
    if paths.is_empty() {
        return Ok(BTreeMap::new());
    }
    let mut statement = connection.prepare_cached(
        "SELECT path, revision, stat_fingerprint, identity_json, input_converted, input_bytes,
                input_nodes, input_width, input_links FROM mdbase_record_query
         WHERE collection_root = ?1 AND path IN (SELECT value FROM json_each(?2))
           AND dependency_digest = ?3 AND record_model_version = ?4
           AND stat_fingerprint IS NOT NULL AND identity_json IS NOT NULL",
    )?;
    let rows = statement.query_map(
        params![
            root,
            serde_json::to_string(paths)?,
            digest,
            MDBASE_RECORD_MODEL_VERSION
        ],
        |row| {
            let count = |index: usize| {
                row.get::<_, i64>(index)
                    .map(|value| usize::try_from(value).unwrap_or(usize::MAX))
            };
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, MdbaseStatFingerprint>(2)?,
                row.get::<_, String>(3)?,
                super::super::MdbaseQueryInputEvidence {
                    converted: row.get::<_, i64>(4)? != 0,
                    bytes: count(5)?,
                    nodes: count(6)?,
                    width: count(7)?,
                    links: count(8)?,
                },
            ))
        },
    )?;
    let mut state = BTreeMap::new();
    for row in rows {
        let (path, revision, fingerprint, identity, evidence) = row?;
        state.insert(
            path,
            QueryState {
                revision,
                fingerprint,
                identity: serde_json::from_str(&identity)?,
                evidence,
            },
        );
    }
    Ok(state)
}

/// Paths whose reverse keys meet `keys` or hold one of `values` (keyed by
/// field, as uniqueness keys).
fn reverse_holders(
    connection: &Connection,
    root: &str,
    keys: &LinkLookupKeys,
    values: &BTreeMap<String, BTreeSet<String>>,
) -> Result<BTreeSet<String>, MdbaseRecordCacheError> {
    let mut statement = connection.prepare_cached(
        "SELECT path FROM mdbase_record_reverse_keys
         WHERE collection_root = ?1 AND kind = ?2 AND key IN (SELECT value FROM json_each(?3))",
    )?;
    let mut holders = BTreeSet::new();
    let lookups = [
        ("path".to_string(), &keys.paths),
        ("basename".to_string(), &keys.basenames),
        ("id".to_string(), &keys.ids),
    ]
    .into_iter()
    .chain(
        values
            .iter()
            .map(|(field, values)| (format!("unique:{field}"), values)),
    );
    for (kind, keys) in lookups {
        if keys.is_empty() {
            continue;
        }
        let rows = statement
            .query_map(params![root, kind, serde_json::to_string(keys)?], |row| {
                row.get::<_, String>(0)
            })?;
        for row in rows {
            holders.insert(row?);
        }
    }
    Ok(holders)
}

/// A record's published local derivation, without body or source.
fn load_local_snapshot(
    connection: &Connection,
    root: &str,
    digest: &str,
    path: &str,
) -> Result<Option<LocalOverlaySnapshot>, MdbaseRecordCacheError> {
    let mut statement = connection.prepare_cached(
        "SELECT cache.local_record_json
         FROM mdbase_record_query AS query
         JOIN mdbase_record_cache AS cache
           ON cache.collection_root = query.collection_root AND cache.path = query.path
          AND cache.revision = query.revision
         WHERE query.collection_root = ?1 AND query.path = ?2 AND query.dependency_digest = ?3
           AND query.record_model_version = ?4 AND cache.local_record_json IS NOT NULL",
    )?;
    let json = statement
        .query_row(
            params![root, path, digest, MDBASE_RECORD_MODEL_VERSION],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    Ok(json.map(|json| serde_json::from_str(&json)).transpose()?)
}
