//! Reproductions for audit findings J1-J3 (public Kite API).
//!
//! J1: traversal `count()` must equal `to_vec().len()` for every config.
//! J2: one property policy on every create/update path (keep non-schema props,
//!     enforce `required`, apply `default`, validate `prop_type`).
//! J3: node/edge prop updates must fail when the target does not exist.

use kitedb::api::kite::{BatchOp, BatchResult, EdgeDef, Kite, KiteOptions, NodeDef, PropDef};
use kitedb::api::traversal::{TraversalBuilder, TraversalDirection};
use kitedb::types::{ETypeId, Edge, NodeId, PropValue};
use kitedb::KiteError;
use std::collections::HashMap;
use tempfile::TempDir;

type Props = HashMap<String, PropValue>;

const PERSON: &str = "Person";
const UPSERT_BY_ID: NodeId = 4242;
const MISSING_NODE: NodeId = 999_999;

fn open(options: KiteOptions) -> (TempDir, Kite) {
  let dir = tempfile::tempdir().expect("tempdir");
  let kite = Kite::open(dir.path().join("audit-api.kitedb"), options).expect("open kite");
  (dir, kite)
}

fn props(entries: &[(&str, PropValue)]) -> Props {
  entries
    .iter()
    .map(|(name, value)| (name.to_string(), value.clone()))
    .collect()
}

fn s(value: &str) -> PropValue {
  PropValue::String(value.to_string())
}

fn graph_schema() -> KiteOptions {
  KiteOptions::new()
    .node(NodeDef::new("User", "user:"))
    .edge(EdgeDef::new("F"))
    .edge(EdgeDef::new("G"))
}

fn person_schema() -> KiteOptions {
  KiteOptions::new()
    .node(
      NodeDef::new(PERSON, "person:")
        .prop(PropDef::string("name").required())
        .prop(PropDef::int("age").default(PropValue::I64(18)))
        .prop(PropDef::float("score")),
    )
    .edge(EdgeDef::new("F"))
}

fn user(kite: &mut Kite, key: &str) -> NodeId {
  kite
    .create_node("User", key, HashMap::new())
    .expect("create user")
    .id()
}

fn person(kite: &mut Kite, key: &str) -> NodeId {
  kite
    .create_node(PERSON, key, props(&[("name", s(key))]))
    .expect("create person")
    .id()
}

fn raw_neighbors(
  kite: &Kite,
) -> impl Fn(NodeId, TraversalDirection, Option<ETypeId>) -> Vec<Edge> + '_ {
  move |node_id, direction, etype| {
    let db = kite.raw();
    let wanted = |edge_etype: ETypeId| etype.map_or(true, |e| e == edge_etype);
    let mut edges = Vec::new();
    if matches!(
      direction,
      TraversalDirection::Out | TraversalDirection::Both
    ) {
      for (edge_etype, dst) in db.out_edges(node_id) {
        if wanted(edge_etype) {
          edges.push(Edge {
            src: node_id,
            etype: edge_etype,
            dst,
          });
        }
      }
    }
    if matches!(direction, TraversalDirection::In | TraversalDirection::Both) {
      for (edge_etype, src) in db.in_edges(node_id) {
        if wanted(edge_etype) {
          edges.push(Edge {
            src,
            etype: edge_etype,
            dst: node_id,
          });
        }
      }
    }
    edges
  }
}

fn assert_no_failures(finding: &str, failures: &[String]) {
  assert!(
    failures.is_empty(),
    "{finding}: {} path(s) violate the contract:\n  {}",
    failures.len(),
    failures.join("\n  ")
  );
}

fn mentions(err: &KiteError, needle: &str) -> bool {
  err.to_string().to_lowercase().contains(needle)
}

fn is_edge_not_found(err: &KiteError) -> bool {
  mentions(err, "edge") && mentions(err, "not found")
}

// ============================================================================
// J1: count() must equal to_vec().len()
// ============================================================================

#[test]
fn audit_j1_count_two_hop_cycle_matches_to_vec() {
  let (_dir, mut kite) = open(graph_schema());
  let alice = user(&mut kite, "alice");
  let bob = user(&mut kite, "bob");
  kite.link(alice, "F", bob).expect("link alice->bob");
  kite.link(bob, "F", alice).expect("link bob->alice");

  let two_hops = || {
    kite
      .from(alice)
      .out(Some("F"))
      .expect("out F")
      .out(Some("F"))
      .expect("out F")
  };
  let ids = two_hops().to_vec();
  let count = two_hops().count();

  // Iterator semantics are the reference: the start node is already visited.
  assert_eq!(ids, Vec::<NodeId>::new(), "to_vec() semantics changed");
  assert_eq!(
    count,
    ids.len(),
    "from(alice).out(F).out(F): count()={count} but to_vec()={ids:?}"
  );
}

#[test]
fn audit_j1_count_multi_start_seeds_visited() {
  let (_dir, mut kite) = open(graph_schema());
  let a = user(&mut kite, "a");
  let b = user(&mut kite, "b");
  let c = user(&mut kite, "c");
  kite.link(a, "F", b).expect("link a->b");
  kite.link(b, "F", c).expect("link b->c");

  let hop = || kite.from_nodes(vec![a, b]).out(Some("F")).expect("out F");
  let ids = hop().to_vec();
  let count = hop().count();

  assert_eq!(ids, vec![c], "to_vec() semantics changed");
  assert_eq!(
    count,
    ids.len(),
    "from_nodes([a,b]).out(F): count()={count} but to_vec()={ids:?}"
  );
}

#[test]
fn audit_j1_count_respects_unique_false() {
  let (_dir, mut kite) = open(graph_schema());
  let a = user(&mut kite, "a");
  let b = user(&mut kite, "b");
  kite.link(a, "F", b).expect("link a-F->b");
  kite.link(a, "G", b).expect("link a-G->b");

  let hop = || TraversalBuilder::new(vec![a]).unique(false).out(None);
  let ids = hop().collect_node_ids(raw_neighbors(&kite));
  let count = hop().count(raw_neighbors(&kite));

  assert_eq!(ids, vec![b, b], "collect_node_ids() semantics changed");
  assert_eq!(
    count,
    ids.len(),
    "unique(false).out(None) over parallel F/G edges: count()={count} but collect_node_ids()={ids:?}"
  );
}

// ============================================================================
// J2: one property policy on every create/update path
// ============================================================================

#[derive(Clone, Copy, Debug)]
enum CreatePath {
  CreateNode,
  Insert,
  InsertMany,
  Upsert,
  UpsertMany,
  UpsertById,
  Batch,
  Transaction,
}

const CREATE_PATHS: [CreatePath; 8] = [
  CreatePath::CreateNode,
  CreatePath::Insert,
  CreatePath::InsertMany,
  CreatePath::Upsert,
  CreatePath::UpsertMany,
  CreatePath::UpsertById,
  CreatePath::Batch,
  CreatePath::Transaction,
];

fn create_via(
  kite: &mut Kite,
  path: CreatePath,
  key: &str,
  props: Props,
) -> kitedb::Result<NodeId> {
  match path {
    CreatePath::CreateNode => kite.create_node(PERSON, key, props).map(|n| n.id()),
    CreatePath::Insert => kite
      .insert(PERSON)?
      .values(key, props)?
      .returning()
      .map(|n| n.id()),
    CreatePath::InsertMany => {
      let nodes = kite
        .insert(PERSON)?
        .values_many(vec![(key, props)])?
        .returning()?;
      Ok(nodes[0].id())
    }
    CreatePath::Upsert => kite
      .upsert(PERSON)?
      .values(key, props)?
      .returning()
      .map(|n| n.id()),
    CreatePath::UpsertMany => {
      let nodes = kite
        .upsert(PERSON)?
        .values_many(vec![(key, props)])?
        .returning()?;
      Ok(nodes[0].id())
    }
    CreatePath::UpsertById => {
      kite
        .upsert_by_id(PERSON, UPSERT_BY_ID)?
        .set_all(props)
        .execute()?;
      Ok(UPSERT_BY_ID)
    }
    CreatePath::Batch => {
      let results = kite.batch(vec![BatchOp::CreateNode {
        node_type: PERSON.to_string(),
        key_suffix: key.to_string(),
        props,
      }])?;
      match results.as_slice() {
        [BatchResult::NodeCreated(node)] => Ok(node.id()),
        other => panic!("unexpected batch result: {other:?}"),
      }
    }
    CreatePath::Transaction => {
      kite.transaction(|ctx| ctx.create_node(PERSON, key, props).map(|n| n.id()))
    }
  }
}

fn node_was_created(kite: &Kite, path: CreatePath, key: &str) -> bool {
  match path {
    CreatePath::UpsertById => kite.exists(UPSERT_BY_ID),
    _ => kite.get(PERSON, key).expect("get person").is_some(),
  }
}

/// Expect a create to fail because of `prop`; the failed create must leave no node behind
/// and must not leave a transaction open.
fn expect_create_rejected(
  failures: &mut Vec<String>,
  path: CreatePath,
  key: &str,
  input: Props,
  prop: &str,
) {
  let (_dir, mut kite) = open(person_schema());
  match create_via(&mut kite, path, key, input) {
    Ok(id) => failures.push(format!(
      "{path:?}: create succeeded (node {id}), expected an error about `{prop}`"
    )),
    Err(err) => {
      if !mentions(&err, prop) {
        failures.push(format!("{path:?}: error does not mention `{prop}`: {err}"));
      }
      if node_was_created(&kite, path, key) {
        failures.push(format!("{path:?}: node exists after failed create"));
      }
      if let Err(err) = kite.create_node(PERSON, "after", props(&[("name", s("after"))])) {
        failures.push(format!("{path:?}: db unusable after failed create: {err}"));
      }
    }
  }
}

#[test]
fn audit_j2_non_schema_prop_kept_on_every_create_path() {
  let mut failures = Vec::new();
  for path in CREATE_PATHS {
    let (_dir, mut kite) = open(person_schema());
    let input = props(&[("name", s("Al")), ("nickname", s("Ally"))]);
    match create_via(&mut kite, path, "al", input) {
      Ok(id) => {
        let nickname = kite.prop(id, "nickname");
        if nickname != Some(s("Ally")) {
          failures.push(format!(
            "{path:?}: non-schema prop `nickname` = {nickname:?}, expected Some(\"Ally\")"
          ));
        }
      }
      Err(err) => failures.push(format!("{path:?}: create failed: {err}")),
    }
  }
  assert_no_failures("J2 non-schema props", &failures);
}

#[test]
fn audit_j2_missing_required_prop_errors_on_create() {
  let mut failures = Vec::new();
  for path in CREATE_PATHS {
    let input = props(&[("age", PropValue::I64(30))]);
    expect_create_rejected(&mut failures, path, "nameless", input, "name");
  }
  assert_no_failures("J2 required", &failures);
}

#[test]
fn audit_j2_default_applied_when_prop_absent_on_create() {
  let mut failures = Vec::new();
  for path in CREATE_PATHS {
    let (_dir, mut kite) = open(person_schema());
    match create_via(&mut kite, path, "kid", props(&[("name", s("Kid"))])) {
      Ok(id) => {
        let age = kite.prop(id, "age");
        if age != Some(PropValue::I64(18)) {
          failures.push(format!(
            "{path:?}: absent `age` = {age:?}, expected default Some(I64(18))"
          ));
        }
      }
      Err(err) => failures.push(format!("{path:?}: create without `age` failed: {err}")),
    }

    // An explicit value must win over the default.
    let (_dir, mut kite) = open(person_schema());
    let input = props(&[("name", s("Old")), ("age", PropValue::I64(70))]);
    match create_via(&mut kite, path, "old", input) {
      Ok(id) => {
        let age = kite.prop(id, "age");
        if age != Some(PropValue::I64(70)) {
          failures.push(format!(
            "{path:?}: explicit `age` = {age:?}, expected Some(I64(70))"
          ));
        }
      }
      Err(err) => failures.push(format!("{path:?}: create with `age` failed: {err}")),
    }
  }
  assert_no_failures("J2 default", &failures);
}

#[test]
fn audit_j2_prop_type_mismatch_errors_on_create() {
  let mut failures = Vec::new();
  for path in CREATE_PATHS {
    let input = props(&[("name", s("Typo")), ("age", s("old"))]);
    expect_create_rejected(&mut failures, path, "typo", input, "age");
  }
  assert_no_failures("J2 prop_type on create", &failures);
}

#[derive(Clone, Copy, Debug)]
enum UpdatePath {
  SetProp,
  SetProps,
  UpdateBuilder,
  Upsert,
  UpsertById,
  Batch,
  Transaction,
}

const UPDATE_PATHS: [UpdatePath; 7] = [
  UpdatePath::SetProp,
  UpdatePath::SetProps,
  UpdatePath::UpdateBuilder,
  UpdatePath::Upsert,
  UpdatePath::UpsertById,
  UpdatePath::Batch,
  UpdatePath::Transaction,
];

fn update_via(
  kite: &mut Kite,
  path: UpdatePath,
  node_id: NodeId,
  key: &str,
  prop: &str,
  value: PropValue,
) -> kitedb::Result<()> {
  match path {
    UpdatePath::SetProp => kite.set_prop(node_id, prop, value),
    UpdatePath::SetProps => kite.set_props(node_id, vec![(prop, value)]),
    UpdatePath::UpdateBuilder => kite.update_by_id(node_id)?.set(prop, value).execute(),
    UpdatePath::Upsert => kite
      .upsert(PERSON)?
      .values(key, props(&[(prop, value)]))?
      .execute(),
    UpdatePath::UpsertById => kite
      .upsert_by_id(PERSON, node_id)?
      .set(prop, value)
      .execute(),
    UpdatePath::Batch => kite
      .batch(vec![BatchOp::SetProp {
        node_id,
        prop_name: prop.to_string(),
        value,
      }])
      .map(|_| ()),
    UpdatePath::Transaction => kite.transaction(|ctx| ctx.set_prop(node_id, prop, value)),
  }
}

#[test]
fn audit_j2_prop_type_mismatch_errors_on_update() {
  let mut failures = Vec::new();
  for path in UPDATE_PATHS {
    let (_dir, mut kite) = open(person_schema());
    let input = props(&[("name", s("Pat")), ("age", PropValue::I64(30))]);
    let id = kite
      .create_node(PERSON, "pat", input)
      .expect("create pat")
      .id();
    match update_via(&mut kite, path, id, "pat", "age", s("old")) {
      Ok(()) => failures.push(format!(
        "{path:?}: setting int `age` to a string succeeded (now {:?})",
        kite.prop(id, "age")
      )),
      Err(err) => {
        if !mentions(&err, "age") {
          failures.push(format!("{path:?}: error does not mention `age`: {err}"));
        }
        let age = kite.prop(id, "age");
        if age != Some(PropValue::I64(30)) {
          failures.push(format!(
            "{path:?}: `age` changed to {age:?} by a failed update"
          ));
        }
      }
    }
  }
  assert_no_failures("J2 prop_type on update", &failures);
}

#[test]
fn audit_j2_int_float_coercion_only_when_lossless() {
  let mut failures = Vec::new();
  let (_dir, mut kite) = open(person_schema());

  // Lossless coercions are accepted and stored as the declared type.
  let input = props(&[
    ("name", s("Coerce")),
    ("age", PropValue::F64(4.0)),
    ("score", PropValue::I64(3)),
  ]);
  match kite.create_node(PERSON, "coerce", input) {
    Ok(node) => {
      let age = kite.prop(node.id(), "age");
      if age != Some(PropValue::I64(4)) {
        failures.push(format!(
          "int `age` from F64(4.0) = {age:?}, expected I64(4)"
        ));
      }
      let score = kite.prop(node.id(), "score");
      if score != Some(PropValue::F64(3.0)) {
        failures.push(format!(
          "float `score` from I64(3) = {score:?}, expected F64(3.0)"
        ));
      }
    }
    Err(err) => failures.push(format!("lossless int<->float create rejected: {err}")),
  }

  // Lossy coercions are type mismatches.
  let lossy_int = props(&[("name", s("Frac")), ("age", PropValue::F64(2.5))]);
  if let Ok(node) = kite.create_node(PERSON, "frac", lossy_int) {
    failures.push(format!(
      "create with int `age` = F64(2.5) succeeded (stored {:?})",
      kite.prop(node.id(), "age")
    ));
  }
  let big = (1i64 << 53) + 1;
  let lossy_float = props(&[("name", s("Big")), ("score", PropValue::I64(big))]);
  if let Ok(node) = kite.create_node(PERSON, "big", lossy_float) {
    failures.push(format!(
      "create with float `score` = I64(2^53+1) succeeded (stored {:?})",
      kite.prop(node.id(), "score")
    ));
  }
  let target = person(&mut kite, "target");
  if kite.set_prop(target, "age", PropValue::F64(2.5)).is_ok() {
    failures.push(format!(
      "set_prop int `age` = F64(2.5) succeeded (stored {:?})",
      kite.prop(target, "age")
    ));
  }

  assert_no_failures("J2 int<->float coercion", &failures);
}

// ============================================================================
// J3: prop updates on missing nodes/edges must error
// ============================================================================

#[test]
fn audit_j3_update_edge_on_missing_edge_errors() {
  let (_dir, mut kite) = open(person_schema());
  let a = person(&mut kite, "a");
  let b = person(&mut kite, "b");
  assert!(!kite.has_edge(a, "F", b).expect("has_edge"));

  let result = kite
    .update_edge(a, "F", b)
    .and_then(|update| update.set("w", PropValue::I64(9)).execute());
  let stray_before_link = kite.edge_prop(a, "F", b, "w").expect("edge_prop");

  kite.link(a, "F", b).expect("link a->b");
  let after_link = kite.edge_prop(a, "F", b, "w").expect("edge_prop");

  match &result {
    Ok(()) => panic!(
      "update_edge on a missing edge succeeded; edge_prop before link = {stray_before_link:?}, \
       after a later link = {after_link:?}"
    ),
    Err(err) => assert!(
      is_edge_not_found(err),
      "update_edge on a missing edge failed for the wrong reason: {err}"
    ),
  }
  assert_eq!(
    after_link, None,
    "fresh link(a,F,b) carries stray prop `w` from the failed update"
  );
}

/// Run `update` against a missing edge (src -F-> fresh node); it must fail with an edge-not-found
/// error, and a later link of that edge must not carry `prop`.
fn check_missing_edge_prop(
  kite: &mut Kite,
  failures: &mut Vec<String>,
  src: NodeId,
  label: &str,
  prop: &str,
  update: impl FnOnce(&mut Kite, NodeId) -> kitedb::Result<()>,
) {
  let dst = person(kite, label);
  match update(kite, dst) {
    Ok(()) => failures.push(format!("{label}: succeeded on a missing edge")),
    Err(err) if !is_edge_not_found(&err) => {
      failures.push(format!("{label}: failed for the wrong reason: {err}"))
    }
    Err(_) => {}
  }
  kite.link(src, "F", dst).expect("link");
  let stray = kite.edge_prop(src, "F", dst, prop).expect("edge_prop");
  if stray.is_some() {
    failures.push(format!(
      "{label}: later link carries stray `{prop}` = {stray:?}"
    ));
  }
}

#[test]
fn audit_j3_set_edge_prop_on_missing_edge_errors() {
  let (_dir, mut kite) = open(person_schema());
  let a = person(&mut kite, "a");
  let mut failures = Vec::new();

  check_missing_edge_prop(
    &mut kite,
    &mut failures,
    a,
    "set_edge_prop",
    "w1",
    |kite, dst| kite.set_edge_prop(a, "F", dst, "w1", PropValue::I64(1)),
  );
  check_missing_edge_prop(
    &mut kite,
    &mut failures,
    a,
    "set_edge_props",
    "w2",
    |kite, dst| kite.set_edge_props(a, "F", dst, props(&[("w2", PropValue::I64(2))])),
  );
  check_missing_edge_prop(
    &mut kite,
    &mut failures,
    a,
    "batch_set_edge_prop",
    "w3",
    |kite, dst| {
      kite
        .batch(vec![BatchOp::SetEdgeProp {
          src: a,
          edge_type: "F".to_string(),
          dst,
          prop_name: "w3".to_string(),
          value: PropValue::I64(3),
        }])
        .map(|_| ())
    },
  );
  check_missing_edge_prop(
    &mut kite,
    &mut failures,
    a,
    "batch_set_edge_props",
    "w4",
    |kite, dst| {
      kite
        .batch(vec![BatchOp::SetEdgeProps {
          src: a,
          edge_type: "F".to_string(),
          dst,
          props: props(&[("w4", PropValue::I64(4))]),
        }])
        .map(|_| ())
    },
  );

  // Existence is checked inside the tx: an edge linked earlier in the same batch counts.
  let c = person(&mut kite, "c");
  let same_batch = kite.batch(vec![
    BatchOp::Link {
      src: a,
      edge_type: "F".to_string(),
      dst: c,
    },
    BatchOp::SetEdgeProp {
      src: a,
      edge_type: "F".to_string(),
      dst: c,
      prop_name: "w5".to_string(),
      value: PropValue::I64(5),
    },
  ]);
  if let Err(err) = same_batch {
    failures.push(format!(
      "batch link+set_edge_prop on the new edge failed: {err}"
    ));
  } else if kite.edge_prop(a, "F", c, "w5").expect("edge_prop") != Some(PropValue::I64(5)) {
    failures.push("batch link+set_edge_prop did not set `w5`".to_string());
  }

  assert_no_failures("J3 edge props on missing edge", &failures);
}

#[test]
fn audit_j3_set_prop_on_missing_node_errors() {
  let (_dir, mut kite) = open(person_schema());
  let deleted = person(&mut kite, "deleted");
  assert!(kite.delete_node(deleted).expect("delete node"));
  let mut failures = Vec::new();

  let mut expect_not_found = |label: &str, node_id: NodeId, result: kitedb::Result<()>| match result
  {
    Err(KiteError::NodeNotFound(id)) if id == node_id => {}
    Err(err) => failures.push(format!("{label}: wrong error for node {node_id}: {err}")),
    Ok(()) => failures.push(format!("{label}: succeeded on missing node {node_id}")),
  };

  for node_id in [MISSING_NODE, deleted] {
    expect_not_found(
      "set_prop",
      node_id,
      kite.set_prop(node_id, "nickname", s("ghost")),
    );
    expect_not_found(
      "set_props",
      node_id,
      kite.set_props(node_id, vec![("nickname", s("ghost"))]),
    );
    expect_not_found(
      "batch_set_prop",
      node_id,
      kite
        .batch(vec![BatchOp::SetProp {
          node_id,
          prop_name: "nickname".to_string(),
          value: s("ghost"),
        }])
        .map(|_| ()),
    );
    expect_not_found(
      "tx_set_prop",
      node_id,
      kite.transaction(|ctx| ctx.set_prop(node_id, "nickname", s("ghost"))),
    );
  }

  // Existence is checked inside the tx: a node created earlier in the same tx counts.
  let same_tx = kite.transaction(|ctx| {
    let node = ctx.create_node(PERSON, "fresh", props(&[("name", s("Fresh"))]))?;
    ctx.set_prop(node.id(), "nickname", s("F"))?;
    Ok(node.id())
  });
  match same_tx {
    Ok(id) if kite.prop(id, "nickname") == Some(s("F")) => {}
    Ok(id) => failures.push(format!(
      "tx create+set_prop: `nickname` = {:?}",
      kite.prop(id, "nickname")
    )),
    Err(err) => failures.push(format!("tx create+set_prop failed: {err}")),
  }

  assert_no_failures("J3 node props on missing node", &failures);
}
