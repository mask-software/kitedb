#!/usr/bin/env python3
"""
RayDB Python Example

This example demonstrates the main features of the RayDB Python bindings:
- Creating and managing a graph database
- Node and edge operations
- Property storage
- Graph traversal
- Pathfinding algorithms
"""

import os
import tempfile
from raydb import Database, PropValue


def main():
    # Create a temporary database for this example
    with tempfile.TemporaryDirectory() as tmpdir:
        db_path = os.path.join(tmpdir, "social_network.raydb")
        
        print("=" * 60)
        print("RayDB Python Example - Social Network")
        print("=" * 60)
        
        # Open database using context manager for automatic cleanup
        with Database(db_path) as db:
            # Start a transaction
            db.begin()
            
            # ----------------------------------------------------------------
            # Create schema (edge types and property keys)
            # ----------------------------------------------------------------
            print("\n1. Setting up schema...")
            
            follows = db.get_or_create_etype("follows")
            friend_of = db.get_or_create_etype("friend_of")
            
            name_key = db.get_or_create_propkey("name")
            age_key = db.get_or_create_propkey("age")
            bio_key = db.get_or_create_propkey("bio")
            weight_key = db.get_or_create_propkey("weight")
            
            print(f"   Edge types: follows={follows}, friend_of={friend_of}")
            print(f"   Property keys: name={name_key}, age={age_key}")
            
            # ----------------------------------------------------------------
            # Create users (nodes with properties)
            # ----------------------------------------------------------------
            print("\n2. Creating users...")
            
            users = {}
            user_data = [
                ("alice", "Alice Smith", 28),
                ("bob", "Bob Jones", 32),
                ("carol", "Carol White", 25),
                ("david", "David Brown", 35),
                ("eve", "Eve Davis", 29),
            ]
            
            for username, full_name, age in user_data:
                node_id = db.create_node(f"user:{username}")
                db.set_node_prop(node_id, name_key, PropValue.string(full_name))
                db.set_node_prop(node_id, age_key, PropValue.int(age))
                users[username] = node_id
                print(f"   Created user {username} (id={node_id})")
            
            # ----------------------------------------------------------------
            # Create relationships (edges)
            # ----------------------------------------------------------------
            print("\n3. Creating relationships...")
            
            # Follow relationships
            follow_edges = [
                ("alice", "bob"),
                ("alice", "carol"),
                ("bob", "david"),
                ("carol", "david"),
                ("david", "eve"),
                ("eve", "alice"),
            ]
            
            for src, dst in follow_edges:
                db.add_edge(users[src], follows, users[dst])
                print(f"   {src} follows {dst}")
            
            # Friendship relationships (bidirectional)
            friendships = [
                ("alice", "carol"),
                ("bob", "david"),
            ]
            
            for user1, user2 in friendships:
                db.add_edge(users[user1], friend_of, users[user2])
                db.add_edge(users[user2], friend_of, users[user1])
                print(f"   {user1} <-> {user2} are friends")
            
            # Commit the transaction
            db.commit()
            
            # ----------------------------------------------------------------
            # Query the database
            # ----------------------------------------------------------------
            print("\n4. Querying the database...")
            
            stats = db.stats()
            print(f"   Total nodes: {db.count_nodes()}")
            print(f"   Total edges: {db.count_edges()}")
            
            # Get Alice's profile
            alice_id = db.get_node_by_key("user:alice")
            alice_name = db.get_node_prop(alice_id, name_key)
            alice_age = db.get_node_prop(alice_id, age_key)
            print(f"\n   Alice's profile:")
            print(f"   - Name: {alice_name.string_value}")
            print(f"   - Age: {alice_age.int_value}")
            
            # Get who Alice follows (single hop)
            alice_follows = db.traverse_out(alice_id, follows)
            print(f"   - Alice follows {len(alice_follows)} users")
            
            # ----------------------------------------------------------------
            # Graph traversal (using direct database methods)
            # ----------------------------------------------------------------
            print("\n5. Graph traversal...")
            
            # Find users reachable from Alice in 2 hops via 'follows'
            print("\n   Users reachable from Alice in 2 'follows' hops:")
            reachable = db.traverse(
                users["alice"],
                max_depth=2,
                etype=follows,
                min_depth=1,
                direction="out",
            )
            
            for result in reachable:
                user_name = db.get_node_prop(result.node_id, name_key)
                if user_name:
                    print(f"   - {user_name.string_value} (depth={result.depth})")
            
            # Find all reachable nodes (any edge type)
            print("\n   All nodes reachable from Alice (max depth 3):")
            all_reachable = db.reachable_nodes(users["alice"], max_depth=3)
            for node_id in all_reachable:
                user_name = db.get_node_prop(node_id, name_key)
                if user_name:
                    print(f"   - {user_name.string_value}")
            
            # ----------------------------------------------------------------
            # Pathfinding (using direct database methods)
            # ----------------------------------------------------------------
            print("\n6. Pathfinding...")
            
            # Find shortest path from Alice to Eve using BFS
            print("\n   Shortest path from Alice to Eve (BFS):")
            path = db.find_path_bfs(users["alice"], users["eve"], etype=follows)
            
            if path.found:
                path_names = []
                for node_id in path.path:
                    name = db.get_node_prop(node_id, name_key)
                    if name:
                        path_names.append(name.string_value)
                print(f"   Path: {' -> '.join(path_names)}")
                print(f"   Path length: {len(path.path) - 1} hops")
            else:
                print("   No path found!")
            
            # Find shortest path using Dijkstra (for weighted graphs)
            print("\n   Shortest path from Alice to Eve (Dijkstra):")
            path_dijkstra = db.find_path_dijkstra(
                users["alice"], 
                users["eve"], 
                etype=follows
            )
            
            if path_dijkstra.found:
                path_names = []
                for node_id in path_dijkstra.path:
                    name = db.get_node_prop(node_id, name_key)
                    if name:
                        path_names.append(name.string_value)
                print(f"   Path: {' -> '.join(path_names)}")
                print(f"   Total weight: {path_dijkstra.total_weight}")
            else:
                print("   No path found!")
            
            # ----------------------------------------------------------------
            # Check connectivity
            # ----------------------------------------------------------------
            print("\n7. Connectivity analysis...")
            
            print("\n   Checking paths between all user pairs:")
            for src_name in users:
                for dst_name in users:
                    if src_name != dst_name:
                        has_path = db.has_path(
                            users[src_name], 
                            users[dst_name], 
                            etype=follows
                        )
                        symbol = "YES" if has_path else "no"
                        print(f"   {src_name:8} -> {dst_name:8}: {symbol}")
        
        print("\n" + "=" * 60)
        print("Example completed successfully!")
        print("=" * 60)


if __name__ == "__main__":
    main()
