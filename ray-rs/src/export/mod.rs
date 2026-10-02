//! Export and Import utilities
//!
//! JSON and JSONL export/import for SingleFileDB.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufReader, BufWriter, Write};
use std::path::Path;

use crate::core::single_file::SingleFileDB;
use crate::error::{KiteError, Result};
use crate::types::{ETypeId, LabelId, NodeId, PropKeyId, PropValue};

// =============================================================================
// Types
// =============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportOptions {
  pub include_nodes: bool,
  pub include_edges: bool,
  pub include_schema: bool,
  pub pretty: bool,
}

impl Default for ExportOptions {
  fn default() -> Self {
    Self {
      include_nodes: true,
      include_edges: true,
      include_schema: true,
      pretty: false,
    }
  }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportOptions {
  pub skip_existing: bool,
  pub batch_size: usize,
}

impl Default for ImportOptions {
  fn default() -> Self {
    Self {
      skip_existing: true,
      batch_size: 1000,
    }
  }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportedPropValue {
  pub r#type: String,
  pub value: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportedNode {
  pub id: u64,
  pub key: Option<String>,
  /// Label names, sorted. Exports written before labels were exported have
  /// none.
  #[serde(default)]
  pub labels: Vec<String>,
  pub props: HashMap<String, ExportedPropValue>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportedEdge {
  pub src: u64,
  pub dst: u64,
  pub etype: u32,
  pub etype_name: Option<String>,
  pub props: HashMap<String, ExportedPropValue>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ExportedSchema {
  pub labels: HashMap<u32, String>,
  pub etypes: HashMap<u32, String>,
  pub prop_keys: HashMap<u32, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportedDatabase {
  pub version: u32,
  pub exported_at: String,
  pub schema: ExportedSchema,
  pub nodes: Vec<ExportedNode>,
  pub edges: Vec<ExportedEdge>,
  pub stats: ExportStats,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportStats {
  pub node_count: usize,
  pub edge_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportResult {
  pub node_count: usize,
  pub edge_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportResult {
  pub node_count: usize,
  pub edge_count: usize,
  pub skipped: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonLine<T> {
  pub r#type: String,
  pub data: Option<T>,
}

// =============================================================================
// PropValue Serialization
// =============================================================================

fn serialize_prop_value(value: &PropValue) -> ExportedPropValue {
  match value {
    PropValue::Null => ExportedPropValue {
      r#type: "null".to_string(),
      value: serde_json::Value::Null,
    },
    PropValue::String(v) => ExportedPropValue {
      r#type: "string".to_string(),
      value: serde_json::Value::String(v.clone()),
    },
    PropValue::I64(v) => ExportedPropValue {
      r#type: "int".to_string(),
      value: serde_json::Value::Number((*v).into()),
    },
    PropValue::F64(v) => ExportedPropValue {
      r#type: "float".to_string(),
      value: serde_json::Value::Number(
        serde_json::Number::from_f64(*v).unwrap_or_else(|| 0.into()),
      ),
    },
    PropValue::Bool(v) => ExportedPropValue {
      r#type: "bool".to_string(),
      value: serde_json::Value::Bool(*v),
    },
    PropValue::VectorF32(v) => ExportedPropValue {
      r#type: "vector".to_string(),
      value: serde_json::Value::Array(
        v.iter()
          .map(|x| {
            serde_json::Value::Number(
              serde_json::Number::from_f64(*x as f64).unwrap_or_else(|| 0.into()),
            )
          })
          .collect(),
      ),
    },
  }
}

fn deserialize_prop_value(value: &ExportedPropValue) -> PropValue {
  match value.r#type.as_str() {
    "null" => PropValue::Null,
    "string" => PropValue::String(value.value.as_str().unwrap_or_default().to_string()),
    "int" => PropValue::I64(value.value.as_i64().unwrap_or_default()),
    "float" => PropValue::F64(value.value.as_f64().unwrap_or_default()),
    "bool" => PropValue::Bool(value.value.as_bool().unwrap_or(false)),
    "vector" => {
      let mut vec = Vec::new();
      if let Some(values) = value.value.as_array() {
        for v in values {
          vec.push(v.as_f64().unwrap_or_default() as f32);
        }
      }
      PropValue::VectorF32(vec)
    }
    _ => PropValue::Null,
  }
}

// =============================================================================
// Schema Helpers
// =============================================================================

/// Every committed definition, from the snapshot and the WAL alike. The
/// delta's `new_*` maps miss whatever a checkpoint already moved into the
/// snapshot.
fn build_schema(db: &SingleFileDB) -> ExportedSchema {
  ExportedSchema {
    labels: db.label_ids.read().clone(),
    etypes: db.etype_ids.read().clone(),
    prop_keys: db.propkey_ids.read().clone(),
  }
}

fn prop_key_name_single(db: &SingleFileDB, key_id: PropKeyId) -> String {
  db.propkey_name(key_id)
    .unwrap_or_else(|| format!("prop_{key_id}"))
}

fn etype_name_single(db: &SingleFileDB, etype_id: ETypeId) -> String {
  db.etype_name(etype_id)
    .unwrap_or_else(|| format!("etype_{etype_id}"))
}

fn label_name_single(db: &SingleFileDB, label_id: LabelId) -> String {
  db.label_name(label_id)
    .unwrap_or_else(|| format!("label_{label_id}"))
}

/// Run `read` with every commit and checkpoint install kept out, so all it
/// reads is one point in time. Takes the locks `create_backup_single_file`
/// takes for its copy: the checkpoint gate's read side, then the commit lock.
/// Writers wait until `read` returns.
fn with_commits_paused<T>(db: &SingleFileDB, read: impl FnOnce() -> T) -> T {
  // A blocking checkpoint can hold the gate's write side while it waits for
  // this thread's open transaction, so a read permit would never come. That
  // transaction keeps blocking checkpoints and compaction out by itself.
  let _checkpoint_gate = (!db.has_transaction()).then(|| db.checkpoint_gate.read());
  let _commit_guard = db.lock_commits();
  read()
}

/// Export the database as one point in time: commits wait until the export
/// is read.
pub fn export_to_object_single(
  db: &SingleFileDB,
  options: ExportOptions,
) -> Result<ExportedDatabase> {
  with_commits_paused(db, || export_snapshot(db, options))
}

fn export_snapshot(db: &SingleFileDB, options: ExportOptions) -> Result<ExportedDatabase> {
  // Read only through the read API, which takes and releases its own guards.
  // Holding `delta.read()` across those calls would re-lock it, and a commit or
  // checkpoint queued on `delta.write()` blocks that nested read forever.
  let schema = if options.include_schema {
    build_schema(db)
  } else {
    ExportedSchema::default()
  };

  let mut nodes = Vec::new();
  let mut edges = Vec::new();

  if options.include_nodes {
    for node_id in db.list_nodes() {
      let key = db.node_key(node_id);
      let mut labels: Vec<String> = db
        .node_labels(node_id)
        .into_iter()
        .map(|label_id| label_name_single(db, label_id))
        .collect();
      labels.sort_unstable();
      let mut props = HashMap::new();
      if let Some(props_by_id) = db.node_props(node_id) {
        for (key_id, value) in props_by_id {
          let name = prop_key_name_single(db, key_id);
          props.insert(name, serialize_prop_value(&value));
        }
      }
      nodes.push(ExportedNode {
        id: node_id,
        key,
        labels,
        props,
      });
    }
  }

  if options.include_edges {
    for edge in db.list_edges(None) {
      let mut props = HashMap::new();
      if let Some(props_by_id) = db.edge_props(edge.src, edge.etype, edge.dst) {
        for (key_id, value) in props_by_id {
          let name = prop_key_name_single(db, key_id);
          props.insert(name, serialize_prop_value(&value));
        }
      }
      edges.push(ExportedEdge {
        src: edge.src,
        dst: edge.dst,
        etype: edge.etype,
        etype_name: Some(etype_name_single(db, edge.etype)),
        props,
      });
    }
  }

  let exported_at = match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
    Ok(duration) => duration.as_secs().to_string(),
    Err(_) => "0".to_string(),
  };

  let node_count = nodes.len();
  let edge_count = edges.len();

  Ok(ExportedDatabase {
    version: 1,
    exported_at,
    schema,
    nodes,
    edges,
    stats: ExportStats {
      node_count,
      edge_count,
    },
  })
}

pub fn export_to_json<P: AsRef<Path>>(
  data: &ExportedDatabase,
  path: P,
  pretty: bool,
) -> Result<ExportResult> {
  let file = File::create(path).map_err(KiteError::Io)?;
  let mut writer = BufWriter::new(file);
  if pretty {
    serde_json::to_writer_pretty(&mut writer, data)
      .map_err(|e| KiteError::Serialization(e.to_string()))?;
  } else {
    serde_json::to_writer(&mut writer, data)
      .map_err(|e| KiteError::Serialization(e.to_string()))?;
  }
  writer.flush().map_err(KiteError::Io)?;
  Ok(ExportResult {
    node_count: data.stats.node_count,
    edge_count: data.stats.edge_count,
  })
}

pub fn export_to_jsonl<P: AsRef<Path>>(data: &ExportedDatabase, path: P) -> Result<ExportResult> {
  let file = File::create(path).map_err(KiteError::Io)?;
  let mut writer = BufWriter::new(file);

  let header = JsonLine::<serde_json::Value> {
    r#type: "header".to_string(),
    data: Some(serde_json::json!({
      "version": data.version,
      "exportedAt": data.exported_at,
    })),
  };
  writeln!(
    writer,
    "{}",
    serde_json::to_string(&header).map_err(|e| KiteError::Serialization(e.to_string()))?
  )
  .map_err(KiteError::Io)?;

  let schema = JsonLine {
    r#type: "schema".to_string(),
    data: Some(
      serde_json::to_value(&data.schema).map_err(|e| KiteError::Serialization(e.to_string()))?,
    ),
  };
  writeln!(
    writer,
    "{}",
    serde_json::to_string(&schema).map_err(|e| KiteError::Serialization(e.to_string()))?
  )
  .map_err(KiteError::Io)?;

  for node in &data.nodes {
    let line = JsonLine {
      r#type: "node".to_string(),
      data: Some(serde_json::to_value(node).map_err(|e| KiteError::Serialization(e.to_string()))?),
    };
    writeln!(
      writer,
      "{}",
      serde_json::to_string(&line).map_err(|e| KiteError::Serialization(e.to_string()))?
    )
    .map_err(KiteError::Io)?;
  }

  for edge in &data.edges {
    let line = JsonLine {
      r#type: "edge".to_string(),
      data: Some(serde_json::to_value(edge).map_err(|e| KiteError::Serialization(e.to_string()))?),
    };
    writeln!(
      writer,
      "{}",
      serde_json::to_string(&line).map_err(|e| KiteError::Serialization(e.to_string()))?
    )
    .map_err(KiteError::Io)?;
  }

  writer.flush().map_err(KiteError::Io)?;
  Ok(ExportResult {
    node_count: data.stats.node_count,
    edge_count: data.stats.edge_count,
  })
}

pub fn import_from_object_single(
  db: &SingleFileDB,
  data: &ExportedDatabase,
  options: ImportOptions,
) -> Result<ImportResult> {
  let mut propkey_name_to_id: HashMap<String, PropKeyId> = HashMap::new();
  let mut etype_name_to_id: HashMap<String, ETypeId> = HashMap::new();
  let mut label_name_to_id: HashMap<String, LabelId> = HashMap::new();
  let schema_tx = db.begin_guard(false)?;

  for name in data.schema.prop_keys.values() {
    resolve_propkey(db, &mut propkey_name_to_id, name)?;
  }
  for name in data.schema.etypes.values() {
    resolve_etype(db, &mut etype_name_to_id, name)?;
  }
  for name in data.schema.labels.values() {
    resolve_label(db, &mut label_name_to_id, name)?;
  }
  schema_tx.commit()?;

  let mut old_to_new: HashMap<NodeId, NodeId> = HashMap::new();
  let mut node_count = 0usize;
  let mut skipped = 0usize;
  let mut batch_count = 0usize;

  let mut tx = db.begin_guard(false)?;
  for node in &data.nodes {
    if options.skip_existing {
      if let Some(ref key) = node.key {
        if let Some(existing) = db.node_by_key(key) {
          old_to_new.insert(node.id as NodeId, existing);
          skipped += 1;
          continue;
        }
      }
    }

    let node_id = db.create_node(node.key.as_deref())?;
    for (prop_name, exported_value) in &node.props {
      // Older exports can carry a partial schema, so names are resolved here.
      let key_id = resolve_propkey(db, &mut propkey_name_to_id, prop_name)?;
      db.set_node_prop(node_id, key_id, deserialize_prop_value(exported_value))?;
    }
    for label in &node.labels {
      let label_id = resolve_label(db, &mut label_name_to_id, label)?;
      db.add_node_label(node_id, label_id)?;
    }

    old_to_new.insert(node.id as NodeId, node_id);
    node_count += 1;
    batch_count += 1;

    if batch_count >= options.batch_size {
      tx.commit()?;
      tx = db.begin_guard(false)?;
      batch_count = 0;
    }
  }

  if batch_count > 0 {
    tx.commit()?;
  } else {
    tx.rollback()?;
  }

  let mut edge_count = 0usize;
  let mut batch_count = 0usize;
  let mut tx = db.begin_guard(false)?;
  for edge in &data.edges {
    let src = match old_to_new.get(&(edge.src as NodeId)) {
      Some(id) => *id,
      None => continue,
    };
    let dst = match old_to_new.get(&(edge.dst as NodeId)) {
      Some(id) => *id,
      None => continue,
    };

    let etype_name = edge
      .etype_name
      .as_deref()
      .or_else(|| data.schema.etypes.get(&edge.etype).map(String::as_str));
    let etype_id = match etype_name {
      Some(name) => resolve_etype(db, &mut etype_name_to_id, name)?,
      None => edge.etype as ETypeId,
    };

    db.add_edge(src, etype_id, dst)?;
    let mut props = Vec::with_capacity(edge.props.len());
    for (prop_name, exported_value) in &edge.props {
      let key_id = resolve_propkey(db, &mut propkey_name_to_id, prop_name)?;
      props.push((key_id, deserialize_prop_value(exported_value)));
    }
    db.set_edge_props(src, etype_id, dst, props)?;
    edge_count += 1;
    batch_count += 1;

    if batch_count >= options.batch_size {
      tx.commit()?;
      tx = db.begin_guard(false)?;
      batch_count = 0;
    }
  }

  if batch_count > 0 {
    tx.commit()?;
  } else {
    tx.rollback()?;
  }

  Ok(ImportResult {
    node_count,
    edge_count,
    skipped,
  })
}

/// Returns the id for a property key name, defining the key in the open
/// write transaction when the database does not know it yet.
fn resolve_propkey(
  db: &SingleFileDB,
  known: &mut HashMap<String, PropKeyId>,
  name: &str,
) -> Result<PropKeyId> {
  if let Some(&id) = known.get(name) {
    return Ok(id);
  }
  let id = db.define_propkey(name)?;
  known.insert(name.to_string(), id);
  Ok(id)
}

/// Returns the id for a label name, defining the label in the open write
/// transaction when the database does not know it yet.
fn resolve_label(
  db: &SingleFileDB,
  known: &mut HashMap<String, LabelId>,
  name: &str,
) -> Result<LabelId> {
  if let Some(&id) = known.get(name) {
    return Ok(id);
  }
  let id = db.define_label(name)?;
  known.insert(name.to_string(), id);
  Ok(id)
}

/// Returns the id for an edge type name, defining the type in the open write
/// transaction when the database does not know it yet.
fn resolve_etype(
  db: &SingleFileDB,
  known: &mut HashMap<String, ETypeId>,
  name: &str,
) -> Result<ETypeId> {
  if let Some(&id) = known.get(name) {
    return Ok(id);
  }
  let id = db.define_etype(name)?;
  known.insert(name.to_string(), id);
  Ok(id)
}

pub fn import_from_json<P: AsRef<Path>>(path: P) -> Result<ExportedDatabase> {
  let file = File::open(path).map_err(KiteError::Io)?;
  let reader = BufReader::new(file);
  let data: ExportedDatabase =
    serde_json::from_reader(reader).map_err(|e| KiteError::Serialization(e.to_string()))?;
  Ok(data)
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::core::single_file::{close_single_file, open_single_file, SingleFileOpenOptions};

  /// Exports written before the schema came from the full name maps carry an
  /// empty schema once the source had checkpointed. Import resolves names from
  /// the nodes and edges themselves, and maps a nameless edge type through the
  /// schema's etypes.
  #[test]
  fn imports_export_with_partial_schema() {
    let json = r#"{
      "version": 1,
      "exported_at": "0",
      "schema": { "labels": {}, "etypes": { "7": "LIKES" }, "prop_keys": {} },
      "nodes": [
        { "id": 1, "key": "user:alice", "props": { "name": { "type": "string", "value": "Alice" } } },
        { "id": 2, "key": "user:bob", "props": { "age": { "type": "int", "value": 41 } } }
      ],
      "edges": [
        { "src": 1, "dst": 2, "etype": 3, "etype_name": "KNOWS",
          "props": { "since": { "type": "int", "value": 2020 } } },
        { "src": 2, "dst": 1, "etype": 7, "etype_name": null, "props": {} }
      ],
      "stats": { "node_count": 2, "edge_count": 2 }
    }"#;
    let data: ExportedDatabase = serde_json::from_str(json).expect("parse export");

    let dir = tempfile::tempdir().expect("tempdir");
    let db =
      open_single_file(dir.path().join("db.kitedb"), SingleFileOpenOptions::new()).expect("open");
    let result = import_from_object_single(&db, &data, ImportOptions::default()).expect("import");
    assert_eq!((result.node_count, result.edge_count), (2, 2));

    let alice = db.node_by_key("user:alice").expect("alice");
    let bob = db.node_by_key("user:bob").expect("bob");
    let prop = |node, name: &str| db.node_prop(node, db.propkey_id(name).expect(name));
    assert_eq!(prop(alice, "name"), Some(PropValue::String("Alice".into())));
    assert_eq!(prop(bob, "age"), Some(PropValue::I64(41)));

    let knows = db.etype_id("KNOWS").expect("KNOWS");
    let since = db.propkey_id("since").expect("since");
    assert_eq!(
      db.edge_prop(alice, knows, bob, since),
      Some(PropValue::I64(2020))
    );
    let likes = db.etype_id("LIKES").expect("LIKES");
    assert!(db.edge_exists(bob, likes, alice));
    close_single_file(db).expect("close");
  }
}
