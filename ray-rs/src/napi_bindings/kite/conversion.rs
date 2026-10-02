//! JS ↔ Rust value conversion utilities
//!
//! Functions for converting between JavaScript values and Rust types,
//! including property values, key specifications, and template rendering.

use napi::bindgen_prelude::*;
use std::collections::HashMap;

use crate::types::PropValue;

use super::super::database::{int_to_js, JsPropValue};
use super::super::validation;
use super::key_spec::KeySpec;

// =============================================================================
// JS Value Conversion
// =============================================================================

/// Convert a JS Unknown value to a Rust PropValue
///
/// - a BigInt must fit an i64 (it is rejected, not wrapped);
/// - a Float32Array is an f32 vector;
/// - a number[] is also stored as an f32 vector (the only array type), so it
///   is rejected when an element cannot survive that: an integer f32 cannot
///   hold exactly (beyond 2^24), or a finite number beyond the f32 range;
/// - a `{ propType, ... }` object must carry the value its `propType` names.
pub(crate) fn js_value_to_prop_value(env: &Env, value: Unknown) -> Result<PropValue> {
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
      Ok(PropValue::I64(validation::bigint_i64(
        "BigInt prop value",
        &big,
      )?))
    }
    ValueType::Object => {
      let obj = value.coerce_to_object()?;
      if obj.is_typedarray()? {
        // SAFETY: value is a typed array; the cast checks it is a Float32Array.
        let vector: Float32ArraySlice = unsafe { value.cast() }.map_err(|_| {
          validation::invalid_argument(
            "Typed array props must be Float32Array (stored as an f32 vector)",
          )
        })?;
        return Ok(PropValue::VectorF32(vector.as_ref().to_vec()));
      }
      if obj.is_array()? {
        // SAFETY: value is an array; NAPI will validate element types on cast.
        let values: Vec<f64> = unsafe { value.cast()? };
        return number_array_to_vector(&values).map(PropValue::VectorF32);
      }

      // JsPropValue-style object
      if obj.has_named_property("propType")? {
        // SAFETY: the raw handles come from a live JS object in this call.
        let prop_value = unsafe { JsPropValue::from_napi_value(env.raw(), value.raw())? };
        return prop_value.try_into();
      }

      Err(Error::from_reason(
        "Object props must be plain values, Float32Array or JsPropValue",
      ))
    }
    _ => Err(Error::from_reason("Unsupported prop value type")),
  }
}

/// A number[] as an f32 vector, rejecting elements f32 would change beyond
/// rounding: integers it cannot hold exactly, and overflow to infinity.
fn number_array_to_vector(values: &[f64]) -> Result<Vec<f32>> {
  values
    .iter()
    .enumerate()
    .map(|(index, &value)| {
      let narrowed = value as f32;
      let overflow = value.is_finite() && narrowed.is_infinite();
      let inexact_integer = value.is_finite() && value.fract() == 0.0 && narrowed as f64 != value;
      if overflow || inexact_integer {
        return Err(validation::invalid_argument(format!(
          "number[] prop element [{index}] = {value} would change when stored: a number[] \
           is stored as an f32 vector, which cannot hold it exactly. Store it as a separate \
           number or BigInt prop, or pass a Float32Array if f32 precision is intended"
        )));
      }
      Ok(narrowed)
    })
    .collect()
}

/// An i64 prop value for JS: a number while it is a safe integer, else a
/// BigInt, so values beyond 2^53 keep every digit.
pub(crate) fn i64_to_js(env: &Env, value: i64) -> Result<Unknown<'_>> {
  int_to_js(value).into_unknown(env)
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
    // The exact decimal digits, however large (a key is a string).
    ValueType::BigInt => Ok(value.coerce_to_string()?.into_utf8()?.as_str()?.to_string()),
    _ => Err(Error::from_reason(format!(
      "Invalid key field '{field}' value type"
    ))),
  }
}

/// Render a template string with argument substitution
pub(crate) fn render_template(template: &str, args: &HashMap<String, String>) -> Result<String> {
  let mut out = String::new();
  let mut chars = template.chars().peekable();
  while let Some(ch) = chars.next() {
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
