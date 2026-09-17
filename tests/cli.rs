use serde_json::{Value, json};
use std::{
    fs,
    path::Path,
    process::{Command, Output},
};

fn qd(source: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_qd"))
        .arg(source)
        .args(args)
        .output()
        .unwrap()
}

fn success(output: Output) -> Value {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn agent_read_patch_preview_save_and_failed_batch() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source.csv");
    let original = "id,name,note\r\n00123,Tallinn,\"line 1\nline 2\"\r\n00456,Tartu\r\n";
    fs::write(&source, original).unwrap();
    assert_eq!(success(qd(&source, &["--check"]))["records"], 3);
    assert_eq!(
        success(qd(&source, &["--read", "A2:C3"]))["rows"],
        json!([
            ["00123", "Tallinn", "line 1\nline 2"],
            ["00456", "Tartu", null]
        ])
    );
    let preview = success(qd(
        &source,
        &["--set", "B2", "changed", "--read", "B2", "--dry-run"],
    ));
    assert_eq!(
        preview["changes"],
        json!([{"cell":"B2", "before":"Tallinn", "after":"changed"}])
    );
    assert_eq!(preview["rows"], json!([["changed"]]));
    assert_eq!(preview["dry_run"], true);
    assert_eq!(
        success(qd(
            &source,
            &[
                "--set",
                "B2",
                "changed",
                "--set",
                "B2",
                "Tallinn",
                "--dry-run"
            ]
        ))["changes"],
        json!([])
    );
    let patch = dir.path().join("patch.json");
    fs::write(
        &patch,
        r#"[{"cell":"B2","value":"New, \"quoted\"\ncity 🦀"},{"cell":"A3","value":"00000"}]"#,
    )
    .unwrap();
    let dest = dir.path().join("edited.csv");
    success(qd(
        &source,
        &[
            "--apply",
            patch.to_str().unwrap(),
            "--output",
            dest.to_str().unwrap(),
        ],
    ));
    assert_eq!(
        success(qd(&dest, &["--read", "A2:C3"]))["rows"],
        json!([
            ["00123", "New, \"quoted\"\ncity 🦀", "line 1\nline 2"],
            ["00000", "Tartu", null]
        ])
    );
    let saved = fs::read(&dest).unwrap();
    assert!(
        !qd(&source, &["--output", dest.to_str().unwrap()])
            .status
            .success()
    );
    assert_eq!(fs::read(&dest).unwrap(), saved);
    let rejected = dir.path().join("rejected.csv");
    assert!(
        !qd(
            &source,
            &[
                "--set",
                "B2",
                "valid",
                "--set",
                "C3",
                "missing",
                "--output",
                rejected.to_str().unwrap()
            ]
        )
        .status
        .success()
    );
    assert!(!rejected.exists());
    for args in [
        vec!["--read", "A4"],
        vec!["--set", "B2", "lost"],
        vec!["--read", "A1:ZZZ10000"],
        vec!["--check", "--dry-run"],
    ] {
        assert!(!qd(&source, &args).status.success(), "{args:?}");
    }
    fs::write(&patch, r#"[{"cell":"B2","value":123}]"#).unwrap();
    assert!(
        !qd(&source, &["--apply", patch.to_str().unwrap(), "--dry-run"])
            .status
            .success()
    );
    fs::write(&patch, "[]").unwrap();
    assert!(
        !qd(&source, &["--apply", patch.to_str().unwrap()])
            .status
            .success()
    );
    let copy = dir.path().join("copy.csv");
    success(qd(&source, &["--output", copy.to_str().unwrap()]));
    assert_eq!(fs::read_to_string(copy).unwrap(), original);
    assert_eq!(fs::read_to_string(source).unwrap(), original);
}

#[test]
fn agent_sort_reads_and_exports_the_same_order() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source.csv");
    fs::write(&source, "id,amount\nx,10\ny,2\nz,2\n").unwrap();
    let preview = success(qd(
        &source,
        &["--sort", "B:n,-A", "--read", "A1:B4", "--dry-run"],
    ));
    assert_eq!(
        preview["rows"],
        json!([["id", "amount"], ["z", "2"], ["y", "2"], ["x", "10"]])
    );
    let dest = dir.path().join("sorted.csv");
    success(qd(
        &source,
        &[
            "--set",
            "B2",
            "1",
            "--sort",
            "B:n",
            "--output",
            dest.to_str().unwrap(),
        ],
    ));
    assert_eq!(
        success(qd(&dest, &["--read", "A2:B4"]))["rows"],
        json!([["x", "1"], ["y", "2"], ["z", "2"]])
    );
    assert!(
        !qd(&source, &["--sort", "B:n", "--no-header", "--dry-run"])
            .status
            .success()
    );
    assert!(!qd(&source, &["--sort", "A"]).status.success());
}
