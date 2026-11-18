//! Cross-crate integration tests for the memory crate.
//!
//! Exercises the SQLite-backed TaskGraph checkpoint round-trip: save a
//! graph with tasks in various states to a fresh database, open a new
//! connection, load it back, and verify that status, results, and
//! dependency edges survived the serialization.

use requiem_core::task::{Task, TaskGraph, TaskStatus};
use requiem_core::Id;
use requiem_memory::Session;
use uuid::Uuid;

/// Build a realistic task graph:
///   A ──► B ──► D
///   A ──► C
/// A and B are completed, C is running, D is available (its dep B is done).
fn build_graph() -> (TaskGraph, Id, Id, Id, Id) {
    let mut graph = TaskGraph::default();

    let a = graph.add(Task::new("A: initial setup"));
    let b = graph.add(Task::new("B: core logic").depends_on(a.clone()));
    let c = graph.add(Task::new("C: parallel work").depends_on(a.clone()));
    let d = graph.add(Task::new("D: integration").depends_on(b.clone()));

    // Advance the graph through its lifecycle.
    graph.start(a.clone());
    graph.complete(a.clone(), serde_json::json!({"status": "ok"}));
    graph.start(b.clone());
    graph.complete(b.clone(), serde_json::json!({"lines": 42}));
    graph.start(c.clone());
    // c remains Running; d is unlocked because b completed.

    (graph, a, b, c, d)
}

#[test]
fn task_graph_checkpoint_roundtrip_via_sqlite() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("checkpoint.db");

    // Phase 1: save.
    let session = Session::open(&db_path).unwrap();
    let (graph, a, b, c, d) = build_graph();
    let session_id = Uuid::new_v4();

    session.save_task_graph(&session_id, &graph).unwrap();

    // Verify d was unlocked by b's completion (dependency reevaluate).
    assert_eq!(
        graph.get(&d).status,
        TaskStatus::Available,
        "d must be available after b completed"
    );
    assert_eq!(
        graph.get(&c).status,
        TaskStatus::Running,
        "c is still running"
    );

    // Phase 2: load in a fresh connection (simulates crash-restart).
    drop(session);
    let reloaded_session = Session::open(&db_path).unwrap();
    let loaded = reloaded_session
        .load_task_graph(&session_id)
        .unwrap()
        .expect("checkpoint must exist");

    // Structure survived.
    assert_eq!(loaded.len(), graph.len());

    // Per-task status survived.
    assert_eq!(loaded.get(&a).status, TaskStatus::Completed);
    assert_eq!(loaded.get(&b).status, TaskStatus::Completed);
    assert_eq!(loaded.get(&c).status, TaskStatus::Running);
    assert_eq!(loaded.get(&d).status, TaskStatus::Available);

    // Result payload survived.
    let a_result = loaded.get(&a).result.as_ref().expect("a has a result");
    assert_eq!(a_result, &serde_json::json!({"status": "ok"}));

    // Dependency edges survived.
    assert!(loaded.get(&b).dependencies.contains(&a));
    assert!(loaded.get(&c).dependencies.contains(&a));
    assert!(loaded.get(&d).dependencies.contains(&b));
}

#[test]
fn load_task_graph_none_for_missing_session() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("empty.db");
    let session = Session::open(&db_path).unwrap();
    let missing = Uuid::new_v4();

    let result = session.load_task_graph(&missing).unwrap();
    assert!(result.is_none());
}

#[test]
fn overwrite_checkpoint_replaces_prior_graph() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("overwrite.db");
    let session = Session::open(&db_path).unwrap();
    let sid = Uuid::new_v4();

    // First checkpoint: one task.
    let mut g1 = TaskGraph::default();
    g1.add(Task::new("task 1"));
    session.save_task_graph(&sid, &g1).unwrap();

    // Second checkpoint: two tasks, first completed.
    let mut g2 = TaskGraph::default();
    let t1 = g2.add(Task::new("task 1"));
    g2.add(Task::new("task 2").depends_on(t1.clone()));
    g2.complete(t1.clone(), serde_json::json!(null));
    session.save_task_graph(&sid, &g2).unwrap();

    // Reload: must see g2, not g1.
    let loaded = session.load_task_graph(&sid).unwrap().unwrap();
    assert_eq!(loaded.len(), 2);
    assert_eq!(loaded.get(&t1).status, TaskStatus::Completed);
}
