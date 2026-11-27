use super::*;

const LIMIT: u64 = 1024;

fn read(uri: &str, path: Option<&str>, limit: u64) -> Result<Vec<u8>> {
    let (raw, encoded) = read_file(target(source(uri)?, path)?, limit)?;
    let bytes = STANDARD.decode(encoded).unwrap();
    assert_eq!(raw, bytes.len() as u64);
    Ok(bytes)
}
fn reason(error: &ApiError) -> &str {
    error.details.as_ref().unwrap()["reason"].as_str().unwrap()
}
fn app(root: &Path) -> String {
    format!("{APP_SCHEME}:{}", root.display())
}
fn web_app() -> (tempfile::TempDir, PathBuf) {
    let root = tempfile::tempdir().unwrap();
    let folder = root.path().join("kanban");
    std::fs::create_dir_all(folder.join("assets")).unwrap();
    std::fs::write(folder.join("index.html"), "<script src=app.js>").unwrap();
    std::fs::write(folder.join("assets").join("app.js"), "board()").unwrap();
    std::fs::write(root.path().join("secret"), "outside").unwrap();
    (root, folder)
}

#[test]
fn app_folder_serves_regular_files_by_relative_path() {
    let (_root, folder) = web_app();
    let uri = app(&folder);
    assert_eq!(
        read(&uri, Some("index.html"), LIMIT).unwrap(),
        b"<script src=app.js>"
    );
    assert_eq!(
        read(&uri, Some("assets/app.js"), LIMIT).unwrap(),
        b"board()"
    );
    assert_eq!(
        read(&uri, Some("./assets/app.js"), LIMIT).unwrap(),
        b"board()"
    );
    let error = read(&uri, None, LIMIT).unwrap_err();
    assert_eq!(reason(&error), "path_required");
    let error = read(&uri, Some("assets"), LIMIT).unwrap_err();
    assert_eq!(error.code, ErrorCode::TargetUnavailable);
    let error = read(&uri, Some("missing.css"), LIMIT).unwrap_err();
    assert_eq!(error.code, ErrorCode::TargetUnavailable);
}

#[test]
fn app_paths_that_leave_the_folder_are_refused() {
    let (root, folder) = web_app();
    let uri = app(&folder);
    let absolute = root.path().join("secret");
    for path in [
        "../secret",
        "assets/../../secret",
        "assets/../index.html",
        absolute.to_str().unwrap(),
    ] {
        let error = read(&uri, Some(path), LIMIT).unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidArgument, "{path}");
        assert_eq!(reason(&error), "path_outside_folder", "{path}");
    }
}

#[cfg(unix)]
#[test]
fn links_are_followed_only_while_they_stay_inside_the_folder() {
    let (root, folder) = web_app();
    let uri = app(&folder);
    std::os::unix::fs::symlink(root.path().join("secret"), folder.join("leak")).unwrap();
    std::os::unix::fs::symlink(root.path(), folder.join("parent")).unwrap();
    std::os::unix::fs::symlink("assets/app.js", folder.join("alias.js")).unwrap();
    for path in ["leak", "parent/secret"] {
        let error = read(&uri, Some(path), LIMIT).unwrap_err();
        assert_eq!(reason(&error), "path_outside_folder", "{path}");
    }
    assert_eq!(read(&uri, Some("alias.js"), LIMIT).unwrap(), b"board()");
    // The folder itself may be reached through a link.
    let linked = root.path().join("linked-app");
    std::os::unix::fs::symlink(&folder, &linked).unwrap();
    assert_eq!(
        read(&app(&linked), Some("index.html"), LIMIT).unwrap(),
        b"<script src=app.js>"
    );
}

#[test]
fn file_uris_and_absolute_paths_name_one_file_only() {
    let (root, folder) = web_app();
    let file = folder.join("index.html");
    let url = url::Url::from_file_path(&file).unwrap().to_string();
    let localhost = url.replacen("file:///", "file://localhost/", 1);
    for uri in [file.to_str().unwrap(), url.as_str(), localhost.as_str()] {
        assert_eq!(read(uri, None, LIMIT).unwrap(), b"<script src=app.js>");
        let error = read(uri, Some("assets/app.js"), LIMIT).unwrap_err();
        assert_eq!(reason(&error), "path_not_allowed", "{uri}");
    }
    let error = read(folder.to_str().unwrap(), None, LIMIT).unwrap_err();
    assert_eq!(error.code, ErrorCode::TargetUnavailable);
    drop(root);
}

#[test]
fn remote_relative_and_unknown_uris_are_refused_without_access() {
    for uri in [
        "http://127.0.0.1:1/index.html",
        "https://example.test/app",
        "relative/index.html",
        "shellboy-app:relative/folder",
        "new+client:opaque",
        "file://remote-host/share/file.txt",
        "FILE://Remote-Host/C:/share/file.txt",
        "file:\\\\remote-host\\share\\file.txt",
        // Parsers strip tabs and newlines, which could join or hide a host.
        "file://remote\thost/share/file.txt",
        "file:/\n/remote-host/share/file.txt",
        "file://localhost\r/tmp/file.txt",
        "file:///tmp/fi\nle.txt",
    ] {
        let error = source(uri).unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidArgument, "{uri}");
        assert_eq!(reason(&error), "uri_not_readable", "{uri}");
    }
}

/// Every form resolves to a path first; one local-path rule then refuses it
/// before any filesystem access.
fn refused_as_not_local(uri: &str, path: Option<&str>) -> ApiError {
    let resolved = source(uri).and_then(|source| target(source, path));
    let Err(error) = resolved else {
        panic!("{uri} {path:?} was accepted as {resolved:?}");
    };
    assert_eq!(error.code, ErrorCode::InvalidArgument, "{uri} {path:?}");
    error
}

#[test]
fn local_path_normalizes_verbatim_drives_but_refuses_other_namespaces() {
    for (path, normalized) in [
        (r"\\?\C:\work\index.html", r"C:\work\index.html"),
        (r"\\?\z:\", r"z:\"),
    ] {
        assert_eq!(local_path(Path::new(path)).unwrap().as_os_str(), normalized);
    }
    for path in [
        r"\\?\UNC\server\share\index.html",
        r"\\.\NUL",
        r"\\server\share\index.html",
        r"\\?\C:relative",
        "\\\\?\\C:\\work\\index\n.html",
    ] {
        let error = local_path(Path::new(path)).unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidArgument, "{path:?}");
        assert_eq!(reason(&error), "path_not_local", "{path:?}");
    }
}

#[test]
fn network_shares_and_control_characters_are_refused_in_every_form() {
    let (_root, folder) = web_app();
    let uri = app(&folder);
    // The same rule on every platform, whatever form produced the path.
    for (uri, path) in [
        ("//server/share/index.html", None),
        ("shellboy-app://server/share/app", Some("index.html")),
        ("file:///C:/tmp/a%0Ab.txt", None),
        (uri.as_str(), Some("index\n.html")),
    ] {
        let error = refused_as_not_local(uri, path);
        assert_eq!(reason(&error), "path_not_local", "{uri} {path:?}");
    }
    // Converters differ per platform (a share, a local path, or no path);
    // whatever comes out, no network path is ever handed to the filesystem.
    for uri in [
        "file:////server/share/x",
        "file:///%2F%2Fserver/share/x",
        "\\\\server\\share\\index.html",
        "shellboy-app:\\\\server\\share\\app",
    ] {
        let path = uri.starts_with(APP_SCHEME).then_some("index.html");
        match source(uri).and_then(|source| target(source, path)) {
            Err(error) => assert!(
                ["path_not_local", "uri_not_readable"].contains(&reason(&error)),
                "{uri}"
            ),
            Ok(Target::File(file)) => {
                let text = file.to_string_lossy();
                assert!(
                    !text.starts_with("//") && !text.starts_with("\\\\"),
                    "{uri} -> {text}"
                );
            }
            Ok(other) => panic!("{uri} resolved to {other:?}"),
        }
    }
}

#[test]
fn reads_above_the_limit_are_refused() {
    let (_root, folder) = web_app();
    let uri = app(&folder);
    let size = std::fs::metadata(folder.join("index.html")).unwrap().len();
    assert!(read(&uri, Some("index.html"), size).is_ok());
    let error = read(&uri, Some("index.html"), size - 1).unwrap_err();
    assert_eq!(error.code, ErrorCode::InputLimit);
    assert_eq!(reason(&error), "file_too_large");
    let file = folder.join("index.html");
    let error = read(file.to_str().unwrap(), None, size - 1).unwrap_err();
    assert_eq!(error.code, ErrorCode::InputLimit);
}

#[test]
fn only_the_exact_saved_version_of_the_named_thread_resolves() {
    let db = rusqlite::Connection::open_in_memory().unwrap();
    db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
    db.execute_batch(
        "INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)
         VALUES ('t','/','','p','m','now'), ('other','/','','p','m','now');
         INSERT INTO artifact(id,thread_id,created_at,updated_at) VALUES ('a','t','now','now');
         INSERT INTO artifact_version(artifact_id,version,uri,title,created_at) VALUES
         ('a',1,'shellboy-app:/first','App','now'), ('a',2,'/second','App','now');",
    )
    .unwrap();
    let args = |thread: &str, version| ArtifactReadArgs {
        thread_id: thread.into(),
        artifact_id: "a".into(),
        version,
        path: None,
    };
    assert_eq!(
        version_uri(&db, &args("t", 1)).unwrap(),
        "shellboy-app:/first"
    );
    assert_eq!(version_uri(&db, &args("t", 2)).unwrap(), "/second");
    assert_eq!(
        version_uri(&db, &args("t", 3)).unwrap_err().code,
        ErrorCode::TargetUnavailable
    );
    assert_eq!(
        version_uri(&db, &args("other", 1)).unwrap_err().code,
        ErrorCode::TargetUnavailable
    );
    assert_eq!(
        version_uri(&db, &args("missing", 1)).unwrap_err().code,
        ErrorCode::ThreadNotFound
    );
}

#[test]
fn files_beyond_one_reply_frame_are_refused_before_reading_with_the_effective_limit() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("large.bin");
    let args = ArtifactReadArgs {
        thread_id: "t".into(),
        artifact_id: "a".into(),
        version: 1,
        path: None,
    };
    let operation = "instance:read";
    // A configured limit above the frame leaves the frame as the bound.
    let limit = effective_limit(u64::MAX, operation, &args);
    assert!(limit < MAX_FRAME_BYTES as u64);
    assert_eq!(effective_limit(limit - 1, operation, &args), limit - 1);
    let file = std::fs::File::create(&path).unwrap();
    file.set_len(limit + 1).unwrap();
    let error = read_file(Target::File(path.clone()), limit).unwrap_err();
    assert_eq!(error.code, ErrorCode::InputLimit);
    assert_eq!(reason(&error), "file_too_large");
    assert_eq!(error.details.as_ref().unwrap()["limit_bytes"], limit);
    // The largest admitted file still fits one frame with its envelope.
    file.set_len(limit).unwrap();
    let (raw_bytes, base64) = read_file(Target::File(path), limit).unwrap();
    let content = ArtifactContent {
        artifact_id: args.artifact_id,
        version: args.version,
        path: None,
        raw_bytes,
        base64,
    };
    let reply = Response::success(operation, serde_json::to_value(content).unwrap());
    assert!(crate::protocol::encode_frame(&reply).is_ok());
}

#[cfg(unix)]
#[test]
fn a_named_pipe_is_refused_from_its_handle_without_blocking() {
    let (_root, folder) = web_app();
    let pipe = folder.join("pipe");
    let name = std::ffi::CString::new(pipe.to_str().unwrap()).unwrap();
    // SAFETY: `name` is a valid NUL-terminated path for the duration of the call.
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    // No writer ever opens the pipe: a blocking open would never return here.
    for uri in [app(&folder), pipe.to_str().unwrap().to_owned()] {
        let path = uri.starts_with(APP_SCHEME).then_some("pipe");
        let error = read(&uri, path, LIMIT).unwrap_err();
        assert_eq!(error.code, ErrorCode::TargetUnavailable, "{uri}");
        assert_eq!(reason(&error), "not_a_file", "{uri}");
    }
}

#[cfg(windows)]
#[test]
fn windows_device_names_and_namespaces_are_refused_before_any_open() {
    let (_root, folder) = web_app();
    let uri = app(&folder);
    for path in [
        "NUL",
        "assets/con.txt",
        "COM1",
        "lpt9.js",
        "COM¹",
        "com².txt",
        "LPT³",
    ] {
        let error = refused_as_not_local(&uri, Some(path));
        assert_eq!(reason(&error), "path_not_local", "{path}");
    }
    for uri in [
        r"C:\work\CON",
        r"\\.\NUL",
        r"\\?\C:\work\NUL",
        // Percent-decoding a file: URI must not slip a device name past the rule.
        "file:///C:/work/%4EUL",
        "file:///C:/work/COM%C2%B9.txt",
    ] {
        let error = refused_as_not_local(uri, None);
        assert_eq!(reason(&error), "path_not_local", "{uri}");
    }
    let device_folder = format!("{APP_SCHEME}:{}", folder.join("aux").display());
    let error = refused_as_not_local(&device_folder, Some("index.html"));
    assert_eq!(reason(&error), "path_not_local");
}
