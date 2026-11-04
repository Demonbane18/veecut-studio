//! `veecut sources --approve` is for a person at a terminal; agents run without one.

use std::process::{Command, Stdio};

use serde_json::json;
use veecut_engine::project::{Canvas, PROJECT_FILE, Project, VideoKind};

#[test]
fn sources_approve_without_a_terminal_is_refused() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    std::fs::create_dir_all(root.join(".veecut")).unwrap();
    std::fs::write(root.join(".veecut/workspace.toml"), "version = 1\n").unwrap();
    let dir = root.join("projects/demo");
    std::fs::create_dir_all(&dir).unwrap();
    let mut project = Project::new("Demo", VideoKind::default(), Canvas::new(1080, 1920));
    project.assets.push(
        serde_json::from_value(json!({
            "id": "asset-1", "path": "footage/a.mp4", "kind": "video", "role": "broll",
            "duration": 10.0, "width": 1080, "height": 1920, "fps": 30.0, "has_audio": false
        }))
        .unwrap(),
    );
    project.sources.insert(
        "asset-1".into(),
        serde_json::from_value(json!({
            "url": "https://example.org/a", "license": "CC BY-NC 2.0",
            "flag": true, "flag_reason": "non-commercial"
        }))
        .unwrap(),
    );
    project.save(&dir.join(PROJECT_FILE)).unwrap();
    let before = std::fs::read(dir.join(PROJECT_FILE)).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_veecut"))
        .arg("--workspace")
        .arg(root)
        .args(["-p", "demo", "sources", "--approve", "asset-1"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{stderr}");
    assert!(stderr.contains("your decision"), "{stderr}");
    assert_eq!(before, std::fs::read(dir.join(PROJECT_FILE)).unwrap());
}
