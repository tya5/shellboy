use shellboy::rag::extract::extract;
use std::{fs, path::Path};

#[test]
fn utf8_input_is_bounded_and_conversion_is_external() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("日本語.MD");
    fs::write(&file, "# 検索\n原文を保存する。\n").unwrap();
    let result = extract(&file).unwrap();
    assert_eq!(result.text, "# 検索\n原文を保存する。\n");
    assert_eq!(result.blocks[0].location["unit"], "document");
    assert_eq!(result.extractor, "text:utf8-v1");
    let extensionless = dir.path().join("README");
    fs::write(&extensionless, "plain UTF-8 input").unwrap();
    assert!(extract(&extensionless).is_ok());
    for ext in ["pdf", "PDF", "docx", "pptx", "xlsx", "custom"] {
        let path = dir.path().join(Path::new("original").with_extension(ext));
        fs::write(&path, "not interpreted by Shellboy").unwrap();
        assert!(extract(&path).unwrap_err().contains("convert externally"));
    }
    fs::write(&file, [0xff]).unwrap();
    assert!(extract(&file).unwrap_err().contains("UTF-8"));
    let big = dir.path().join("big.txt");
    fs::File::create(&big)
        .unwrap()
        .set_len(16 * 1024 * 1024 + 1)
        .unwrap();
    assert!(extract(&big).unwrap_err().contains("16 MiB"));
    assert!(extract(dir.path()).is_err());
}
