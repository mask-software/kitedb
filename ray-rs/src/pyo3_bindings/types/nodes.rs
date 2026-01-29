//! Node types for Python bindings

use super::values::PropValue;
use pyo3::prelude::*;

/// Node property key-value pair
#[pyclass(name = "NodeProp")]
#[derive(Debug, Clone)]
pub struct NodeProp {
  #[pyo3(get)]
  pub key_id: u32,
  #[pyo3(get)]
  pub value: PropValue,
}

#[pymethods]
impl NodeProp {
  #[new]
  fn new(key_id: u32, value: PropValue) -> Self {
    NodeProp { key_id, value }
  }

  fn __repr__(&self) -> String {
    format!("NodeProp(key_id={}, value={:?})", self.key_id, self.value)
  }
}

impl NodeProp {
  /// Create a new NodeProp (pub for crate use)
  pub fn create(key_id: u32, value: PropValue) -> Self {
    Self::new(key_id, value)
  }
}

/// Node entry with properties (used in streaming)
#[pyclass(name = "NodeWithProps")]
#[derive(Debug, Clone)]
pub struct NodeWithProps {
  #[pyo3(get)]
  pub id: i64,
  #[pyo3(get)]
  pub key: Option<String>,
  #[pyo3(get)]
  pub props: Vec<NodeProp>,
}

#[pymethods]
impl NodeWithProps {
  #[new]
  #[pyo3(signature = (id, key=None, props=vec![]))]
  fn new(id: i64, key: Option<String>, props: Vec<NodeProp>) -> Self {
    NodeWithProps { id, key, props }
  }

  fn __repr__(&self) -> String {
    format!(
      "NodeWithProps(id={}, key={:?}, props_count={})",
      self.id,
      self.key,
      self.props.len()
    )
  }
}

impl NodeWithProps {
  /// Create a new NodeWithProps (pub for crate use)
  pub fn create(id: i64, key: Option<String>, props: Vec<NodeProp>) -> Self {
    Self::new(id, key, props)
  }
}

/// Edge entry with properties (used in streaming)
#[pyclass(name = "EdgeWithProps")]
#[derive(Debug, Clone)]
pub struct EdgeWithProps {
  #[pyo3(get)]
  pub src: i64,
  #[pyo3(get)]
  pub etype: u32,
  #[pyo3(get)]
  pub dst: i64,
  #[pyo3(get)]
  pub props: Vec<NodeProp>,
}

#[pymethods]
impl EdgeWithProps {
  #[new]
  fn new(src: i64, etype: u32, dst: i64, props: Vec<NodeProp>) -> Self {
    EdgeWithProps {
      src,
      etype,
      dst,
      props,
    }
  }

  fn __repr__(&self) -> String {
    format!(
      "EdgeWithProps(src={}, etype={}, dst={}, props_count={})",
      self.src,
      self.etype,
      self.dst,
      self.props.len()
    )
  }
}

impl EdgeWithProps {
  /// Create a new EdgeWithProps (pub for crate use)
  pub fn create(src: i64, etype: u32, dst: i64, props: Vec<NodeProp>) -> Self {
    Self::new(src, etype, dst, props)
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_node_prop_creation() {
    let prop = NodeProp::create(1, PropValue::new_int(42));
    assert_eq!(prop.key_id, 1);
    assert_eq!(prop.value.int_value, Some(42));
  }

  #[test]
  fn test_node_with_props_creation() {
    let props = vec![NodeProp::create(
      1,
      PropValue::new_string("test".to_string()),
    )];
    let node = NodeWithProps::create(42, Some("user:1".to_string()), props);
    assert_eq!(node.id, 42);
    assert_eq!(node.key, Some("user:1".to_string()));
    assert_eq!(node.props.len(), 1);
  }

  #[test]
  fn test_edge_with_props_creation() {
    let props = vec![NodeProp::create(1, PropValue::new_float(1.5))];
    let edge = EdgeWithProps::create(1, 2, 3, props);
    assert_eq!(edge.src, 1);
    assert_eq!(edge.etype, 2);
    assert_eq!(edge.dst, 3);
    assert_eq!(edge.props.len(), 1);
  }
}
