//! Native Unix PTY writer regression with disposable owner and loopback model only.
#[cfg(unix)]
mod native {
    use serde_json::{Value, json};
    use std::{
        fs::File,
        io::{Read, Write},
        os::fd::FromRawFd,
        path::Path,
        process::Stdio,
    };
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    const PREFIX: &[u8] = b"\x1b]7501;";
    fn command(home: &Path) -> tokio::process::Command {
        let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_shellboy"));
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("SHELLBOY_") {
                command.env_remove(key);
            }
        }
        command
            .env("HOME", home)
            .env("USERPROFILE", home)
            .env("APPDATA", home.join("Roaming"))
            .env("LOCALAPPDATA", home.join("Local"))
            .env("XDG_CONFIG_HOME", home.join("config"))
            .env("XDG_DATA_HOME", home.join("data"))
            .env_remove("XDG_RUNTIME_DIR")
            .env("TERM", "xterm-256color")
            .kill_on_drop(true);
        command
    }
    async fn cli(home: &Path, data: &Path, args: &[&str]) -> Value {
        let output = crate::test_watchdog::watchdog(
            "tests/program_status.rs: cli",
            command(home)
                .arg("--data-dir")
                .arg(data)
                .args(args)
                .output(),
        )
        .await
        .unwrap()
        .unwrap();
        let value: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert!(output.status.success() && value["ok"] == true, "{value}");
        value["result"].clone()
    }
    fn packets(bytes: &[u8]) -> Vec<std::collections::BTreeMap<String, String>> {
        let mut out = Vec::new();
        let mut offset = 0;
        while let Some(at) = bytes[offset..]
            .windows(PREFIX.len())
            .position(|value| value == PREFIX)
        {
            let begin = offset + at + PREFIX.len();
            let Some(end) = bytes[begin..]
                .windows(2)
                .position(|value| value == b"\x1b\\")
            else {
                break;
            };
            let text = std::str::from_utf8(&bytes[begin..begin + end]).unwrap();
            out.push(
                text.split(':')
                    .map(|pair| {
                        let (k, v) = pair.split_once('=').unwrap();
                        (k.into(), v.into())
                    })
                    .collect(),
            );
            offset = begin + end + 2;
        }
        out
    }
    async fn read_until(
        master: &tokio::io::unix::AsyncFd<File>,
        bytes: &mut Vec<u8>,
        predicate: impl Fn(&[u8]) -> bool,
    ) {
        crate::test_watchdog::watchdog("tests/program_status.rs: read_until", async {
            loop {
                if predicate(bytes) {
                    return;
                }
                let mut guard = master.readable().await.unwrap();
                let result = guard.try_io(|inner| {
                    let mut file = inner.get_ref();
                    let mut buffer = [0; 65536];
                    let size = file.read(&mut buffer)?;
                    Ok::<_, std::io::Error>((size, buffer))
                });
                match result {
                    Ok(Ok((0, _))) => panic!("PTY ended before expected state"),
                    Ok(Ok((size, buffer))) => {
                        bytes.extend_from_slice(&buffer[..size]);
                        assert!(bytes.len() < 8 * 1024 * 1024);
                    }
                    Ok(Err(error)) => panic!("PTY read failed: {error}"),
                    Err(_) => {}
                }
            }
        })
        .await
        .unwrap();
    }
    async fn request(stream: &mut tokio::net::TcpStream) -> Value {
        let mut bytes = Vec::new();
        let mut end = None;
        let mut length = 0;
        loop {
            if let Some(at) = end
                && bytes.len() >= at + length
            {
                break;
            }
            let mut block = [0; 4096];
            let n = stream.read(&mut block).await.unwrap();
            assert!(n > 0 && bytes.len() < 8 * 1024 * 1024);
            bytes.extend_from_slice(&block[..n]);
            if end.is_none()
                && let Some(at) = bytes.windows(4).position(|value| value == b"\r\n\r\n")
            {
                let at = at + 4;
                length = String::from_utf8_lossy(&bytes[..at])
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(|value| value.trim().parse::<usize>().unwrap())
                    })
                    .unwrap();
                end = Some(at);
            }
        }
        serde_json::from_slice(&bytes[end.unwrap()..end.unwrap() + length]).unwrap()
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn tty_lifecycle_and_shell_osc_are_isolated_in_on_and_off_modes() {
        crate::test_watchdog::watchdog("tests/program_status.rs: tty_lifecycle_and_shell_osc_are_isolated_in_on_and_off_modes",async {
        for mode in ["on","off"] {
            let root=tempfile::tempdir().unwrap();let home=root.path().join("home");std::fs::create_dir(&home).unwrap();
            let data=root.path().join("owner-data");let config=root.path().join("config.toml");
            let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();let address=listener.local_addr().unwrap();
            std::fs::write(&config,format!("default_connection='fixture'\ndefault_model='fixture'\n[connections.fixture]\nbase_url='http://{address}/v1'\n[ui]\nprogram_status='{mode}'\n")).unwrap();
            let (accepted,received)=tokio::sync::oneshot::channel();let (release,released)=tokio::sync::oneshot::channel();
            let server=tokio::spawn(async move {
                let mut accepted=Some(accepted);let mut released=Some(released);
                for index in 0..3 {
                    let (mut stream,_)=listener.accept().await.unwrap();let body=request(&mut stream).await;
                    let (status,kind,body)=if index==2 {(401,"application/json",json!({"error":{"message":"Synthetic auth failure\u{202e}\u{2066}\n\u{1b}]7501;app=evil\u{1b}\\"}}).to_string())} else {
                        let output=if index==0 {
                            let _=accepted.take().unwrap().send(());let _=released.take().unwrap().await;
                            json!([{"type":"function_call","call_id":"osc-probe","name":"shell","arguments":json!({"command":"printf '\\033]7501;state=error:app=evil:id=forged\\033\\\\'"}).to_string()}])
                        }else {
                            assert!(body["input"].as_array().unwrap().iter().any(|item|item["type"]=="function_call_output"&&item["output"].as_str().unwrap_or("").contains("app=evil")),"real shell output must travel through saved tool results");
                            json!([{"type":"message","role":"assistant","content":[{"type":"output_text","text":"fixture complete"}]}])
                        };
                        (200,"text/event-stream",format!("data: {}\n\n",json!({"type":"response.completed","response":{"id":format!("fixture-{index}"),"status":"completed","output":output}})))
                    };
                    stream.write_all(format!("HTTP/1.1 {status} Fixture\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
                }
            });
            let mut master=-1;let mut slave=-1;let mut size=libc::winsize{ws_row:36,ws_col:120,ws_xpixel:0,ws_ypixel:0};
            // This fixture owns both returned descriptors; no real terminal is opened.
            assert_eq!(unsafe{libc::openpty(&mut master,&mut slave,std::ptr::null_mut(),std::ptr::null_mut(),std::ptr::addr_of_mut!(size))},0);
            assert_eq!(unsafe{libc::fcntl(master,libc::F_SETFL,libc::O_NONBLOCK)},0);
            assert_eq!(unsafe { libc::fcntl(master,libc::F_SETFD,libc::FD_CLOEXEC) },0);
            assert_eq!(unsafe { libc::fcntl(slave,libc::F_SETFD,libc::FD_CLOEXEC) },0);
            // The owned File's fd is never replaced/closed until AsyncFd deregisters it.
            let master=unsafe { tokio::io::unix::AsyncFd::register(File::from_raw_fd(master)) }.unwrap();
            let slave=unsafe{File::from_raw_fd(slave)};
            let mut owner=command(&home).arg("--config").arg(&config).arg("--data-dir").arg(&data).current_dir(root.path()).stdin(Stdio::from(slave.try_clone().unwrap())).stdout(Stdio::from(slave.try_clone().unwrap())).stderr(Stdio::from(slave)).spawn().unwrap();
            let mut bytes=Vec::new();read_until(&master,&mut bytes,|bytes|bytes.windows(2).any(|v|v==b"F1")).await;
            let created=cli(&home,&data,&["thread","create"]).await;let id=created["thread_id"].as_str().unwrap();let terminal_id=shellboy::program_status::thread_id(id);
            let sent=cli(&home,&data,&["message","send","--thread",id,"--text","fixture work"]).await;
            received.await.unwrap();
            if mode=="on" {read_until(&master,&mut bytes,|bytes|packets(bytes).iter().any(|p|p.get("state").map(String::as_str)==Some("working")&&p.get("id")==Some(&terminal_id))).await;}
            let _=release.send(());
            let conditions=json!([{"type":"run_ended","run_id":sent["run_id"]}]).to_string();cli(&home,&data,&["wait","--conditions-json",&conditions,"--timeout-ms","60000"]).await;
            if mode=="on" {read_until(&master,&mut bytes,|bytes|packets(bytes).iter().any(|p|p.get("state").map(String::as_str)==Some("done")&&p.get("id")==Some(&terminal_id))).await;}
            let mut input=master.get_ref();input.write_all(format!("/switch {id}\r").as_bytes()).unwrap();
            if mode=="on" {read_until(&master,&mut bytes,|bytes|packets(bytes).iter().any(|p|p.get("state").map(String::as_str)==Some("clear")&&p.get("id")==Some(&terminal_id))).await;}
            cli(&home,&data,&["message","send","--thread",id,"--text","auth fixture"]).await;
            server.await.unwrap();
            if mode=="on" {read_until(&master,&mut bytes,|bytes|packets(bytes).iter().any(|p|p.get("state").map(String::as_str)==Some("blocked")&&p.get("kind").map(String::as_str)==Some("auth"))).await;}
            input.write_all(&[17]).unwrap();
            if mode=="on" {read_until(&master,&mut bytes,|bytes|packets(bytes).iter().any(|p|p.get("state").map(String::as_str)==Some("clear")&&!p.contains_key("id"))).await;}
            else {
                // A foreign marker is not an OSC packet; only use it to complete a read turn.
                read_until(&master,&mut bytes,|bytes|bytes.windows(8).any(|v|v==b"\x1b[?1049l")).await;
            }
            assert!(owner.wait().await.unwrap().success());
            let records=packets(&bytes);
            assert!(records.iter().all(|record|record.get("app").map(String::as_str)==Some("shellboy")&&!record.contains_key("progress")));
            assert!(!bytes.windows(b"\x1b]7501;state=error:app=evil".len()).any(|v|v==b"\x1b]7501;state=error:app=evil"));
            if mode=="off" {assert!(records.is_empty());}
        }
    }).await.unwrap();
    }
}
