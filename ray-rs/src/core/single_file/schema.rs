//! Schema management for SingleFileDB
//!
//! Handles label, edge type, and property key definitions and lookups.
//!
//! Schema maps are intentionally not part of the MVCC version chain. A
//! transaction gets read-your-writes through its local staging overlay; other
//! transactions see only the last committed global mapping.

use crate::error::Result;
use crate::types::*;

use super::SingleFileDB;

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
}
