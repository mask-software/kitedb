#!/usr/bin/env python3
"""
Fluent Query Example for RayDB Python API

This example demonstrates the new fluent API for RayDB,
matching the TypeScript example in example/fluent-query.ts.

Usage:
    python fluent_query.py
"""

import tempfile
import shutil
from raydb import ray, define_node, define_edge, prop, optional


# =============================================================================
# Schema Definition
# =============================================================================

# Define user node type
user = define_node("user",
    key=lambda id: f"user:{id}",
    props={
        "name": prop.string("name"),
        "email": prop.string("email"),
        "age": optional(prop.int("age")),
    }
)

# Define company node type
company = define_node("company",
    key=lambda id: f"company:{id}",
    props={
        "name": prop.string("name"),
        "founded": prop.int("founded"),
    }
)

# Define edge types
knows = define_edge("knows", {
    "since": prop.int("since"),
})

works_at = define_edge("worksAt", {
    "role": prop.string("role"),
    "start_date": prop.int("startDate"),
})


# =============================================================================
# Main Example
# =============================================================================

def main():
    # Create a temporary directory for the database
    dir = tempfile.mkdtemp(prefix="ray-example-")
    db_path = f"{dir}/example.raydb"
    
    try:
        # Open database with schema
        db = ray(db_path, nodes=[user, company], edges=[knows, works_at])
        
        try:
            # Insert Alice
            alice = db.insert(user).values(
                key="alice",
                name="Alice",
                email="alice@example.com",
                age=30,
            ).returning()
            
            print(f"Created Alice: {alice.key}, name={alice.name}")
            
            # Insert Bob
            bob = db.insert(user).values(
                key="bob",
                name="Bob",
                email="bob@example.com",
                age=25,
            ).returning()
            
            print(f"Created Bob: {bob.key}, name={bob.name}")
            
            # Insert Acme company
            acme = db.insert(company).values(
                key="acme",
                name="Acme Co",
                founded=1999,
            ).returning()
            
            print(f"Created Acme: {acme.key}, name={acme.name}")
            
            # Create edges
            db.link(alice, knows, bob, since=2020)
            print("Linked: Alice knows Bob (since 2020)")
            
            db.link(alice, works_at, acme, role="Engineer", start_date=2022)
            print("Linked: Alice works at Acme (as Engineer)")
            
            # Update Alice's email using where clause
            db.update(user).set(email="alice@new.com").where(key="user:alice").execute()
            print("Updated Alice's email via where clause")
            
            # Update Alice's age using node reference
            db.update(alice).set(age=31).execute()
            print("Updated Alice's age via node reference")
            
            # =============================================================
            # Traversal Examples (with lazy loading optimization)
            # =============================================================
            
            # Fastest: get just IDs
            friend_ids = db.from_(alice).out(knows).ids()
            print(f"Friend IDs: {friend_ids}")
            
            # Fast: get keys only
            friend_keys = db.from_(alice).out(knows).keys()
            print(f"Friend keys: {friend_keys}")
            
            # Default: NodeRefs without properties (fast)
            friends = db.from_(alice).out(knows).to_list()
            print(f"Friends (no props): {[f.key for f in friends]}")
            
            # With specific properties (selective loading)
            friends_with_name = db.from_(alice).out(knows).load_props("name").to_list()
            print(f"Friends with name: {[(f.key, f.name) for f in friends_with_name]}")
            
            # With all properties (slower but complete)
            friends_full = db.from_(alice).out(knows).with_props().to_list()
            print(f"Friends full: {[(f.key, f.name, f.age) for f in friends_full]}")
            
            # With filter (automatically loads properties for predicate)
            young_friends = (
                db.from_(alice)
                .out(knows)
                .where_node(lambda n: n.age is not None and n.age < 35)
                .to_list()
            )
            print(f"Young friends: {[f.key for f in young_friends]}")
            
            # Verify Bob's age
            refreshed_bob = db.get(user, "bob")
            if refreshed_bob:
                print(f"Bob's age: {refreshed_bob.age}")
            
            # Delete Bob
            db.delete(user).where(key="user:bob").execute()
            print("Deleted Bob")
            
            # Verify Bob is gone
            if not db.get(user, "bob"):
                print("Bob successfully deleted")
            
            # Count remaining users
            user_count = db.count(user)
            print(f"Remaining users: {user_count}")
            
            # =============================================================
            # Transaction Batching Example
            # =============================================================
            print("\n--- Transaction Batching ---")
            
            # Test transaction context manager (batches multiple operations)
            with db.transaction():
                carol = db.insert(user).values(
                    key="carol",
                    name="Carol",
                    email="carol@example.com",
                    age=28,
                ).returning()
                print(f"Created Carol in transaction: {carol.key}")
                
                dave = db.insert(user).values(
                    key="dave",
                    name="Dave",
                    email="dave@example.com",
                    age=35,
                ).returning()
                print(f"Created Dave in transaction: {dave.key}")
                
                # This should work now - link inside transaction
                db.link(carol, knows, dave, since=2023)
                print("Linked Carol -> Dave inside transaction")
                
                # Update inside transaction
                db.update(carol).set(age=29).execute()
                print("Updated Carol's age inside transaction")
            
            print("Transaction committed successfully!")
            
            # Verify the data persisted
            final_count = db.count(user)
            print(f"Final user count: {final_count}")
            
        finally:
            db.close()
            
    finally:
        # Clean up temp directory
        shutil.rmtree(dir, ignore_errors=True)


if __name__ == "__main__":
    main()
