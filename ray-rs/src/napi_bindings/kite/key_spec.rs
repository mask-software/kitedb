//! Key specification parsing and handling
//!
//! Internal module for parsing JsKeySpec into the internal KeySpec enum
//! and handling key generation strategies.

use napi::bindgen_prelude::*;

use super::types::{JsKeySpec, JsPropSpec};
use crate::api::kite::{PropDef, PropType as KitePropType};

// =============================================================================
// Key Specs
// =============================================================================

/// Internal representation of key generation strategy
#[derive(Clone, Debug)]
pub(crate) enum KeySpec {
  /// Simple prefix-based keys: "prefix" + user-provided suffix
  Prefix { prefix: String },
  /// Template-based keys with placeholder substitution
  Template { prefix: String, template: String },
  /// Multi-part keys joined by separator
  Parts {
    prefix: String,
    fields: Vec<String>,
    separator: String,
  },
}

impl KeySpec {
  /// Get the key prefix
  pub(crate) fn prefix(&self) -> &str {
    match self {
      KeySpec::Prefix { prefix } => prefix,
      KeySpec::Template { prefix, .. } => prefix,
      KeySpec::Parts { prefix, .. } => prefix,
    }
  }
}

/// Parse a JsKeySpec into the internal KeySpec representation
pub(crate) fn parse_key_spec(node_name: &str, spec: Option<JsKeySpec>) -> Result<KeySpec> {
  let spec = match spec {
    Some(spec) => spec,
    None => {
      return Ok(KeySpec::Prefix {
        prefix: format!("{node_name}:"),
      })
    }
  };

  let kind = spec.kind.as_str();
  match kind {
    "prefix" => Ok(KeySpec::Prefix {
      prefix: spec.prefix.unwrap_or_else(|| format!("{node_name}:")),
    }),
    "template" => {
      let template = spec
        .template
        .ok_or_else(|| Error::from_reason("template key spec requires template"))?;
      let prefix = spec
        .prefix
        .unwrap_or_else(|| infer_prefix_from_template(&template));
      Ok(KeySpec::Template { prefix, template })
    }
    "parts" => {
      let fields = spec
        .fields
        .ok_or_else(|| Error::from_reason("parts key spec requires fields"))?;
      if fields.is_empty() {
        return Err(Error::from_reason(
          "parts key spec requires at least one field",
        ));
      }
      Ok(KeySpec::Parts {
        prefix: spec.prefix.unwrap_or_else(|| format!("{node_name}:")),
        fields,
        separator: spec.separator.unwrap_or_else(|| ":".to_string()),
      })
    }
    _ => Err(Error::from_reason(format!("unknown key spec kind: {kind}"))),
  }
}

/// Infer the prefix from a template by taking everything before the first '{'
pub(crate) fn infer_prefix_from_template(template: &str) -> String {
  match template.find('{') {
    Some(pos) => template[..pos].to_string(),
    None => "".to_string(),
  }
}

/// Node objects carry their identity as `id`, `key` and `type`; a prop with one
/// of these names would shadow it in every node returned to JS.
const RESERVED_NODE_PROPS: [&str; 3] = ["id", "key", "type"];

/// Convert a node prop spec, rejecting names reserved for node identity
pub(crate) fn node_prop_spec_to_def(
  node_name: &str,
  name: &str,
  spec: &JsPropSpec,
) -> Result<PropDef> {
  if RESERVED_NODE_PROPS.contains(&name) {
    return Err(Error::new(
      Status::InvalidArg,
      format!(
        "node type \"{node_name}\": prop name \"{name}\" is reserved for node identity (id, key, type)"
      ),
    ));
  }
  prop_spec_to_def(name, spec)
}

/// Convert a JsPropSpec to a PropDef for schema configuration
pub(crate) fn prop_spec_to_def(name: &str, spec: &JsPropSpec) -> Result<PropDef> {
  let mut prop = match spec.r#type.as_str() {
    "string" => PropDef::string(name),
    "int" => PropDef::int(name),
    "float" => PropDef::float(name),
    "bool" => PropDef::bool(name),
    "vector" => PropDef {
      name: name.to_string(),
      prop_type: KitePropType::Any,
      required: false,
      default: None,
    },
    "any" => PropDef {
      name: name.to_string(),
      prop_type: KitePropType::Any,
      required: false,
      default: None,
    },
    other => return Err(Error::from_reason(format!("unknown prop type: {other}"))),
  };

  let optional = spec.optional.unwrap_or(false);
  if !optional {
    prop = prop.required();
  }

  if let Some(default_value) = spec.r#default.clone() {
    prop = prop.default(default_value.into());
  }

  Ok(prop)
}
