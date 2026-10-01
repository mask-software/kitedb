//! Schema management for SingleFileDB
//!
//! Handles label, edge type, and property key definitions and lookups.
//!
//! Schema maps are intentionally not part of the MVCC version chain. A
//! transaction gets read-your-writes through its local staging overlay; other
//! transactions see only the last committed global mapping.

use std::collections::HashMap;
use std::sync::atomic::Ordering;

use crate::core::wal::record::{
  build_define_etype_payload, build_define_label_payload, build_define_propkey_payload, WalRecord,
};
use crate::error::Result;
use crate::types::*;

use super::{SchemaReservation, SingleFileDB};

/// Schema namespaces that share the replica define path.
#[derive(Debug, Clone, Copy)]
enum SchemaKind {
  Label,
  EdgeType,
  PropertyKey,
}

impl SchemaKind {
  fn define_record_type(self) -> WalRecordType {
    match self {
      SchemaKind::Label => WalRecordType::DefineLabel,
      SchemaKind::EdgeType => WalRecordType::DefineEtype,
      SchemaKind::PropertyKey => WalRecordType::DefinePropkey,
    }
  }

  fn define_payload(self, id: u32, name: &str) -> Vec<u8> {
    match self {
      SchemaKind::Label => build_define_label_payload(id, name),
      SchemaKind::EdgeType => build_define_etype_payload(id, name),
      SchemaKind::PropertyKey => build_define_propkey_payload(id, name),
    }
  }
}

impl SingleFileDB {
  /// Ensure a schema entry exists, opening an implicit write transaction when
  /// the caller is not already inside one.
  fn ensure_schema_entry<T, F>(&self, existing: Option<T>, define: F) -> Result<T>
  where
    F: FnOnce(&Self) -> Result<T>,
  {
    if let Some(id) = existing {
      return Ok(id);
    }

    if self.has_transaction() {
      return define(self);
    }

    self.begin(false)?;
    match define(self) {
      Ok(id) => {
        self.commit()?;
        Ok(id)
      }
      Err(err) => {
        let _ = self.rollback();
        Err(err)
      }
    }
  }

  /// Ensure a label ID exists and WAL-log its creation when needed.
  pub(crate) fn ensure_label(&self, name: &str) -> Result<LabelId> {
    self.ensure_schema_entry(self.label_id(name), |db| db.define_label(name))
  }

  /// Get label ID by name
  pub fn label_id(&self, name: &str) -> Option<LabelId> {
    if let Some(handle) = self.current_tx_handle() {
      let tx = handle.lock();
      if let Some(id) = tx.schema.label_id(name) {
        return Some(id);
      }
    }
    self.label_names.read().get(name).copied()
  }

  /// Get label name by ID
  pub fn label_name(&self, id: LabelId) -> Option<String> {
    if let Some(handle) = self.current_tx_handle() {
      let tx = handle.lock();
      if let Some(name) = tx.schema.label_name(id) {
        return Some(name);
      }
    }
    self.label_ids.read().get(&id).cloned()
  }

  /// Ensure an edge type ID exists and WAL-log its creation when needed.
  pub(crate) fn ensure_etype(&self, name: &str) -> Result<ETypeId> {
    self.ensure_schema_entry(self.etype_id(name), |db| db.define_etype(name))
  }

  /// Get edge type ID by name
  pub fn etype_id(&self, name: &str) -> Option<ETypeId> {
    if let Some(handle) = self.current_tx_handle() {
      let tx = handle.lock();
      if let Some(id) = tx.schema.etype_id(name) {
        return Some(id);
      }
    }
    self.etype_names.read().get(name).copied()
  }

  /// Get edge type name by ID
  pub fn etype_name(&self, id: ETypeId) -> Option<String> {
    if let Some(handle) = self.current_tx_handle() {
      let tx = handle.lock();
      if let Some(name) = tx.schema.etype_name(id) {
        return Some(name);
      }
    }
    self.etype_ids.read().get(&id).cloned()
  }

  /// Ensure a property key ID exists and WAL-log its creation when needed.
  pub(crate) fn ensure_propkey(&self, name: &str) -> Result<PropKeyId> {
    self.ensure_schema_entry(self.propkey_id(name), |db| db.define_propkey(name))
  }

  /// Get property key ID by name
  pub fn propkey_id(&self, name: &str) -> Option<PropKeyId> {
    if let Some(handle) = self.current_tx_handle() {
      let tx = handle.lock();
      if let Some(id) = tx.schema.propkey_id(name) {
        return Some(id);
      }
    }
    self.propkey_names.read().get(name).copied()
  }

  /// Get property key name by ID
  pub fn propkey_name(&self, id: PropKeyId) -> Option<String> {
    if let Some(handle) = self.current_tx_handle() {
      let tx = handle.lock();
      if let Some(name) = tx.schema.propkey_name(id) {
        return Some(name);
      }
    }
    self.propkey_ids.read().get(&id).cloned()
  }

  /// Local label id for a name the replication primary defined as
  /// `primary_id`; see `ensure_replica_schema`.
  pub(crate) fn ensure_replica_label(&self, name: &str, primary_id: LabelId) -> Result<LabelId> {
    self.ensure_replica_schema(SchemaKind::Label, name, primary_id)
  }

  /// Local edge type id for a name the replication primary defined as
  /// `primary_id`; see `ensure_replica_schema`.
  pub(crate) fn ensure_replica_etype(&self, name: &str, primary_id: ETypeId) -> Result<ETypeId> {
    self.ensure_replica_schema(SchemaKind::EdgeType, name, primary_id)
  }

  /// Local property key id for a name the replication primary defined as
  /// `primary_id`; see `ensure_replica_schema`.
  pub(crate) fn ensure_replica_propkey(
    &self,
    name: &str,
    primary_id: PropKeyId,
  ) -> Result<PropKeyId> {
    self.ensure_replica_schema(SchemaKind::PropertyKey, name, primary_id)
  }

  /// A replica keeps its own schema ids (an application may define names
  /// before the first pull) and translates the primary's ids by name. An
  /// existing local id for the name wins. Otherwise the name is defined here,
  /// under the primary's id when that id is free, so a replica without schema
  /// of its own mirrors the primary's ids exactly.
  fn ensure_replica_schema(&self, kind: SchemaKind, name: &str, primary_id: u32) -> Result<u32> {
    let (txid, tx_handle) = self.require_write_tx_handle()?;
    let staged_or_committed = match kind {
      SchemaKind::Label => self.label_id(name),
      SchemaKind::EdgeType => self.etype_id(name),
      SchemaKind::PropertyKey => self.propkey_id(name),
    };
    if let Some(id) = staged_or_committed {
      return Ok(id);
    }

    // Claim under the reservation lock, like a local define, so concurrent
    // local transactions defining the same name share this id.
    let local_id = {
      let mut reservations = self.schema_reservations.lock();
      let (committed_id, primary_id_committed, claims) = match kind {
        SchemaKind::Label => (
          self.label_names.read().get(name).copied(),
          self.label_ids.read().contains_key(&primary_id),
          &mut reservations.labels,
        ),
        SchemaKind::EdgeType => (
          self.etype_names.read().get(name).copied(),
          self.etype_ids.read().contains_key(&primary_id),
          &mut reservations.etypes,
        ),
        SchemaKind::PropertyKey => (
          self.propkey_names.read().get(name).copied(),
          self.propkey_ids.read().contains_key(&primary_id),
          &mut reservations.propkeys,
        ),
      };
      if let Some(id) = committed_id {
        return Ok(id);
      }
      if let Some(claim) = claims.get_mut(name) {
        claim.owners.insert(txid);
        claim.id
      } else {
        let primary_id_free =
          !primary_id_committed && !claims.values().any(|claim| claim.id == primary_id);
        let id = if primary_id_free {
          // Keep later allocations clear of the adopted id.
          let allocator = match kind {
            SchemaKind::Label => &self.next_label_id,
            SchemaKind::EdgeType => &self.next_etype_id,
            SchemaKind::PropertyKey => &self.next_propkey_id,
          };
          allocator.fetch_max(primary_id.saturating_add(1), Ordering::SeqCst);
          primary_id
        } else {
          match kind {
            SchemaKind::Label => self.alloc_unclaimed_label_id(),
            SchemaKind::EdgeType => self.alloc_unclaimed_etype_id(),
            SchemaKind::PropertyKey => self.alloc_unclaimed_propkey_id(),
          }
        };
        claims.insert(name.to_string(), SchemaReservation::new(id, txid));
        id
      }
    };

    let record = WalRecord::new(
      kind.define_record_type(),
      txid,
      kind.define_payload(local_id, name),
    );
    if let Err(error) = self.write_wal_tx(&tx_handle, record) {
      match kind {
        SchemaKind::Label => self.release_label_reservation(name, txid),
        SchemaKind::EdgeType => self.release_etype_reservation(name, txid),
        SchemaKind::PropertyKey => self.release_propkey_reservation(name, txid),
      }
      return Err(error);
    }

    let mut tx = tx_handle.lock();
    match kind {
      SchemaKind::Label => {
        tx.schema.define_label(local_id, name);
        tx.pending.define_label(local_id, name);
      }
      SchemaKind::EdgeType => {
        tx.schema.define_etype(local_id, name);
        tx.pending.define_etype(local_id, name);
      }
      SchemaKind::PropertyKey => {
        tx.schema.define_propkey(local_id, name);
        tx.pending.define_propkey(local_id, name);
      }
    }
    Ok(local_id)
  }

  /// WAL-log every committed schema name/id pair in the current transaction.
  /// A promoted primary uses this to announce its whole schema in the new
  /// epoch; replaying these records is a no-op.
  pub(crate) fn log_committed_schema(&self) -> Result<()> {
    let (txid, tx_handle) = self.require_write_tx_handle()?;
    let entries = [
      (SchemaKind::Label, sorted_entries(&self.label_ids.read())),
      (SchemaKind::EdgeType, sorted_entries(&self.etype_ids.read())),
      (
        SchemaKind::PropertyKey,
        sorted_entries(&self.propkey_ids.read()),
      ),
    ];
    for (kind, pairs) in entries {
      for (id, name) in pairs {
        let record = WalRecord::new(
          kind.define_record_type(),
          txid,
          kind.define_payload(id, &name),
        );
        self.write_wal_tx(&tx_handle, record)?;
      }
    }
    Ok(())
  }
}

fn sorted_entries(ids: &HashMap<u32, String>) -> Vec<(u32, String)> {
  let mut entries: Vec<_> = ids.iter().map(|(&id, name)| (id, name.clone())).collect();
  entries.sort_unstable_by_key(|&(id, _)| id);
  entries
}
