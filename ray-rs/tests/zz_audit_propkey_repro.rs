// Repro: dynamically created propkeys are not WAL-logged, so name->id mapping
// is lost on reopen without checkpoint.
use kitedb::api::kite::{Kite, KiteOptions, NodeDef, PropDef};
use kitedb::types::PropValue;
use std::collections::HashMap;

#[test]
fn propkey_mapping_lost_on_reopen_without_checkpoint() {
  let dir = tempfile::tempdir().unwrap();
  let path = dir.path().join("db");

  let schema = || {
    KiteOptions::new().node(
      NodeDef::new("User", "user:")
        .prop(PropDef::string("name"))
        .prop(PropDef::int("age")),
    )
    // default close_checkpoint_if_wal_usage_at_least = Some(0.2);
    // tiny WAL usage -> no checkpoint on close
  };

  let alice_id = {
    let mut kite = Kite::open(&path, schema()).unwrap();
    let mut props = HashMap::new();
    props.insert("name".to_string(), PropValue::String("Alice".into()));
    props.insert("age".to_string(), PropValue::I64(30));
    let alice = kite.create_node("User", "alice", props).unwrap();
    // also a dynamic (non-schema) prop
    kite
      .set_prop(alice.id(), "nickname", PropValue::String("Al".into()))
      .unwrap();
    let id = alice.id();
    kite.close().unwrap();
    id
  };

  for round in 0..10 {
    let kite = Kite::open(&path, schema()).unwrap();
    let name = kite.prop(alice_id, "name");
    let age = kite.prop(alice_id, "age");
    let nick = kite.prop(alice_id, "nickname");
    assert_eq!(
      name,
      Some(PropValue::String("Alice".into())),
      "round {round}: name corrupted: {name:?} age={age:?} nick={nick:?}"
    );
    assert_eq!(
      age,
      Some(PropValue::I64(30)),
      "round {round}: age corrupted"
    );
    assert_eq!(
      nick,
      Some(PropValue::String("Al".into())),
      "round {round}: dynamic prop lost"
    );
    kite.close().unwrap();
  }
}
