//! Export and Import utilities
//!
//! JSON and JSONL export/import for GraphDB and SingleFileDB.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufReader, BufWriter, Write};
use std::path::Path;

use crate::core::single_file::SingleFileDB;
use crate::error::{RayError, Result};
use crate::graph::db::GraphDB;
use crate::graph::definitions::{define_etype, define_label, define_propkey};
use crate::graph::edges::{add_edge, get_edge_props_db};
use crate::graph::iterators::{
  list_edges as graph_list_edges, list_nodes as graph_list_nodes, ListEdgesOptions,
};
use crate::graph::key_index::get_node_key;
use crate::graph::nodes::{create_node, get_node_by_key_db, get_node_props_db, NodeOpts};
use crate::graph::tx::{begin_tx, commit, rollback};
use crate::types::{ETypeId, NodeId, PropKeyId, PropValue};

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

fn build_schema_from_delta(delta: &crate::types::DeltaState) -> ExportedSchema {
  let mut schema = ExportedSchema::default();
  for (id, name) in &delta.new_labels {
    schema.labels.insert(*id, name.clone());
  }
  for (id, name) in &delta.new_etypes {
    schema.etypes.insert(*id, name.clone());
  }
  for (id, name) in &delta.new_propkeys {
    schema.prop_keys.insert(*id, name.clone());
  }
  schema
}

fn get_prop_key_name_graph(db: &GraphDB, key_id: PropKeyId) -> String {
  db.get_propkey_name(key_id)
    .unwrap_or_else(|| format!("prop_{key_id}"))
}

fn get_etype_name_graph(db: &GraphDB, etype_id: ETypeId) -> String {
  db.get_etype_name(etype_id)
    .unwrap_or_else(|| format!("etype_{etype_id}"))
}

fn get_prop_key_name_single(db: &SingleFileDB, key_id: PropKeyId) -> String {
  db.get_propkey_name(key_id)
    .unwrap_or_else(|| format!("prop_{key_id}"))
}

fn get_etype_name_single(db: &SingleFileDB, etype_id: ETypeId) -> String {
  db.get_etype_name(etype_id)
    .unwrap_or_else(|| format!("etype_{etype_id}"))
}

// =============================================================================
// Export (GraphDB)
// =============================================================================

pub fn export_to_object_graph(db: &GraphDB, options: ExportOptions) -> Result<ExportedDatabase> {
  let delta = db.delta.read();
  let schema = if options.include_schema {
    build_schema_from_delta(&delta)
  } else {
    ExportedSchema::default()
  };

  let mut nodes = Vec::new();
  let mut edges = Vec::new();

  if options.include_nodes {
    for node_id in graph_list_nodes(db) {
      let key = get_node_key(db.snapshot.as_ref(), &delta, node_id);
      let mut props = HashMap::new();
      if let Some(props_by_id) = get_node_props_db(db, node_id) {
        for (key_id, value) in props_by_id {
          let name = get_prop_key_name_graph(db, key_id);
          props.insert(name, serialize_prop_value(&value));
        }
      }

      nodes.push(ExportedNode {
        id: node_id,
        key,
        props,
      });
    }
  }

  if options.include_edges {
    let options = ListEdgesOptions::default();
    for edge in graph_list_edges(db, options) {
      let mut props = HashMap::new();
      if let Some(props_by_id) = get_edge_props_db(db, edge.src, edge.etype, edge.dst) {
        for (key_id, value) in props_by_id {
          let name = get_prop_key_name_graph(db, key_id);
          props.insert(name, serialize_prop_value(&value));
        }
      }
      edges.push(ExportedEdge {
        src: edge.src,
        dst: edge.dst,
        etype: edge.etype,
        etype_name: Some(get_etype_name_graph(db, edge.etype)),
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

pub fn export_to_object_single(
  db: &SingleFileDB,
  options: ExportOptions,
) -> Result<ExportedDatabase> {
  let delta = db.delta.read();
  let schema = if options.include_schema {
    build_schema_from_delta(&delta)
  } else {
    ExportedSchema::default()
  };

  let mut nodes = Vec::new();
  let mut edges = Vec::new();

  if options.include_nodes {
    for node_id in db.list_nodes() {
      let key = db.get_node_key(node_id);
      let mut props = HashMap::new();
      if let Some(props_by_id) = db.get_node_props(node_id) {
        for (key_id, value) in props_by_id {
          let name = get_prop_key_name_single(db, key_id);
          props.insert(name, serialize_prop_value(&value));
        }
      }
      nodes.push(ExportedNode {
        id: node_id,
        key,
        props,
      });
    }
  }

  if options.include_edges {
    for edge in db.list_edges(None) {
      let mut props = HashMap::new();
      if let Some(props_by_id) = db.get_edge_props(edge.src, edge.etype, edge.dst) {
        for (key_id, value) in props_by_id {
          let name = get_prop_key_name_single(db, key_id);
          props.insert(name, serialize_prop_value(&value));
        }
      }
      edges.push(ExportedEdge {
        src: edge.src,
        dst: edge.dst,
        etype: edge.etype,
        etype_name: Some(get_etype_name_single(db, edge.etype)),
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
  let file = File::create(path).map_err(RayError::Io)?;
  let mut writer = BufWriter::new(file);
  if pretty {
    serde_json::to_writer_pretty(&mut writer, data)
      .map_err(|e| RayError::Serialization(e.to_string()))?;
  } else {
    serde_json::to_writer(&mut writer, data).map_err(|e| RayError::Serialization(e.to_string()))?;
  }
  writer.flush().map_err(RayError::Io)?;
  Ok(ExportResult {
    node_count: data.stats.node_count,
    edge_count: data.stats.edge_count,
  })
}

pub fn export_to_jsonl<P: AsRef<Path>>(data: &ExportedDatabase, path: P) -> Result<ExportResult> {
  let file = File::create(path).map_err(RayError::Io)?;
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
    serde_json::to_string(&header).map_err(|e| RayError::Serialization(e.to_string()))?
  )
  .map_err(RayError::Io)?;

  let schema = JsonLine {
    r#type: "schema".to_string(),
    data: Some(
      serde_json::to_value(&data.schema).map_err(|e| RayError::Serialization(e.to_string()))?,
    ),
  };
  writeln!(
    writer,
    "{}",
    serde_json::to_string(&schema).map_err(|e| RayError::Serialization(e.to_string()))?
  )
  .map_err(RayError::Io)?;

  for node in &data.nodes {
    let line = JsonLine {
      r#type: "node".to_string(),
      data: Some(serde_json::to_value(node).map_err(|e| RayError::Serialization(e.to_string()))?),
    };
    writeln!(
      writer,
      "{}",
      serde_json::to_string(&line).map_err(|e| RayError::Serialization(e.to_string()))?
    )
    .map_err(RayError::Io)?;
  }

  for edge in &data.edges {
    let line = JsonLine {
      r#type: "edge".to_string(),
      data: Some(serde_json::to_value(edge).map_err(|e| RayError::Serialization(e.to_string()))?),
    };
    writeln!(
      writer,
      "{}",
      serde_json::to_string(&line).map_err(|e| RayError::Serialization(e.to_string()))?
    )
    .map_err(RayError::Io)?;
  }

  writer.flush().map_err(RayError::Io)?;
  Ok(ExportResult {
    node_count: data.stats.node_count,
    edge_count: data.stats.edge_count,
  })
}

// =============================================================================
// Import (GraphDB)
// =============================================================================

pub fn import_from_object_graph(
  db: &GraphDB,
  data: &ExportedDatabase,
  options: ImportOptions,
) -> Result<ImportResult> {
  let mut propkey_name_to_id: HashMap<String, PropKeyId> = HashMap::new();
  let mut etype_name_to_id: HashMap<String, ETypeId> = HashMap::new();

  let mut tx = begin_tx(db)?;
  for name in data.schema.prop_keys.values() {
    let id = db
      .get_propkey_id(name)
      .unwrap_or_else(|| define_propkey(&mut tx, name).unwrap_or(0));
    propkey_name_to_id.insert(name.clone(), id);
  }
  for name in data.schema.etypes.values() {
    let id = db
      .get_etype_id(name)
      .unwrap_or_else(|| define_etype(&mut tx, name).unwrap_or(0));
    etype_name_to_id.insert(name.clone(), id);
  }
  for name in data.schema.labels.values() {
    let _ = db
      .get_label_id(name)
      .unwrap_or_else(|| define_label(&mut tx, name).unwrap_or(0));
  }
  commit(&mut tx)?;

  let mut old_to_new: HashMap<NodeId, NodeId> = HashMap::new();
  let mut node_count = 0usize;
  let mut skipped = 0usize;
  let mut batch_count = 0usize;

  let mut tx = begin_tx(db)?;
  for node in &data.nodes {
    if options.skip_existing {
      if let Some(ref key) = node.key {
        if let Some(existing) = get_node_by_key_db(db, key) {
          old_to_new.insert(node.id as NodeId, existing);
          skipped += 1;
          continue;
        }
      }
    }

    let mut node_opts = NodeOpts::new();
    if let Some(ref key) = node.key {
      node_opts = node_opts.with_key(key);
    }
    let node_id = match create_node(&mut tx, node_opts) {
      Ok(id) => id,
      Err(e) => {
        rollback(&mut tx)?;
        return Err(e);
      }
    };

    for (prop_name, exported_value) in &node.props {
      if let Some(&key_id) = propkey_name_to_id.get(prop_name) {
        let value = deserialize_prop_value(exported_value);
        crate::graph::nodes::set_node_prop(&mut tx, node_id, key_id, value)?;
      }
    }

    old_to_new.insert(node.id as NodeId, node_id);
    node_count += 1;
    batch_count += 1;

    if batch_count >= options.batch_size {
      commit(&mut tx)?;
      tx = begin_tx(db)?;
      batch_count = 0;
    }
  }

  if batch_count > 0 {
    commit(&mut tx)?;
  }

  let mut edge_count = 0usize;
  let mut batch_count = 0usize;
  let mut tx = begin_tx(db)?;
  for edge in &data.edges {
    let src = match old_to_new.get(&(edge.src as NodeId)) {
      Some(id) => *id,
      None => continue,
    };
    let dst = match old_to_new.get(&(edge.dst as NodeId)) {
      Some(id) => *id,
      None => continue,
    };

    let etype_id = edge
      .etype_name
      .as_ref()
      .and_then(|name| etype_name_to_id.get(name).copied())
      .unwrap_or(edge.etype as ETypeId);

    add_edge(&mut tx, src, etype_id, dst)?;
    edge_count += 1;
    batch_count += 1;

    if batch_count >= options.batch_size {
      commit(&mut tx)?;
      tx = begin_tx(db)?;
      batch_count = 0;
    }
  }

  if batch_count > 0 {
    commit(&mut tx)?;
  }

  Ok(ImportResult {
    node_count,
    edge_count,
    skipped,
  })
}

pub fn import_from_object_single(
  db: &SingleFileDB,
  data: &ExportedDatabase,
  options: ImportOptions,
) -> Result<ImportResult> {
  let mut propkey_name_to_id: HashMap<String, PropKeyId> = HashMap::new();
  let mut etype_name_to_id: HashMap<String, ETypeId> = HashMap::new();

  for name in data.schema.prop_keys.values() {
    let id = db
      .get_propkey_id(name)
      .unwrap_or_else(|| db.define_propkey(name).unwrap_or(0));
    propkey_name_to_id.insert(name.clone(), id);
  }
  for name in data.schema.etypes.values() {
    let id = db
      .get_etype_id(name)
      .unwrap_or_else(|| db.define_etype(name).unwrap_or(0));
    etype_name_to_id.insert(name.clone(), id);
  }
  for name in data.schema.labels.values() {
    let _ = db
      .get_label_id(name)
      .unwrap_or_else(|| db.define_label(name).unwrap_or(0));
  }

  let mut old_to_new: HashMap<NodeId, NodeId> = HashMap::new();
  let mut node_count = 0usize;
  let mut skipped = 0usize;
  let mut batch_count = 0usize;

  db.begin(false)?;
  for node in &data.nodes {
    if options.skip_existing {
      if let Some(ref key) = node.key {
        if let Some(existing) = db.get_node_by_key(key) {
          old_to_new.insert(node.id as NodeId, existing);
          skipped += 1;
          continue;
        }
      }
    }

    let node_id = db.create_node(node.key.as_deref())?;
    for (prop_name, exported_value) in &node.props {
      if let Some(&key_id) = propkey_name_to_id.get(prop_name) {
        let value = deserialize_prop_value(exported_value);
        db.set_node_prop(node_id, key_id, value)?;
      }
    }

    old_to_new.insert(node.id as NodeId, node_id);
    node_count += 1;
    batch_count += 1;

    if batch_count >= options.batch_size {
      db.commit()?;
      db.begin(false)?;
      batch_count = 0;
    }
  }

  if batch_count > 0 {
    db.commit()?;
  }

  let mut edge_count = 0usize;
  let mut batch_count = 0usize;
  db.begin(false)?;
  for edge in &data.edges {
    let src = match old_to_new.get(&(edge.src as NodeId)) {
      Some(id) => *id,
      None => continue,
    };
    let dst = match old_to_new.get(&(edge.dst as NodeId)) {
      Some(id) => *id,
      None => continue,
    };

    let etype_id = edge
      .etype_name
      .as_ref()
      .and_then(|name| etype_name_to_id.get(name).copied())
      .unwrap_or(edge.etype as ETypeId);

    db.add_edge(src, etype_id, dst)?;
    edge_count += 1;
    batch_count += 1;

    if batch_count >= options.batch_size {
      db.commit()?;
      db.begin(false)?;
      batch_count = 0;
    }
  }

  if batch_count > 0 {
    db.commit()?;
  }

  Ok(ImportResult {
    node_count,
    edge_count,
    skipped,
  })
}

pub fn import_from_json<P: AsRef<Path>>(path: P) -> Result<ExportedDatabase> {
  let file = File::open(path).map_err(RayError::Io)?;
  let reader = BufReader::new(file);
  let data: ExportedDatabase =
    serde_json::from_reader(reader).map_err(|e| RayError::Serialization(e.to_string()))?;
  Ok(data)
}
