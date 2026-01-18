//! Schema management for SingleFileDB
//!
//! Handles label, edge type, and property key definitions and lookups.

use crate::types::*;

use super::SingleFileDB;

impl SingleFileDB {
  /// Get or create a label ID by name
  pub fn get_or_create_label(&self, name: &str) -> LabelId {
    {
      let names = self.label_names.read();
      if let Some(&id) = names.get(name) {
        return id;
      }
    }

    let id = self.alloc_label_id();
    {
      let mut names = self.label_names.write();
      let mut ids = self.label_ids.write();
      if let Some(&existing) = names.get(name) {
        return existing;
      }
      names.insert(name.to_string(), id);
      ids.insert(id, name.to_string());
    }
    id
  }

  /// Get label ID by name
  pub fn get_label_id(&self, name: &str) -> Option<LabelId> {
    self.label_names.read().get(name).copied()
  }

  /// Get label name by ID
  pub fn get_label_name(&self, id: LabelId) -> Option<String> {
    self.label_ids.read().get(&id).cloned()
  }

  /// Get or create an edge type ID by name
  pub fn get_or_create_etype(&self, name: &str) -> ETypeId {
    {
      let names = self.etype_names.read();
      if let Some(&id) = names.get(name) {
        return id;
      }
    }

    let id = self.alloc_etype_id();
    {
      let mut names = self.etype_names.write();
      let mut ids = self.etype_ids.write();
      if let Some(&existing) = names.get(name) {
        return existing;
      }
      names.insert(name.to_string(), id);
      ids.insert(id, name.to_string());
    }
    id
  }

  /// Get edge type ID by name
  pub fn get_etype_id(&self, name: &str) -> Option<ETypeId> {
    self.etype_names.read().get(name).copied()
  }

  /// Get edge type name by ID
  pub fn get_etype_name(&self, id: ETypeId) -> Option<String> {
    self.etype_ids.read().get(&id).cloned()
  }

  /// Get or create a property key ID by name
  pub fn get_or_create_propkey(&self, name: &str) -> PropKeyId {
    {
      let names = self.propkey_names.read();
      if let Some(&id) = names.get(name) {
        return id;
      }
    }

    let id = self.alloc_propkey_id();
    {
      let mut names = self.propkey_names.write();
      let mut ids = self.propkey_ids.write();
      if let Some(&existing) = names.get(name) {
        return existing;
      }
      names.insert(name.to_string(), id);
      ids.insert(id, name.to_string());
    }
    id
  }

  /// Get property key ID by name
  pub fn get_propkey_id(&self, name: &str) -> Option<PropKeyId> {
    self.propkey_names.read().get(name).copied()
  }

  /// Get property key name by ID
  pub fn get_propkey_name(&self, id: PropKeyId) -> Option<String> {
    self.propkey_ids.read().get(&id).cloned()
  }
}
