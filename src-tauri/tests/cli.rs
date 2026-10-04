//! The c2pa-iscc command-line binary: inspect and sign a fixture, timestamp fallback, error
//! handling. No test goes online: signing passes --no-timestamp or a closed local port.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn cli(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_c2pa-iscc"))
        .args(args)
        .output()
        .expect("the CLI runs")
}

fn json(output: &Output) -> Value {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("stdout is JSON")
}

#[test]
fn inspect_prints_the_inspection() {
    let path = fixture("no_manifest.jpg");
    let inspection = json(&cli(&["inspect", path.to_str().unwrap(), "--no-preview"]));
    assert_eq!(inspection["kind"], "image");
    assert_eq!(inspection["preview"], "");
    assert!(inspection["manifest"].is_null());
    assert_eq!(inspection["iscc"].as_array().unwrap().len(), 4);
    assert_eq!(inspection["meta_fields"]["name"], "no manifest");

    let compact = cli(&["--compact", "inspect", path.to_str().unwrap()]);
    let stdout = String::from_utf8(compact.stdout).unwrap();
    assert_eq!(stdout.trim_end().lines().count(), 1, "one line");
    assert!(
        stdout.contains("data:image/jpeg;base64,"),
        "preview included"
    );
}

#[test]
fn sign_writes_a_trusted_copy_with_prefilled_fields() {
    let dir = std::env::temp_dir().join("iscc-c2pa-demo-test-cli");
    std::fs::create_dir_all(&dir).unwrap();
    let output = dir.join("cli-signed.jpg");
    let _ = std::fs::remove_file(&output);
    let result = json(&cli(&[
        "sign",
        fixture("no_manifest.jpg").to_str().unwrap(),
        "--output",
        output.to_str().unwrap(),
        "--description",
        "Signed on the command line",
        "--training",
        "cawg.ai_training=notAllowed",
        "--training",
        "cawg.data_mining=constrained:research only",
        "--no-timestamp",
        "--no-preview",
    ]));
    assert!(output.exists());
    assert_eq!(result["timestamp"]["status"], "off");
    assert_eq!(
        result["inspection"]["manifest"]["signature"]["timestamp"]["status"],
        "none"
    );
    assert_eq!(result["units"].as_array().unwrap().len(), 4);
    let manifest = &result["inspection"]["manifest"];
    assert_eq!(manifest["validation_state"], "Trusted");
    assert_eq!(manifest["title"], "no manifest", "title from the file name");
    let entries = &manifest["training_mining"]["entries"];
    assert_eq!(entries["cawg.ai_training"]["use"], "notAllowed");
    assert_eq!(
        entries["cawg.data_mining"]["constraint_info"],
        "research only"
    );
    for m in manifest["soft_bindings"][0]["matches"].as_array().unwrap() {
        assert_eq!(m["similarity"], 1.0, "{}", m["embedded"]["name"]);
    }
}

#[test]
fn sign_without_reachable_timestamp_service_notes_it_and_succeeds() {
    let dir = std::env::temp_dir().join("iscc-c2pa-demo-test-cli-tsa");
    std::fs::create_dir_all(&dir).unwrap();
    let output = dir.join("cli-no-tsa.jpg");
    let _ = std::fs::remove_file(&output);
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let tsa = format!("http://127.0.0.1:{port}/");
    let signed = cli(&[
        "sign",
        fixture("no_manifest.jpg").to_str().unwrap(),
        "--output",
        output.to_str().unwrap(),
        "--tsa",
        &tsa,
        "--no-preview",
    ]);
    let stderr = String::from_utf8_lossy(&signed.stderr);
    assert!(
        stderr.contains("note: signed without a timestamp: http://127.0.0.1"),
        "{stderr}"
    );
    let result = json(&signed);
    assert_eq!(result["timestamp"]["status"], "failed");
    assert_eq!(result["timestamp"]["url"], tsa.as_str());
    let manifest = &result["inspection"]["manifest"];
    assert_eq!(manifest["validation_state"], "Trusted");
    assert_eq!(manifest["signature"]["timestamp"]["status"], "none");
}

#[test]
fn meta_code_and_formats() {
    let unit = json(&cli(&[
        "meta-code",
        "--title",
        "Title",
        "--description",
        "Text",
    ]));
    assert_eq!(unit["name"], "Meta-Code");
    assert!(unit["iscc"].as_str().unwrap().starts_with("ISCC:AAD"));
    let formats = json(&cli(&["formats"]));
    let formats = formats.as_array().unwrap();
    assert!(formats
        .iter()
        .any(|f| f["extensions"][0] == "docx" && f["kind"] == "text"));
    assert!(formats
        .iter()
        .any(|f| f["extensions"][0] == "m4a" && f["kind"] == "audio"));
}

#[test]
fn inspect_audio() {
    let path = fixture("demo.mp3");
    let inspection = json(&cli(&["inspect", path.to_str().unwrap(), "--no-preview"]));
    assert_eq!(inspection["kind"], "audio");
    assert_eq!(inspection["iscc"][1]["name"], "Content-Code Audio");
    assert!(inspection["duration_secs"].as_f64().unwrap() > 15.0);
}

#[test]
fn sign_audio_too_short_leaves_the_content_code_out() {
    let dir = std::env::temp_dir().join("iscc-c2pa-demo-test-cli-short");
    std::fs::create_dir_all(&dir).unwrap();
    let output = dir.join("short-signed.wav");
    let _ = std::fs::remove_file(&output);
    let signed = cli(&[
        "sign",
        fixture("short.wav").to_str().unwrap(),
        "--output",
        output.to_str().unwrap(),
        "--no-timestamp",
        "--no-preview",
    ]);
    let stderr = String::from_utf8_lossy(&signed.stderr);
    assert!(
        stderr.contains("note: Content-Code left out: audio too short"),
        "{stderr}"
    );
    let result = json(&signed);
    let names: Vec<&str> = result["units"]
        .as_array()
        .unwrap()
        .iter()
        .map(|u| u["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["Meta-Code", "Data-Code", "Instance-Code"]);
}

#[test]
fn errors_exit_with_one_line_and_usage_errors_with_two() {
    let missing = cli(&["inspect", "no-such-file.jpg"]);
    assert_eq!(missing.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&missing.stderr);
    assert_eq!(stderr.trim_end().lines().count(), 1, "{stderr}");
    assert!(stderr.starts_with("error: cannot read"), "{stderr}");

    let bad_training = cli(&[
        "sign",
        fixture("no_manifest.jpg").to_str().unwrap(),
        "--training",
        "cawg.ai_training=maybe",
    ]);
    assert_eq!(bad_training.status.code(), Some(1));

    let bad_unit = cli(&[
        "sign",
        fixture("no_manifest.jpg").to_str().unwrap(),
        "--units",
        "meta,imag,data,instance",
    ]);
    assert_eq!(bad_unit.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&bad_unit.stderr);
    assert!(stderr.contains("'imag'"), "{stderr}");

    let both = cli(&[
        "sign",
        fixture("no_manifest.jpg").to_str().unwrap(),
        "--tsa",
        "http://127.0.0.1:9/",
        "--no-timestamp",
    ]);
    assert_eq!(both.status.code(), Some(2));
    assert_eq!(cli(&["sign"]).status.code(), Some(2));
    assert_eq!(cli(&["nonsense"]).status.code(), Some(2));
}

#[test]
fn sign_pdf_refuses_encryption_and_notes_a_digital_signature() {
    let dir = std::env::temp_dir().join("iscc-c2pa-demo-test-cli-pdf");
    std::fs::create_dir_all(&dir).unwrap();
    let encrypted = cli(&[
        "sign",
        fixture("basic-password.pdf").to_str().unwrap(),
        "--output",
        dir.join("locked.pdf").to_str().unwrap(),
        "--no-timestamp",
    ]);
    assert_eq!(encrypted.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&encrypted.stderr);
    assert_eq!(stderr.trim_end().lines().count(), 1, "{stderr}");
    assert!(stderr.starts_with("error: Encrypted PDFs"), "{stderr}");

    let output = dir.join("retest-signed.pdf");
    let _ = std::fs::remove_file(&output);
    let signed = cli(&[
        "sign",
        fixture("basic-retest.pdf").to_str().unwrap(),
        "--output",
        output.to_str().unwrap(),
        "--no-timestamp",
        "--no-preview",
    ]);
    let stderr = String::from_utf8_lossy(&signed.stderr);
    assert!(
        stderr.contains("note: Signing rewrites the PDF and breaks its existing digital signature"),
        "{stderr}"
    );
    let result = json(&signed);
    assert_eq!(result["units"][1]["name"], "Content-Code Text");
    assert_eq!(
        result["inspection"]["manifest"]["validation_state"],
        "Trusted"
    );
}

#[test]
fn video_inspects_and_signs_once_ffmpeg_is_installed() {
    // Installs into the user's tools folder on the first run on a machine; instant afterwards.
    let status = json(&cli(&["tools", "install"]));
    assert_eq!(status["installed"], true);
    assert_eq!(status["licence"], "GPL-2.0-or-later");
    assert_eq!(json(&cli(&["tools", "status"])), status);

    let path = fixture("demo.mp4");
    let inspection = json(&cli(&["inspect", path.to_str().unwrap(), "--no-preview"]));
    assert_eq!(inspection["kind"], "video");
    assert_eq!(inspection["iscc"][1]["name"], "Content-Code Video");
    assert_eq!(inspection["width"], 176);

    let dir = std::env::temp_dir().join("iscc-c2pa-demo-test-cli-video");
    std::fs::create_dir_all(&dir).unwrap();
    let output = dir.join("demo-signed.mp4");
    let _ = std::fs::remove_file(&output);
    let result = json(&cli(&[
        "sign",
        path.to_str().unwrap(),
        "--output",
        output.to_str().unwrap(),
        "--no-timestamp",
        "--no-preview",
    ]));
    assert_eq!(result["units"].as_array().unwrap().len(), 4);
    let manifest = &result["inspection"]["manifest"];
    assert_eq!(manifest["validation_state"], "Trusted");
    let sb = &manifest["soft_bindings"][0];
    assert_eq!(sb["preservation"], "no_source_view");
    assert_eq!(sb["matches"][1]["embedded"]["name"], "Content-Code Video");
    assert_eq!(sb["matches"][1]["similarity"], 1.0);
}

#[test]
fn mp4_with_sound_only_is_signed_with_its_audio_code() {
    let dir = std::env::temp_dir().join("iscc-c2pa-demo-test-cli-sound-only");
    std::fs::create_dir_all(&dir).unwrap();
    let result = json(&cli(&[
        "sign",
        fixture("no-video.mp4").to_str().unwrap(),
        "--output",
        dir.join("no-video-signed.mp4").to_str().unwrap(),
        "--no-timestamp",
        "--no-preview",
    ]));
    assert_eq!(result["units"][1]["name"], "Content-Code Audio");
    let inspection = &result["inspection"];
    assert_eq!(inspection["kind"], "audio");
    assert_eq!(inspection["format_label"], "MP4 audio");
    assert_eq!(inspection["mime"], "video/mp4");
    let manifest = &inspection["manifest"];
    assert_eq!(manifest["validation_state"], "Trusted");
    let sb = &manifest["soft_bindings"][0];
    assert_eq!(sb["matches"][1]["embedded"]["name"], "Content-Code Audio");
    assert_eq!(sb["matches"][1]["similarity"], 1.0);
}
