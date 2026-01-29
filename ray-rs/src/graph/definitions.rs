//! Schema definitions (labels, etypes, propkeys)
//!
//! Provides functions for defining and looking up schema elements like
//! labels (node types), edge types, and property keys.

use crate::error::Result;
use crate::types::*;

use super::db::GraphDB;
use super::tx::TxHandle;

// ============================================================================
// Label Operations
// ============================================================================

/// Define a new label (node type)
/// Returns an existing ID if the label was already defined
pub fn define_label(handle: &mut TxHandle, name: &str) -> Result<LabelId> {
  if let Some(existing) = handle.db.get_label_id(name) {
    return Ok(existing);
  }

  for (existing_id, existing_name) in &handle.tx.pending_new_labels {
    if existing_name == name {
      return Ok(*existing_id);
    }
  }

  let label_id = handle.db.alloc_label_id();
  handle
    .tx
    .pending_new_labels
    .insert(label_id, name.to_string());
  Ok(label_id)
}

/// Get label ID by name (returns None if not defined)
pub fn get_label_id(db: &GraphDB, name: &str) -> Option<LabelId> {
  db.get_label_id(name)
}

/// Get label name by ID (returns None if not found)
pub fn get_label_name(db: &GraphDB, id: LabelId) -> Option<String> {
  db.get_label_name(id)
}

// ============================================================================
// Edge Type Operations
// ============================================================================

/// Define a new edge type
/// Returns an existing ID if the edge type was already defined
pub fn define_etype(handle: &mut TxHandle, name: &str) -> Result<ETypeId> {
  if let Some(existing) = handle.db.get_etype_id(name) {
    return Ok(existing);
  }

  for (existing_id, existing_name) in &handle.tx.pending_new_etypes {
    if existing_name == name {
      return Ok(*existing_id);
    }
  }

  let etype_id = handle.db.alloc_etype_id();
  handle
    .tx
    .pending_new_etypes
    .insert(etype_id, name.to_string());
  Ok(etype_id)
}

/// Get edge type ID by name
pub fn get_etype_id(db: &GraphDB, name: &str) -> Option<ETypeId> {
  db.get_etype_id(name)
}

/// Get edge type name by ID
pub fn get_etype_name(db: &GraphDB, id: ETypeId) -> Option<String> {
  db.get_etype_name(id)
}

// ============================================================================
// Property Key Operations
// ============================================================================

/// Define a new property key
/// Returns an existing ID if the property key was already defined
pub fn define_propkey(handle: &mut TxHandle, name: &str) -> Result<PropKeyId> {
  if let Some(existing) = handle.db.get_propkey_id(name) {
    return Ok(existing);
  }

  for (existing_id, existing_name) in &handle.tx.pending_new_propkeys {
    if existing_name == name {
      return Ok(*existing_id);
    }
  }

  let propkey_id = handle.db.alloc_propkey_id();
  handle
    .tx
    .pending_new_propkeys
    .insert(propkey_id, name.to_string());
  Ok(propkey_id)
}

/// Get property key ID by name
pub fn get_propkey_id(db: &GraphDB, name: &str) -> Option<PropKeyId> {
  db.get_propkey_id(name)
}

/// Get property key name by ID
pub fn get_propkey_name(db: &GraphDB, id: PropKeyId) -> Option<String> {
  db.get_propkey_name(id)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
  use super::*;
  use crate::graph::db::{close_graph_db, open_graph_db, OpenOptions};
  use crate::graph::tx::{begin_tx, commit};
  use tempfile::tempdir;

  #[test]
  fn test_define_label() {
    let temp_dir = tempdir().unwrap();
    let db = open_graph_db(temp_dir.path(), OpenOptions::new()).unwrap();

    let mut tx = begin_tx(&db).unwrap();

    let person = define_label(&mut tx, "Person").unwrap();
    let company = define_label(&mut tx, "Company").unwrap();

    assert!(person >= 1);
    assert!(company >= 1);
    assert_ne!(person, company);

    // Defining same label again should return same ID
    let person2 = define_label(&mut tx, "Person").unwrap();
    assert_eq!(person, person2);

    commit(&mut tx).unwrap();
    close_graph_db(db).unwrap();
  }

  #[test]
  fn test_define_etype() {
    let temp_dir = tempdir().unwrap();
    let db = open_graph_db(temp_dir.path(), OpenOptions::new()).unwrap();

    let mut tx = begin_tx(&db).unwrap();

    let knows = define_etype(&mut tx, "KNOWS").unwrap();
    let follows = define_etype(&mut tx, "FOLLOWS").unwrap();

    assert!(knows >= 1);
    assert!(follows >= 1);
    assert_ne!(knows, follows);

    commit(&mut tx).unwrap();
    close_graph_db(db).unwrap();
  }

  #[test]
  fn test_define_propkey() {
    let temp_dir = tempdir().unwrap();
    let db = open_graph_db(temp_dir.path(), OpenOptions::new()).unwrap();

    let mut tx = begin_tx(&db).unwrap();

    let name_key = define_propkey(&mut tx, "name").unwrap();
    let age_key = define_propkey(&mut tx, "age").unwrap();

    assert!(name_key >= 1);
    assert!(age_key >= 1);
    assert_ne!(name_key, age_key);

    commit(&mut tx).unwrap();
    close_graph_db(db).unwrap();
  }

  #[test]
  fn test_lookup_definitions() {
    let temp_dir = tempdir().unwrap();
    let db = open_graph_db(temp_dir.path(), OpenOptions::new()).unwrap();

    let mut tx = begin_tx(&db).unwrap();

    let person = define_label(&mut tx, "Person").unwrap();
    let knows = define_etype(&mut tx, "KNOWS").unwrap();
    let name_key = define_propkey(&mut tx, "name").unwrap();

    commit(&mut tx).unwrap();

    // Lookup by name
    assert_eq!(get_label_id(&db, "Person"), Some(person));
    assert_eq!(get_etype_id(&db, "KNOWS"), Some(knows));
    assert_eq!(get_propkey_id(&db, "name"), Some(name_key));

    // Lookup by ID
    assert_eq!(get_label_name(&db, person), Some("Person".to_string()));
    assert_eq!(get_etype_name(&db, knows), Some("KNOWS".to_string()));
    assert_eq!(get_propkey_name(&db, name_key), Some("name".to_string()));

    // Non-existent
    assert_eq!(get_label_id(&db, "Unknown"), None);
    assert_eq!(get_label_name(&db, 9999), None);

    close_graph_db(db).unwrap();
  }
}
