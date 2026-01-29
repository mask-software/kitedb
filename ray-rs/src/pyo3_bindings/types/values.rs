//! Property value types for Python bindings

use crate::types::PropValue as CorePropValue;
use pyo3::prelude::*;

/// Property value types
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PropType {
  Null,
  Bool,
  Int,
  Float,
  String,
  Vector,
}

/// Property value wrapper for Python
#[pyclass(name = "PropValue")]
#[derive(Debug, Clone)]
pub struct PropValue {
  #[pyo3(get)]
  pub prop_type: String,
  #[pyo3(get)]
  pub bool_value: Option<bool>,
  #[pyo3(get)]
  pub int_value: Option<i64>,
  #[pyo3(get)]
  pub float_value: Option<f64>,
  #[pyo3(get)]
  pub string_value: Option<String>,
  #[pyo3(get)]
  pub vector_value: Option<Vec<f64>>,
}

#[pymethods]
impl PropValue {
  /// Create a null value
  #[staticmethod]
  fn null() -> Self {
    PropValue {
      prop_type: "null".to_string(),
      bool_value: None,
      int_value: None,
      float_value: None,
      string_value: None,
      vector_value: None,
    }
  }

  /// Create a boolean value
  #[staticmethod]
  fn bool(value: bool) -> Self {
    PropValue {
      prop_type: "bool".to_string(),
      bool_value: Some(value),
      int_value: None,
      float_value: None,
      string_value: None,
      vector_value: None,
    }
  }

  /// Create an integer value
  #[staticmethod]
  fn int(value: i64) -> Self {
    PropValue {
      prop_type: "int".to_string(),
      bool_value: None,
      int_value: Some(value),
      float_value: None,
      string_value: None,
      vector_value: None,
    }
  }

  /// Create a float value
  #[staticmethod]
  #[pyo3(name = "float")]
  fn float_val(value: f64) -> Self {
    PropValue {
      prop_type: "float".to_string(),
      bool_value: None,
      int_value: None,
      float_value: Some(value),
      string_value: None,
      vector_value: None,
    }
  }

  /// Create a string value
  #[staticmethod]
  fn string(value: String) -> Self {
    PropValue {
      prop_type: "string".to_string(),
      bool_value: None,
      int_value: None,
      float_value: None,
      string_value: Some(value),
      vector_value: None,
    }
  }

  /// Create a vector value
  #[staticmethod]
  fn vector(value: Vec<f64>) -> Self {
    PropValue {
      prop_type: "vector".to_string(),
      bool_value: None,
      int_value: None,
      float_value: None,
      string_value: None,
      vector_value: Some(value),
    }
  }

  /// Get the Python value
  fn value(&self, py: Python<'_>) -> PyObject {
    use pyo3::ToPyObject;
    match self.prop_type.as_str() {
      "null" => py.None(),
      "bool" => self.bool_value.unwrap_or(false).to_object(py),
      "int" => self.int_value.unwrap_or(0).to_object(py),
      "float" => self.float_value.unwrap_or(0.0).to_object(py),
      "string" => self.string_value.clone().unwrap_or_default().to_object(py),
      "vector" => self.vector_value.clone().unwrap_or_default().to_object(py),
      _ => py.None(),
    }
  }

  fn __repr__(&self) -> String {
    match self.prop_type.as_str() {
      "null" => "PropValue(null)".to_string(),
      "bool" => format!("PropValue({})", self.bool_value.unwrap_or(false)),
      "int" => format!("PropValue({})", self.int_value.unwrap_or(0)),
      "float" => format!("PropValue({})", self.float_value.unwrap_or(0.0)),
      "string" => format!(
        "PropValue(\"{}\")",
        self.string_value.clone().unwrap_or_default()
      ),
      "vector" => format!(
        "PropValue(vector, len={})",
        self.vector_value.as_ref().map(|v| v.len()).unwrap_or(0)
      ),
      _ => "PropValue(unknown)".to_string(),
    }
  }
}

// Public constructors for use within the crate (tests, other modules)
impl PropValue {
  /// Create a null value (pub for crate use)
  pub fn new_null() -> Self {
    Self::null()
  }

  /// Create a bool value (pub for crate use)
  pub fn new_bool(value: bool) -> Self {
    Self::bool(value)
  }

  /// Create an int value (pub for crate use)
  pub fn new_int(value: i64) -> Self {
    Self::int(value)
  }

  /// Create a float value (pub for crate use)
  pub fn new_float(value: f64) -> Self {
    Self::float_val(value)
  }

  /// Create a string value (pub for crate use)
  pub fn new_string(value: String) -> Self {
    Self::string(value)
  }

  /// Create a vector value (pub for crate use)
  pub fn new_vector(value: Vec<f64>) -> Self {
    Self::vector(value)
  }
}

impl From<CorePropValue> for PropValue {
  fn from(value: CorePropValue) -> Self {
    match value {
      CorePropValue::Null => PropValue::null(),
      CorePropValue::Bool(v) => PropValue::bool(v),
      CorePropValue::I64(v) => PropValue::int(v),
      CorePropValue::F64(v) => PropValue::float_val(v),
      CorePropValue::String(v) => PropValue::string(v),
      CorePropValue::VectorF32(v) => PropValue::vector(v.iter().map(|&x| x as f64).collect()),
    }
  }
}

impl From<PropValue> for CorePropValue {
  fn from(value: PropValue) -> Self {
    match value.prop_type.as_str() {
      "null" => CorePropValue::Null,
      "bool" => CorePropValue::Bool(value.bool_value.unwrap_or(false)),
      "int" => CorePropValue::I64(value.int_value.unwrap_or(0)),
      "float" => CorePropValue::F64(value.float_value.unwrap_or(0.0)),
      "string" => CorePropValue::String(value.string_value.unwrap_or_default()),
      "vector" => {
        let vector = value.vector_value.unwrap_or_default();
        CorePropValue::VectorF32(vector.iter().map(|&x| x as f32).collect())
      }
      _ => CorePropValue::Null,
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_prop_value_null() {
    let pv = PropValue::null();
    assert_eq!(pv.prop_type, "null");
  }

  #[test]
  fn test_prop_value_bool() {
    let pv = PropValue::bool(true);
    assert_eq!(pv.prop_type, "bool");
    assert_eq!(pv.bool_value, Some(true));
  }

  #[test]
  fn test_prop_value_int() {
    let pv = PropValue::int(42);
    assert_eq!(pv.prop_type, "int");
    assert_eq!(pv.int_value, Some(42));
  }

  #[test]
  fn test_prop_value_float() {
    let pv = PropValue::float_val(3.14);
    assert_eq!(pv.prop_type, "float");
    assert_eq!(pv.float_value, Some(3.14));
  }

  #[test]
  fn test_prop_value_string() {
    let pv = PropValue::string("hello".to_string());
    assert_eq!(pv.prop_type, "string");
    assert_eq!(pv.string_value, Some("hello".to_string()));
  }

  #[test]
  fn test_prop_value_vector() {
    let pv = PropValue::vector(vec![1.0, 2.0, 3.0]);
    assert_eq!(pv.prop_type, "vector");
    assert_eq!(pv.vector_value, Some(vec![1.0, 2.0, 3.0]));
  }

  #[test]
  fn test_core_conversion_roundtrip() {
    // Test int roundtrip
    let pv = PropValue::int(42);
    let core: CorePropValue = pv.into();
    let back: PropValue = core.into();
    assert_eq!(back.int_value, Some(42));

    // Test string roundtrip
    let pv = PropValue::string("test".to_string());
    let core: CorePropValue = pv.into();
    let back: PropValue = core.into();
    assert_eq!(back.string_value, Some("test".to_string()));
  }
}
