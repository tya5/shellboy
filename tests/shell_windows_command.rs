use shellboy::{protocol::ErrorCode, shell::windows_command::*};
fn w(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}
fn s(w: &[u16]) -> String {
    String::from_utf16(w).unwrap()
}
#[test]
fn crt_quoting_preserves_empty_spaces_quotes_backslashes_and_unicode() {
    for (input, expected) in [
        ("", r#""""#),
        ("word", r#""word""#),
        ("a b", r#""a b""#),
        ("a\"b", r#""a\"b""#),
        ("日本語", r#""日本語""#),
        (r"C:\folder\", r#""C:\folder\\""#),
        (r#"x\"y"#, r#""x\\\"y""#),
    ] {
        assert_eq!(s(&quote_arg(&w(input)).unwrap()), expected, "{input}");
    }
    assert!(quote_arg(&[0]).is_err());
    let line = command_line(
        &w(r"C:\Program Files\shell.exe"),
        &[w("-c"), w("printf 'a b'"), w("")],
    )
    .unwrap();
    assert_eq!(*line.last().unwrap(), 0);
    assert_eq!(
        s(&line[..line.len() - 1]),
        r#""C:\Program Files\shell.exe" "-c" "printf 'a b'" """#
    );
    assert_eq!(
        command_line(&w("shell.exe"), &[vec![b'x' as u16; 32767]])
            .unwrap_err()
            .code,
        ErrorCode::InputLimit
    );
}
#[test]
fn unicode_environment_preserves_hidden_drive_entries_and_case_insensitive_override() {
    let compare = |a: &[u16], b: &[u16]| Ok(s(a).to_uppercase().cmp(&s(b).to_uppercase()));
    let inherited = vec![
        (w("Path"), w("old")),
        (w("=C:"), w(r"C:\project")),
        (w("KEEP"), w("日本語")),
    ];
    let overrides = vec![
        (w("PATH"), w("configured")),
        (w("PATH"), w("origin")),
        (w("SHELLBOY_THREAD_ID"), w("thread")),
    ];
    let entries = merge_environment(inherited, overrides, compare).unwrap();
    assert_eq!(
        entries
            .iter()
            .filter(|(key, _)| s(key).eq_ignore_ascii_case("path"))
            .count(),
        1
    );
    let block = environment_block(&entries).unwrap();
    assert_eq!(
        s(&block),
        "=C:=C:\\project\0KEEP=日本語\0PATH=origin\0SHELLBOY_THREAD_ID=thread\0\0"
    );
    assert_eq!(environment_block(&[]).unwrap(), [0, 0]);
    for entries in [
        vec![(w("BAD=KEY"), w("x"))],
        vec![(w("KEY"), w("bad\0value"))],
        vec![(w(""), w("x"))],
    ] {
        assert!(environment_block(&entries).is_err());
    }
}
