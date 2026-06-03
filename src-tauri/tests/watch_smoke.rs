// Empirical check: does the notify watcher actually deliver an event for an
// EXTERNAL content modification on this platform, and does its EventKind land
// on the Create/Remove/Modify(_) arms the watcher in commands.rs matches?
// This is the load-bearing assumption behind the "reload-on-external-change"
// feature: if a pure content write is swallowed, no frontend change can help.

use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::sync::mpsc;
use std::time::Duration;

#[test]
fn external_content_modify_fires_a_matched_event() {
    let dir = std::env::temp_dir().join(format!("lite_editor_watch_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("note.md");
    std::fs::write(&file, b"original\n").unwrap();

    let (tx, rx) = mpsc::channel::<Event>();
    let mut watcher: RecommendedWatcher = notify::recommended_watcher(move |res| {
        if let Ok(event) = res {
            let _ = tx.send(event);
        }
    })
    .unwrap();
    watcher.watch(&dir, RecursiveMode::Recursive).unwrap();

    // Give the watcher a beat to arm before mutating (FSEvents warm-up).
    std::thread::sleep(Duration::from_millis(300));

    // External content modification (not a create/delete).
    std::fs::write(&file, b"changed by an external tool\n").unwrap();

    // Drain events for up to ~3s.
    let mut kinds = Vec::new();
    let mut matched_with_path = false;
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while std::time::Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(event) => {
                let on_arm = matches!(
                    event.kind,
                    EventKind::Create(_) | EventKind::Remove(_) | EventKind::Modify(_)
                );
                let touches_file = event.paths.iter().any(|p| p.ends_with("note.md"));
                eprintln!(
                    "event kind={:?} on_matched_arm={} paths={:?}",
                    event.kind, on_arm, event.paths
                );
                kinds.push(event.kind);
                if on_arm && touches_file {
                    matched_with_path = true;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if matched_with_path {
                    break;
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    let _ = std::fs::remove_dir_all(&dir);

    assert!(
        !kinds.is_empty(),
        "watcher delivered NO events for an external content write — the modify is being swallowed at the OS layer"
    );
    assert!(
        matched_with_path,
        "watcher fired but no event matched Create/Remove/Modify(_) for note.md; kinds seen: {:?}",
        kinds
    );
}
