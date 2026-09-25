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

fn failure(output: Output) -> String {
    assert!(
        !output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(output.stdout.is_empty());
    String::from_utf8(output.stderr).unwrap()
}

#[test]
fn agent_find_options_and_output_shape() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source.csv");
    fs::write(
        &source,
        "id,city,note\n1,Tallinn,x\n2,Tartu,\n3,tallinn-Nõmme,Tallinn\n4\n",
    )
    .unwrap();
    let found = success(qd(&source, &["--find", "Tallinn"]));
    assert_eq!(
        found,
        json!({
            "source": fs::canonicalize(&source).unwrap().to_string_lossy(),
            "find": {"text": "Tallinn", "columns": null, "exact": false, "ignore_case": false, "from": "A1", "limit": 100},
            "coordinates": "source",
            "matches": [{"cell": "B2", "value": "Tallinn"}, {"cell": "C4", "value": "Tallinn"}],
            "next": null,
            "records_scanned": 5
        })
    );
    let page = success(qd(
        &source,
        &[
            "--find",
            "TALLINN",
            "--ignore-case",
            "--columns",
            "c,b,B",
            "--from",
            "b2",
            "--limit",
            "2",
        ],
    ));
    assert_eq!(
        page["find"],
        json!({"text": "TALLINN", "columns": ["B", "C"], "exact": false, "ignore_case": true, "from": "B2", "limit": 2})
    );
    assert_eq!(
        page["matches"],
        json!([{"cell": "B2", "value": "Tallinn"}, {"cell": "B4", "value": "tallinn-Nõmme"}])
    );
    assert_eq!(page["next"], "C4");
    assert_eq!(page["records_scanned"], 3);
    let rest = success(qd(
        &source,
        &[
            "--find",
            "TALLINN",
            "--ignore-case",
            "--columns",
            "B,C",
            "--from",
            "C4",
            "--limit",
            "2",
        ],
    ));
    assert_eq!(rest["matches"], json!([{"cell": "C4", "value": "Tallinn"}]));
    assert_eq!(rest["next"], Value::Null);
    let empty = success(qd(&source, &["--exact", "--find", ""]));
    assert_eq!(empty["matches"], json!([{"cell": "C3", "value": ""}]));
    assert_eq!(empty["find"]["exact"], true);
    let exact = success(qd(
        &source,
        &["--find", "Tallinn", "--exact", "--columns", "B"],
    ));
    assert_eq!(
        exact["matches"],
        json!([{"cell": "B2", "value": "Tallinn"}])
    );
    let last = success(qd(
        &source,
        &["--find", "4", "--limit", "1", "--from", "A5"],
    ));
    assert_eq!(
        (&last["next"], &last["records_scanned"]),
        (&json!("B5"), &json!(1))
    );
    assert_eq!(
        success(qd(&source, &["--find", "4", "--from", "B5"]))["matches"],
        json!([])
    );
    assert_eq!(
        success(qd(&source, &["--find", "a", "--limit", "10000"]))["find"]["limit"],
        10000
    );
}

#[test]
fn agent_find_with_edits_and_sort() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source.csv");
    fs::write(&source, "name,city\nc,Tartu\na,Tallinn\nb,Tallinn\n").unwrap();
    let edited = success(qd(
        &source,
        &[
            "--set",
            "B2",
            "Tallinn",
            "--set",
            "B3",
            "Narva",
            "--find",
            "Tallinn",
            "--dry-run",
        ],
    ));
    assert_eq!(
        edited["matches"],
        json!([{"cell": "B2", "value": "Tallinn"}, {"cell": "B4", "value": "Tallinn"}])
    );
    assert_eq!(edited["dry_run"], true);
    assert_eq!(edited["changes"].as_array().unwrap().len(), 2);
    assert_eq!(edited["coordinates"], "source");
    let sorted = success(qd(&source, &["--sort", "A", "--find", "Tallinn"]));
    assert_eq!(sorted["coordinates"], "sorted_view");
    assert_eq!(
        sorted["matches"],
        json!([{"cell": "B2", "value": "Tallinn"}, {"cell": "B3", "value": "Tallinn"}])
    );
    assert_eq!(
        sorted["sort"],
        json!({"columns": "A", "header": true, "changes_coordinates": "source", "read_coordinates": "sorted_view"})
    );
    assert!(sorted.get("changes").is_none() && sorted.get("dry_run").is_none());
    let both = success(qd(
        &source,
        &[
            "--set",
            "B2",
            "Tallinn",
            "--sort",
            "A",
            "--no-header",
            "--find",
            "Tallinn",
            "--dry-run",
        ],
    ));
    assert_eq!(
        both["matches"],
        json!([{"cell": "B1", "value": "Tallinn"}, {"cell": "B2", "value": "Tallinn"}, {"cell": "B3", "value": "Tallinn"}])
    );
    assert_eq!(both["sort"]["header"], false);
}

#[test]
fn agent_find_does_not_wait_for_the_full_scan() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source.csv");
    fs::write(&source, b"needle\nhay\n\xff\n").unwrap();
    let early = success(qd(&source, &["--find", "needle", "--limit", "1"]));
    assert_eq!(early["matches"], json!([{"cell": "A1", "value": "needle"}]));
    assert!(failure(qd(&source, &["--find", "needle"])).contains("UTF-8"));
}

#[test]
fn agent_find_rejects_invalid_options() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source.csv");
    fs::write(&source, "a,b\nc,d\n").unwrap();
    let out = dir.path().join("out.csv");
    let out = out.to_str().unwrap();
    for (args, message) in [
        (vec!["--exact"], "require --find"),
        (vec!["--ignore-case"], "require --find"),
        (vec!["--columns", "A"], "require --find"),
        (vec!["--from", "A1"], "require --find"),
        (vec!["--limit", "5"], "require --find"),
        (vec!["--find", ""], "--exact"),
        (vec!["--find"], "Missing value for --find"),
        (
            vec!["--find", "a", "--columns"],
            "Missing value for --columns",
        ),
        (vec!["--find", "a", "--from"], "Missing value for --from"),
        (vec!["--find", "a", "--limit"], "Missing value for --limit"),
        (vec!["--find", "a", "--read", "A1"], "cannot be combined"),
        (vec!["--find", "a", "--output", out], "cannot be combined"),
        (vec!["--find", "a", "--check"], "cannot be combined"),
        (vec!["--find", "a", "--set", "A1", "x"], "--dry-run"),
        (vec!["--find", "a", "--limit", "0"], "1 to 10,000"),
        (vec!["--find", "a", "--limit", "10001"], "1 to 10,000"),
        (vec!["--find", "a", "--limit", "-1"], "1 to 10,000"),
        (vec!["--find", "a", "--limit", "many"], "1 to 10,000"),
        (vec!["--find", "a", "--columns", ""], "column letters"),
        (vec!["--find", "a", "--columns", "A,,B"], "column letters"),
        (vec!["--find", "a", "--columns", "A, B"], "column letters"),
        (vec!["--find", "a", "--columns", "A,"], "column letters"),
        (vec!["--find", "a", "--columns", "B1"], "column letters"),
        (
            vec!["--find", "a", "--columns", "ZZZZZZZZZZZZZZZZZZZZZZZZ"],
            "too large",
        ),
        (vec!["--find", "a", "--from", "A0"], "Rows start at 1"),
        (
            vec!["--find", "a", "--from", "ZZZZZZZZZZZZZZZZZZZZZZZZ1"],
            "too large",
        ),
        (vec!["--find", "a", "--from", "B"], "cell address"),
        (
            vec!["--find", "a", "--no-header"],
            "--no-header requires --sort",
        ),
        (vec!["--find", "a", "--sort", "A,A"], "only appear once"),
        (vec!["--find", "a", "--sheets"], "--sheets by itself"),
    ] {
        let stderr = failure(qd(&source, &args));
        assert!(stderr.contains(message), "{args:?}: {stderr}");
    }
}

#[test]
fn csv_wider_than_xfd_reads_edits_and_pages() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("wide.csv");
    let mut record = vec![String::new(); 16_390];
    record[16_383] = "hit".into(); // XFD
    record[16_385] = "hit".into(); // XFF
    fs::write(&source, format!("{}\nhit\n", record.join(","))).unwrap();
    assert_eq!(
        success(qd(&source, &["--read", "XFF1"]))["rows"],
        json!([["hit"]])
    );
    let edited = success(qd(&source, &["--set", "XFE1", "x", "--dry-run"]));
    assert_eq!(
        edited["changes"],
        json!([{"cell": "XFE1", "before": "", "after": "x"}])
    );
    let mut from = "A1".to_owned();
    let mut cells = Vec::new();
    loop {
        let page = success(qd(
            &source,
            &["--find", "hit", "--limit", "1", "--from", &from],
        ));
        cells.extend(
            page["matches"]
                .as_array()
                .unwrap()
                .iter()
                .map(|m| m["cell"].clone()),
        );
        match page["next"].as_str() {
            Some(next) => from = next.to_owned(),
            None => break,
        }
    }
    assert_eq!(cells, [json!("XFD1"), json!("XFF1"), json!("A2")]);
}
