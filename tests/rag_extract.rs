#[path = "../src/rag/extract.rs"]
mod extractor;
use extractor::extract;
#[cfg(unix)]
use serde_json::json;
use std::{fs, path::Path};

#[test]
fn utf8_is_api_free_and_bounds_unknown_inputs() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("日本語.md");
    fs::write(&file, "# 検索\n原文を保存する。\n").unwrap();
    let result = extract(&file, None, None).unwrap();
    assert_eq!(result.text, "# 検索\n原文を保存する。\n");
    assert_eq!(result.blocks[0].location["unit"], "document");
    assert_eq!(result.extractor, "text:utf8-v1");
    let extensionless = dir.path().join("README");
    fs::write(&extensionless, "plain UTF-8 input").unwrap();
    assert!(extract(&extensionless, None, None).is_ok());
    let office = dir.path().join("fixture.pdf");
    fs::write(&office, "invalid PDF").unwrap();
    assert!(
        extract(&office, None, None)
            .unwrap_err()
            .contains("requires --extractor")
    );
    fs::write(&file, [0xff]).unwrap();
    assert!(extract(&file, None, None).unwrap_err().contains("UTF-8"));
    let big = dir.path().join("big.txt");
    fs::File::create(&big)
        .unwrap()
        .set_len(16 * 1024 * 1024 + 1)
        .unwrap();
    assert!(extract(&big, None, None).unwrap_err().contains("16 MiB"));
}

#[test]
#[cfg(unix)]
fn third_party_adapter_uses_json_blocks_without_shell_interpolation() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input $(touch SHOULD_NOT_EXIST).custom");
    fs::write(&input, "anonymous").unwrap();
    let manifest = dir.path().join("adapter.json");
    fs::write(&manifest, json!({"schema_version":1,"id":"synthetic","version":"1", "executable":"/bin/sh","extensions":["custom"],"argv":["-c", "test -f \"$1\" || exit 9; printf '%s' '{\"blocks\":[{\"text\":\"派生資料の匿名本文\",\"location\":{\"unit\":\"figure\",\"number\":2}}]}'", "adapter", "{input}"]}).to_string()).unwrap();
    let result = extract(&input, None, Some(&manifest)).unwrap();
    assert_eq!(result.text, "派生資料の匿名本文");
    assert_eq!(result.blocks[0].location["number"], 2);
    assert!(result.extractor.starts_with("adapter:synthetic:1:"));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let nested = runtime
        .block_on(async { extract(&input, None, Some(&manifest)) })
        .unwrap();
    assert_eq!(nested.text, result.text);
    assert!(!dir.path().join("SHOULD_NOT_EXIST").exists());
    let invalid = dir.path().join("invalid.json");
    fs::write(&invalid, "{}").unwrap();
    assert!(
        extract(&input, None, Some(&invalid))
            .unwrap_err()
            .contains("manifest")
    );
}

#[test]
#[ignore = "requires explicit SHELLBOY_TEST_XBERG_1_3_10 backend; downloads nothing"]
fn pinned_native_backend_extracts_all_four_document_formats() {
    let backend = std::env::var_os("SHELLBOY_TEST_XBERG_1_3_10").expect("explicit backend path");
    let dir = tempfile::tempdir().unwrap();
    assert!(
        std::process::Command::new("python3")
            .args(["-c", include_str!("fixtures/rag/generate.py")])
            .arg(dir.path())
            .status()
            .unwrap()
            .success()
    );
    for (ext, marker, count, unit) in [
        ("pdf", "SHELLBOY_PDF_V1", 1, "page"),
        ("docx", "SHELLBOY_DOCX_V1", 1, "document_block"),
        ("pptx", "SHELLBOY_PPTX_2", 2, "slide"),
        ("xlsx", "SHELLBOY_XLSX_2", 2, "sheet"),
    ] {
        let result = extract(
            &dir.path().join(format!("synthetic.{ext}")),
            Some(Path::new(&backend)),
            None,
        )
        .unwrap();
        assert!(result.text.contains(marker), "{ext}");
        assert_eq!(result.extractor, "xberg:1.3.10");
        assert_eq!(result.blocks.len(), count, "{ext}");
        assert_eq!(result.blocks[0].location["unit"], unit);
        if ext == "xlsx" {
            assert_eq!(result.blocks[0].location["name"], "資料一");
        }
        if ext != "pdf" {
            assert!(result.text.contains("日本語"));
        }
    }
}

#[test]
#[cfg(unix)]
fn incompatible_backend_is_rejected_before_extraction() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("synthetic.pdf");
    fs::write(&input, "not sent to incompatible backend").unwrap();
    let backend = dir.path().join("old-xberg");
    fs::write(
        &backend,
        "#!/bin/sh\nprintf '%s' '{\"version\":\"0.0.1\"}'\n",
    )
    .unwrap();
    fs::set_permissions(&backend, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(
        extract(&input, Some(&backend), None)
            .unwrap_err()
            .contains("supported Xberg version")
    );
}
