//! Integrity checks for SingleFileDB.

use crate::types::CheckResult;

use super::SingleFileDB;

impl SingleFileDB {
  /// Check database integrity.
  ///
  /// Performs validation checks on the database structure:
  /// - Verifies edge endpoints exist
  /// - Validates edge existence via edge_exists
  /// - Compares list counts against count_* helpers
  pub fn check(&self) -> CheckResult {
    let mut errors = Vec::new();
    let mut warnings = Vec::new();

    let all_nodes = self.list_nodes();
    let node_count = all_nodes.len();

    if node_count == 0 {
      warnings.push("No nodes in database".to_string());
      return CheckResult {
        valid: true,
        errors,
        warnings,
      };
    }

    let all_edges = self.list_edges(None);
    let edge_count = all_edges.len();

    for edge in &all_edges {
      if !self.node_exists(edge.src) {
        errors.push(format!(
          "Edge references non-existent source node: {} -[{}]-> {}",
          edge.src, edge.etype, edge.dst
        ));
      }

      if !self.node_exists(edge.dst) {
        errors.push(format!(
          "Edge references non-existent destination node: {} -[{}]-> {}",
          edge.src, edge.etype, edge.dst
        ));
      }
    }

    for edge in &all_edges {
      let exists = self.edge_exists(edge.src, edge.etype, edge.dst);
      if !exists {
        errors.push(format!(
          "Edge inconsistency: edge {} -[{}]-> {} listed but not found via edge_exists",
          edge.src, edge.etype, edge.dst
        ));
      }
    }

    let counted_nodes = self.count_nodes();
    if counted_nodes != node_count {
      warnings.push(format!(
        "Node count mismatch: list_nodes returned {node_count} but count_nodes returned {counted_nodes}"
      ));
    }

    let counted_edges = self.count_edges();
    if counted_edges != edge_count {
      warnings.push(format!(
        "Edge count mismatch: list_edges returned {edge_count} but count_edges returned {counted_edges}"
      ));
    }

    CheckResult {
      valid: errors.is_empty(),
      errors,
      warnings,
    }
  }
}
