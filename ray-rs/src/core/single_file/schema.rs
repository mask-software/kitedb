//! Schema management for SingleFileDB
//!
//! Handles label, edge type, and property key definitions and lookups.
//!
//! Schema maps are intentionally not part of the MVCC version chain. A
//! transaction gets read-your-writes through its local staging overlay; other
//! transactions see only the last committed global mapping.

use std::sync::atomic::Ordering;

use crate::core::wal::record::{
  build_define_etype_payload, build_define_label_payload, build_define_propkey_payload, WalRecord,
};
use crate::error::{KiteError, Result};
use crate::types::*;

use super::SingleFileDB;

/// Schema namespaces that share the explicit-ID define path.
#[derive(Debug, Clone, Copy)]
enum SchemaKind {
  Label,
  EdgeType,
  PropertyKey,
}

impl SchemaKind {
  fn noun(self) -> &'static str {
    match self {
      SchemaKind::Label => "label",
      SchemaKind::EdgeType => "edge type",
      SchemaKind::PropertyKey => "property key",
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

  /// Define a label under the ID the replication primary assigned to it.
  pub(crate) fn define_label_with_id(&self, id: LabelId, name: &str) -> Result<()> {
    self.define_schema_with_id(SchemaKind::Label, id, name)
  }

  /// Define an edge type under the ID the replication primary assigned to it.
  pub(crate) fn define_etype_with_id(&self, id: ETypeId, name: &str) -> Result<()> {
    self.define_schema_with_id(SchemaKind::EdgeType, id, name)
  }

  /// Define a property key under the ID the replication primary assigned to it.
  pub(crate) fn define_propkey_with_id(&self, id: PropKeyId, name: &str) -> Result<()> {
    self.define_schema_with_id(SchemaKind::PropertyKey, id, name)
  }

  /// Mutation records address schema by numeric ID, so a replica must hold
  /// the primary's exact name/ID pairs. Re-defining an identical pair is a
  /// no-op. A name or ID already bound differently here (committed, staged,
  /// or claimed by an in-flight local define) is a divergence that only a
  /// reseed into a fresh replica database can repair.
  fn define_schema_with_id(&self, kind: SchemaKind, id: u32, name: &str) -> Result<()> {
    let (txid, tx_handle) = self.require_write_tx_handle()?;

    let (bound_id, bound_name) = match kind {
      SchemaKind::Label => (self.label_id(name), self.label_name(id)),
      SchemaKind::EdgeType => (self.etype_id(name), self.etype_name(id)),
      SchemaKind::PropertyKey => (self.propkey_id(name), self.propkey_name(id)),
    };
    match (bound_id, bound_name.as_deref()) {
      (Some(bound_id), Some(bound_name)) if bound_id == id && bound_name == name => return Ok(()),
      (None, None) => {}
      (bound_id, bound_name) => {
        let detail = match (bound_id.filter(|&bound| bound != id), bound_name) {
          (Some(bound_id), _) => format!("{name:?} is already id {bound_id} here"),
          (None, Some(bound_name)) if bound_name != name => {
            format!("id {id} is already {bound_name:?} here")
          }
          _ => "this database maps only one side of the pair".to_string(),
        };
        return Err(schema_divergence(kind, id, name, &detail));
      }
    }

    {
      let reservations = self.schema_reservations.lock();
      let claims = match kind {
        SchemaKind::Label => &reservations.labels,
        SchemaKind::EdgeType => &reservations.etypes,
        SchemaKind::PropertyKey => &reservations.propkeys,
      };
      if let Some((claimed_name, claim)) = claims
        .iter()
        .find(|(claimed_name, claim)| (claimed_name.as_str() == name) != (claim.id == id))
      {
        return Err(schema_divergence(
          kind,
          id,
          name,
          &format!(
            "an in-flight local transaction claims {claimed_name:?} as id {}",
            claim.id
          ),
        ));
      }
    }

    let (record_type, payload) = match kind {
      SchemaKind::Label => (
        WalRecordType::DefineLabel,
        build_define_label_payload(id, name),
      ),
      SchemaKind::EdgeType => (
        WalRecordType::DefineEtype,
        build_define_etype_payload(id, name),
      ),
      SchemaKind::PropertyKey => (
        WalRecordType::DefinePropkey,
        build_define_propkey_payload(id, name),
      ),
    };
    self.write_wal_tx(&tx_handle, WalRecord::new(record_type, txid, payload))?;

    // Stage like a local define; commit publishes the pair. Raising the
    // allocator keeps later local allocations clear of the primary's IDs.
    let next_id = id.saturating_add(1);
    let mut tx = tx_handle.lock();
    match kind {
      SchemaKind::Label => {
        tx.schema.define_label(id, name);
        tx.pending.define_label(id, name);
        self.next_label_id.fetch_max(next_id, Ordering::SeqCst);
      }
      SchemaKind::EdgeType => {
        tx.schema.define_etype(id, name);
        tx.pending.define_etype(id, name);
        self.next_etype_id.fetch_max(next_id, Ordering::SeqCst);
      }
      SchemaKind::PropertyKey => {
        tx.schema.define_propkey(id, name);
        tx.pending.define_propkey(id, name);
        self.next_propkey_id.fetch_max(next_id, Ordering::SeqCst);
      }
    }
    Ok(())
  }
}

fn schema_divergence(kind: SchemaKind, id: u32, name: &str, detail: &str) -> KiteError {
  KiteError::InvalidReplication(format!(
    "replica schema diverged from the primary: {} {name:?} must have id {id}, but {detail}; \
     reseed into a fresh replica database",
    kind.noun()
  ))
}
