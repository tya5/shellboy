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
fn unicode_environment_preserves_hidden_entries_and_case_insensitive_override() {
    let compare = |a: &[u16], b: &[u16]| Ok(s(a).to_uppercase().cmp(&s(b).to_uppercase()));
    let inherited = vec![
        (w("Path"), w("old")),
        (w("=C:"), w(r"C:\project")),
        (w("=::"), w(r"::\")),
        (w("=ExitCode"), w("00000000")),
        (w("=ExitCodeAscii"), w("00000000")),
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
        "=::=::\\\0=C:=C:\\project\0=ExitCode=00000000\0=ExitCodeAscii=00000000\0KEEP=日本語\0PATH=origin\0SHELLBOY_THREAD_ID=thread\0\0"
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

#[test]
fn invalid_environment_entries_cannot_inject_variables_or_disclose_values() {
    for key in ["", "BAD=KEY", "==Hidden", "=Hidden=KEY", "BAD\0KEY"] {
        let error = environment_block(&[(w(key), w("PRIVATE_VALUE"))]).unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidArgument);
        assert!(error.message.contains(&format!("{key:?}")));
        assert!(!error.message.contains("PRIVATE_VALUE"));
        assert!(!error.message.contains('\0'));
    }
    for key in ["KEY", "=::", "=ExitCode"] {
        let error = environment_block(&[(w(key), w("PRIVATE\0INJECTED=value"))]).unwrap_err();
        assert!(error.message.contains(&format!("{key:?}")));
        assert!(!error.message.contains("PRIVATE"));
        assert!(!error.message.contains("INJECTED"));
    }
    let key = format!("{}\nPRIVATE_SUFFIX", "X".repeat(128));
    let error = environment_block(&[(w(&key), w("PRIVATE\0VALUE"))]).unwrap_err();
    assert!(error.message.contains(&"X".repeat(128)));
    assert!(!error.message.contains("PRIVATE"));
    assert!(!error.message.contains('\n'));
}

#[test]
fn environment_values_keep_equals_and_non_unicode_utf16_without_boundary_changes() {
    let key = vec![b'=' as u16, 0xd800];
    let value = vec![0xdc00, b'=' as u16, b'x' as u16];
    assert_eq!(
        environment_block(&[(key.clone(), value.clone())]).unwrap(),
        [key, vec![b'=' as u16], value, vec![0, 0]].concat()
    );
    assert_eq!(
        s(&environment_block(&[(w("KEY"), w("a=b=c"))]).unwrap()),
        "KEY=a=b=c\0\0"
    );
}
