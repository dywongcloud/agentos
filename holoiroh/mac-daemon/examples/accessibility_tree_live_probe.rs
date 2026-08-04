use holoiroh_daemon::accessibility_tree::{
    BRIDGE_SNAPSHOT_TIMEOUT, SnapshotAttempt, SnapshotLimits, snapshot_frontmost_application,
};

fn main() {
    match snapshot_frontmost_application() {
        SnapshotAttempt::Captured { snapshot, elapsed } => {
            let limits = SnapshotLimits::default();
            assert!(snapshot.node_count > 0);
            assert_eq!(snapshot.byte_count, snapshot.json.len());
            assert!(snapshot.node_count <= limits.max_nodes);
            assert!(snapshot.byte_count <= limits.max_total_bytes);
            assert!(
                elapsed < BRIDGE_SNAPSHOT_TIMEOUT,
                "AX snapshot missed the 350ms bridge gate and must not be wired: {elapsed:?}"
            );

            let parsed: serde_json::Value =
                serde_json::from_str(&snapshot.json).expect("live snapshot JSON");
            assert_eq!(
                parsed["node_count"].as_u64().unwrap() as usize,
                snapshot.node_count
            );
            for node in parsed["nodes"].as_array().expect("nodes array") {
                let role = node["role"]
                    .as_str()
                    .unwrap_or_default()
                    .to_ascii_lowercase();
                let subrole = node
                    .get("subrole")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_ascii_lowercase();
                if role.contains("secure")
                    || role.contains("password")
                    || subrole.contains("secure")
                    || subrole.contains("password")
                {
                    assert!(
                        node.get("value").is_none(),
                        "password/secure text must be absent: {node}"
                    );
                }
            }

            println!(
                "accessibility_tree_live_probe: CAPTURED elapsed_ms={} nodes={} bytes={} truncated={}",
                elapsed.as_secs_f64() * 1_000.0,
                snapshot.node_count,
                snapshot.byte_count,
                snapshot.truncated
            );
        }
        SnapshotAttempt::Omitted { reason, elapsed } => {
            println!(
                "accessibility_tree_live_probe: OMITTED elapsed_ms={} reason={reason}",
                elapsed.as_secs_f64() * 1_000.0
            );
        }
    }
}
