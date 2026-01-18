//! Python bindings for RayDB using PyO3
//!
//! Exposes SingleFileDB and related types to Python.

#[cfg(feature = "python")]
pub mod database;
#[cfg(feature = "python")]
pub mod traversal;
#[cfg(feature = "python")]
pub mod vector;

#[cfg(feature = "python")]
pub use database::*;
#[cfg(feature = "python")]
pub use traversal::*;
#[cfg(feature = "python")]
pub use vector::*;

#[cfg(feature = "python")]
use pyo3::prelude::*;

/// RayDB Python module
#[cfg(feature = "python")]
#[pymodule]
#[pyo3(name = "_raydb")]
pub fn raydb(m: &Bound<'_, PyModule>) -> PyResult<()> {
  // Database classes
  m.add_class::<database::PyDatabase>()?;
  m.add_class::<database::PyOpenOptions>()?;
  m.add_class::<database::PySyncMode>()?;
  m.add_class::<database::PyDbStats>()?;
  m.add_class::<database::PyCacheStats>()?;
  m.add_class::<database::PyPropValue>()?;
  m.add_class::<database::PyEdge>()?;
  m.add_class::<database::PyFullEdge>()?;
  m.add_class::<database::PyNodeProp>()?;

  // Traversal result classes
  m.add_class::<traversal::PyTraversalResult>()?;
  m.add_class::<traversal::PyPathResult>()?;
  m.add_class::<traversal::PyPathEdge>()?;

  // Vector search classes
  m.add_class::<vector::PyIvfIndex>()?;
  m.add_class::<vector::PyIvfPqIndex>()?;
  m.add_class::<vector::PyIvfConfig>()?;
  m.add_class::<vector::PyPqConfig>()?;
  m.add_class::<vector::PySearchOptions>()?;
  m.add_class::<vector::PySearchResult>()?;
  m.add_class::<vector::PyIvfStats>()?;

  // Standalone functions
  m.add_function(wrap_pyfunction!(database::open_database, m)?)?;
  m.add_function(wrap_pyfunction!(version, m)?)?;
  m.add_function(wrap_pyfunction!(vector::brute_force_search, m)?)?;

  Ok(())
}

/// Get RayDB version
#[cfg(feature = "python")]
#[pyfunction]
pub fn version() -> String {
  env!("CARGO_PKG_VERSION").to_string()
}
