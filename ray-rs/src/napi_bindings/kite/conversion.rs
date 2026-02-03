//! JS ↔ Rust value conversion utilities
//!
//! Functions for converting between JavaScript values and Rust types,
//! including property values, key specifications, and template rendering.

use napi::bindgen_prelude::*;
use std::collections::HashMap;

use crate::api::kite::{PropDef, PropType as KitePropType};
use crate::types::PropValue;

use super::super::database::{JsPropValue, PropType as DbPropType};
use super::key_spec::KeySpec;
use super::types::JsPropSpec;

// =============================================================================
// Prop Spec Conversion
// =============================================================================

/// Convert a JS property specification to a Rust PropDef
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

// =============================================================================
// JS Value Conversion
// =============================================================================

/// Convert a JS Unknown value to a Rust PropValue
pub(crate) fn js_value_to_prop_value(_env: &Env, value: Unknown) -> Result<PropValue> {
  match value.get_type()? {
    ValueType::Undefined => Ok(PropValue::Null),
    ValueType::Null => Ok(PropValue::Null),
    ValueType::Boolean => Ok(PropValue::Bool(value.coerce_to_bool()?)),
    ValueType::Number => Ok(PropValue::F64(value.coerce_to_number()?.get_double()?)),
    ValueType::String => Ok(PropValue::String(
      value.coerce_to_string()?.into_utf8()?.as_str()?.to_string(),
    )),
    ValueType::BigInt => {
      // SAFETY: value type checked as BigInt above.
      let big: BigInt = unsafe { value.cast()? };
      let (v, _lossless) = big.get_i64();
      Ok(PropValue::I64(v))
    }
    ValueType::Object => {
      let obj = value.coerce_to_object()?;
      if obj.is_array()? {
        // SAFETY: value is an array; NAPI will validate element types on cast.
        let values: Vec<f64> = unsafe { value.cast()? };
        let values = values.into_iter().map(|v| v as f32).collect();
        return Ok(PropValue::VectorF32(values));
      }

      // Check for JsPropValue-style object
      if obj.has_named_property("propType")? {
        let prop_type: DbPropType = obj.get_named_property("propType")?;
        let bool_value: Option<bool> = obj.get_named_property("boolValue")?;
        let int_value: Option<i64> = obj.get_named_property("intValue")?;
        let float_value: Option<f64> = obj.get_named_property("floatValue")?;
        let string_value: Option<String> = obj.get_named_property("stringValue")?;
        let vector_value: Option<Vec<f64>> = obj.get_named_property("vectorValue")?;
        let prop_value = JsPropValue {
          prop_type,
          bool_value,
          int_value,
          float_value,
          string_value,
          vector_value,
        };
        return Ok(prop_value.into());
      }

      Err(Error::from_reason(
        "Object props must be plain values or JsPropValue",
      ))
    }
    _ => Err(Error::from_reason("Unsupported prop value type")),
  }
}

/// Convert a JS Object of properties to a HashMap
pub(crate) fn js_props_to_map(
  env: &Env,
  props: Option<Object>,
) -> Result<HashMap<String, PropValue>> {
  let mut result = HashMap::new();
  let props = match props {
    Some(props) => props,
    None => return Ok(result),
  };

  for name in Object::keys(&props)? {
    let value: Unknown = props.get_named_property(&name)?;
    result.insert(name, js_value_to_prop_value(env, value)?);
  }

  Ok(result)
}

/// Convert a JS value to a string (for key fields)
pub(crate) fn js_value_to_string(_env: &Env, value: Unknown, field: &str) -> Result<String> {
  match value.get_type()? {
    ValueType::String => Ok(value.coerce_to_string()?.into_utf8()?.as_str()?.to_string()),
    ValueType::Number => Ok(value.coerce_to_number()?.get_double()?.to_string()),
    ValueType::Boolean => Ok(value.coerce_to_bool()?.to_string()),
    ValueType::BigInt => {
      // SAFETY: value type checked as BigInt above.
      let big: BigInt = unsafe { value.cast()? };
      let (v, _lossless) = big.get_i64();
      Ok(v.to_string())
    }
    _ => Err(Error::from_reason(format!(
      "Invalid key field '{field}' value type"
    ))),
  }
}

/// Render a template string with argument substitution
pub(crate) fn render_template(template: &str, args: &HashMap<String, String>) -> Result<String> {
  let mut out = String::new();
  let mut chars = template.chars().peekable();
  loop {
    let Some(ch) = chars.next() else { break };
    if ch == '{' {
      let mut field = String::new();
      for c in chars.by_ref() {
        if c == '}' {
          break;
        }
        field.push(c);
      }
      if field.is_empty() {
        return Err(Error::from_reason("Empty template field"));
      }
      let value = args
        .get(&field)
        .ok_or_else(|| Error::from_reason(format!("Missing key field: {field}")))?;
      out.push_str(value);
    } else {
      out.push(ch);
    }
  }
  Ok(out)
}

/// Extract key suffix from a JS value based on the key specification
pub(crate) fn key_suffix_from_js(env: &Env, spec: &KeySpec, value: Unknown) -> Result<String> {
  let prefix = spec.prefix();
  match value.get_type()? {
    ValueType::String => {
      let raw = value.coerce_to_string()?.into_utf8()?.as_str()?.to_string();
      if let Some(stripped) = raw.strip_prefix(prefix) {
        Ok(stripped.to_string())
      } else {
        match spec {
          KeySpec::Prefix { .. } => Ok(raw),
          _ => Err(Error::from_reason(
            "Key spec requires object or full key string",
          )),
        }
      }
    }
    ValueType::Object => {
      let obj = value.coerce_to_object()?;

      match spec {
        KeySpec::Prefix { .. } => {
          if obj.has_named_property("id")? {
            let val: Unknown = obj.get_named_property("id")?;
            return js_value_to_string(env, val, "id");
          }
          Err(Error::from_reason("Key object must include 'id'"))
        }
        KeySpec::Template { prefix, template } => {
          let mut args = HashMap::new();
          for name in Object::keys(&obj)? {
            let val: Unknown = obj.get_named_property(&name)?;
            args.insert(name.clone(), js_value_to_string(env, val, &name)?);
          }
          let full_key = render_template(template, &args)?;
          if !full_key.starts_with(prefix) {
            return Err(Error::from_reason(
              "Template key does not start with prefix",
            ));
          }
          Ok(full_key[prefix.len()..].to_string())
        }
        KeySpec::Parts {
          fields, separator, ..
        } => {
          let mut parts = Vec::with_capacity(fields.len());
          for field in fields {
            let val: Unknown = obj
              .get_named_property(field)
              .map_err(|_| Error::from_reason(format!("Missing key field: {field}")))?;
            parts.push(js_value_to_string(env, val, field)?);
          }
          Ok(parts.join(separator))
        }
      }
    }
    _ => Err(Error::from_reason("Invalid key value")),
  }
}
