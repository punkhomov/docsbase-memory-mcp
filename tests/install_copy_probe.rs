//! Temporary, manual macOS-only investigation for issue #113.
#![cfg(target_os = "macos")]

use std::fs;
use std::os::unix::{fs::MetadataExt, process::ExitStatusExt};
use std::process::{Command, Stdio};

#[test]
#[ignore = "manual copied-executable investigation; not a regression verdict"]
fn compare_launcher_copy_lifecycles() {
    let source = assert_cmd::cargo::cargo_bin!("docsbase");
    let output_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target/ci-observations/install-copy");
    fs::create_dir_all(&output_dir).expect("diagnostics");
    let mut records = Vec::new();
    let mut failures = 0;
    for mode in ["overwrite", "fresh-name", "unlink"] {
        let root = tempfile::TempDir::new().expect("launchers");
        for trial in 0..50 {
            let launcher = root.path().join(if mode == "fresh-name" {
                format!("docsbase-{trial}")
            } else {
                "docsbase".to_owned()
            });
            let old_inode = fs::metadata(&launcher).ok().map(|m| m.ino());
            if mode == "unlink" && launcher.exists() {
                fs::remove_file(&launcher).expect("unlink old launcher");
            }
            fs::copy(source, &launcher).expect("copy launcher");
            let metadata = fs::metadata(&launcher).expect("metadata");
            let child = Command::new(&launcher)
                .arg("--version")
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn();
            let result = match child {
                Ok(child) => {
                    let pid = child.id();
                    let output = child.wait_with_output().expect("wait");
                    serde_json::json!({
                        "pid": pid, "code": output.status.code(),
                        "signal": output.status.signal(), "success": output.status.success(),
                        "stdout": String::from_utf8_lossy(&output.stdout),
                        "stderr": String::from_utf8_lossy(&output.stderr),
                    })
                }
                Err(error) => {
                    serde_json::json!({"success": false, "spawn_error": error.to_string()})
                }
            };
            let success = result["success"] == true;
            let record = serde_json::json!({
                "mode": mode, "trial": trial, "old_inode": old_inode,
                "inode": metadata.ino(), "length": metadata.len(), "result": result,
            });
            eprintln!("{record}");
            records.push(record);
            fs::write(
                output_dir.join("results.json"),
                serde_json::to_vec_pretty(&records).expect("serialize"),
            )
            .expect("write records");
            if !success {
                failures += 1;
                let signature = Command::new("/usr/bin/codesign")
                    .args(["--verify", "--strict", "--verbose=4"])
                    .arg(&launcher)
                    .output()
                    .expect("codesign verify");
                eprintln!("signature after failure: {signature:?}");
                let log = Command::new("/usr/bin/log")
                    .args(["show", "--last", "3m", "--style", "compact", "--predicate",
                        "(process == 'kernel' OR process == 'amfid' OR process == 'taskgated-helper') AND eventMessage CONTAINS[c] 'docsbase'"])
                    .output().expect("unified log");
                fs::write(output_dir.join(format!("{mode}-system.log")), &log.stdout)
                    .expect("save log");
                eprintln!(
                    "unified log status={:?}, stderr={}",
                    log.status,
                    String::from_utf8_lossy(&log.stderr)
                );
                break;
            }
            // Keep the probe's disk use constant even for large debug CLIs.
            // The next fresh pathname is still distinct; recorded inode IDs
            // show whether the filesystem actually reuses an inode.
            if mode == "fresh-name" {
                fs::remove_file(&launcher).expect("remove successful fresh launcher");
            }
        }
    }
    assert_eq!(
        failures, 0,
        "copied-executable failures; inspect diagnostics"
    );
}
