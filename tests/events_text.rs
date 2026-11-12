//! Unsaved model text over `events.wait` (#243 E3), through a real driver and a loopback
//! provider that streams exactly the chunks a test writes. Temporary database only.
use crate::runtime_features::{answer, owner, send, settled, thread};
use crate::test_watchdog::watchdog;
use serde_json::{Value, json};
use shellboy::{
    control::Control,
    protocol::{Command, EventsWaitArgs, HistoryListArgs, Request, RunTarget, ThreadSetArgs},
};
use std::{sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::mpsc,
};
use tokio_util::sync::CancellationToken;

/// One provider request: chunks written here reach the socket in order; dropping it ends the body.
struct Provider {
    chunks: mpsc::UnboundedSender<Vec<u8>>,
}
impl Provider {
    fn raw(&self, bytes: impl Into<Vec<u8>>) {
        self.chunks.send(bytes.into()).unwrap();
    }
    fn delta(&self, text: &str) {
        self.raw(sse(
            &json!({"type":"response.output_text.delta","delta":text}),
        ));
    }
    fn reasoning(&self, text: &str) {
        self.raw(sse(
            &json!({"type":"response.reasoning_summary_text.delta","delta":text}),
        ));
    }
    fn complete(self, text: &str) {
        self.raw(sse(
            &json!({"type":"response.completed","response":answer(text)}),
        ));
    }
}
fn sse(value: &Value) -> Vec<u8> {
    format!("data: {value}\n\n").into_bytes()
}

async fn provider() -> (
    String,
    mpsc::Receiver<Provider>,
    tokio::task::JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let (requests, receive) = mpsc::channel(8);
    let task = tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            stream.set_nodelay(true).unwrap();
            let requests = requests.clone();
            tokio::spawn(async move {
                let mut bytes = Vec::new();
                let mut chunk = [0; 8192];
                loop {
                    let Ok(count) = stream.read(&mut chunk).await else {
                        return;
                    };
                    if count == 0 {
                        return;
                    }
                    bytes.extend_from_slice(&chunk[..count]);
                    if let Some(end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
                        let head = String::from_utf8_lossy(&bytes[..end]).to_lowercase();
                        let length: usize = head
                            .lines()
                            .find_map(|line| line.strip_prefix("content-length:"))
                            .and_then(|value| value.trim().parse().ok())
                            .unwrap();
                        if bytes.len() >= end + 4 + length {
                            break;
                        }
                    }
                }
                let (chunks, mut written) = mpsc::unbounded_channel::<Vec<u8>>();
                if requests.send(Provider { chunks }).await.is_err() {
                    return;
                }
                let head = "HTTP/1.1 200 Fixture\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n";
                if stream.write_all(head.as_bytes()).await.is_err() {
                    return;
                }
                while let Some(chunk) = written.recv().await {
                    if stream.write_all(&chunk).await.is_err() {
                        return;
                    }
                }
            });
        }
    });
    (url, receive, task)
}

/// Every test here ends within the shared deadlock budget and says which one did not.
async fn bounded(name: &'static str, test: impl std::future::Future<Output = ()>) {
    assert!(
        watchdog(name, test).await.is_ok(),
        "test exceeded the deadlock budget: {name}"
    );
}

struct Fixture {
    _root: tempfile::TempDir,
    owner: shellboy::app::DataOwner,
    control: Arc<Control>,
    requests: mpsc::Receiver<Provider>,
    server: tokio::task::JoinHandle<()>,
}
impl Fixture {
    async fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let (url, requests, server) = provider().await;
        let owner = owner(root.path(), &url).await;
        let control = Control::new(&owner, root.path().into());
        Self {
            _root: root,
            owner,
            control,
            requests,
            server,
        }
    }
    /// The partial-output transport retry is on by default; `response_max_retries` enables it.
    async fn with_retries() -> Self {
        let root = tempfile::tempdir().unwrap();
        let (url, requests, server) = provider().await;
        let config = root.path().join("config.toml");
        std::fs::write(
            &config,
            format!("default_connection=\"fixture\"\ndefault_model=\"fixture\"\n[connections.fixture]\nbase_url={url:?}\nresponse_max_retries=2\nresponse_retry_max_delay_ms=10\n"),
        )
        .unwrap();
        let owner = shellboy::app::DataOwner::start(
            Some(config),
            Some(root.path().join("data")),
            root.path().into(),
        )
        .await
        .unwrap();
        let control = Control::new(&owner, root.path().into());
        Self {
            _root: root,
            owner,
            control,
            requests,
            server,
        }
    }
    async fn next_request(&mut self) -> Provider {
        watchdog("provider request", self.requests.recv())
            .await
            .unwrap()
            .unwrap()
    }
    async fn close(self) {
        self.control.shutdown().await.unwrap();
        self.owner.shutdown().await.unwrap();
        self.server.abort();
    }
}

fn wait_command(after: Option<&str>, threads: &[&str], timeout_ms: u64, text: bool) -> Command {
    Command::EventsWait(EventsWaitArgs {
        after: after.map(str::to_owned),
        thread_ids: threads.iter().map(|id| (*id).to_owned()).collect(),
        timeout_ms,
        include_text: text,
        include_reasoning_activity: false,
    })
}
fn wait_request(control: &Control, command: Command) -> Request {
    Request::new(
        format!("{}:{}", control.store.instance_id, uuid::Uuid::new_v4()),
        command,
    )
}
async fn poll(
    control: &Arc<Control>,
    after: Option<&str>,
    threads: &[&str],
    timeout: u64,
) -> Value {
    let request = wait_request(control, wait_command(after, threads, timeout, true));
    let response = watchdog("events.wait text", control.request(request))
        .await
        .unwrap();
    assert!(response.ok, "{:?}", response.error);
    response.result.unwrap()
}
fn cursor(value: &Value) -> String {
    value["cursor"].as_str().unwrap().to_owned()
}
fn entry<'a>(value: &'a Value, thread: &str) -> Option<&'a Value> {
    value["live_text"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["thread_id"] == thread)
}
fn joined(entry: &Value) -> String {
    entry["deltas"]
        .as_array()
        .unwrap()
        .iter()
        .map(|delta| delta["text"].as_str().unwrap())
        .collect()
}
/// Polls until `thread`'s text from `at` reaches `wanted`; asserts the pieces are contiguous.
async fn read_text(
    control: &Arc<Control>,
    at: &mut String,
    thread: &str,
    start: u64,
    wanted: &str,
) -> String {
    let mut text = String::new();
    let mut offset = start;
    let mut attempt = String::new();
    while text != wanted {
        assert!(wanted.starts_with(&text), "unexpected text {text:?}");
        let value = poll(control, Some(at), &[thread], 30_000).await;
        assert_ne!(value["status"], "timeout", "no text for {wanted:?}");
        *at = cursor(&value);
        let Some(item) = entry(&value, thread) else {
            continue;
        };
        assert_eq!(item["gap"], false);
        attempt = item["attempt_id"].as_str().unwrap().to_owned();
        for delta in item["deltas"].as_array().unwrap() {
            assert_eq!(delta["offset"].as_u64().unwrap(), offset);
            let piece = delta["text"].as_str().unwrap();
            offset += piece.chars().count() as u64;
            text.push_str(piece);
        }
    }
    attempt
}
async fn attempts(control: &Control, thread: &str) -> Vec<String> {
    let thread = thread.to_owned();
    control
        .store
        .database
        .call(move |db| {
            let mut query = db.prepare(
                "SELECT a.id FROM model_attempt a JOIN turn t ON t.id=a.turn_id WHERE t.thread_id=?1 ORDER BY a.rowid",
            )?;
            let rows = query
                .query_map([thread], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .await
        .unwrap()
}
async fn saved(control: &Arc<Control>, thread: &str) -> String {
    let request = wait_request(
        control,
        Command::HistoryList(HistoryListArgs {
            thread_id: thread.to_owned(),
            excerpt_chars: 4096,
            limit: 50,
            cursor: None,
            run_id: None,
            since: None,
            state: None,
        }),
    );
    let response = watchdog("history list", control.request(request))
        .await
        .unwrap();
    assert!(response.ok, "{:?}", response.error);
    response.result.unwrap().to_string()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deltas_arrive_in_order_with_offsets_then_the_attempt_ends_and_saved_output_stays() {
    bounded(
        "deltas_arrive_in_order_with_offsets_then_the_attempt_ends_and_saved_output_stays",
        async {
            let mut fixture = Fixture::new().await;
            let control = fixture.control.clone();
            let t = thread(&control).await;
            send(&control, &t, "go").await;
            let provider = fixture.next_request().await;
            // An attempt with no text yet is not listed, and the baseline is immediate.
            let base = poll(&control, None, &[&t], 0).await;
            assert_eq!(base["status"], "baseline");
            assert_eq!(base["live_text"], json!([]));
            let mut at = cursor(&base);

            provider.delta("hello");
            let attempt = read_text(&control, &mut at, &t, 0, "hello").await;
            assert_eq!(attempts(&control, &t).await, std::slice::from_ref(&attempt));
            // Offsets count Unicode scalar values, not bytes: 5 ASCII then 3 kana and an emoji.
            provider.delta(" こんに😀");
            read_text(&control, &mut at, &t, 5, " こんに😀").await;
            provider.reasoning("hmm");
            let value = poll(&control, Some(&at), &[&t], 30_000).await;
            assert_eq!(value["status"], "timeout");
            assert_eq!(entry(&value, &t).unwrap()["deltas"], json!([]));
            assert_eq!(attempts(&control, &t).await, std::slice::from_ref(&attempt));
            at = cursor(&value);
            // Nothing new: the attempt stays listed with no text at timeout.
            let idle = poll(&control, Some(&at), &[&t], 0).await;
            assert_eq!(idle["status"], "timeout");
            assert_eq!(entry(&idle, &t).unwrap()["deltas"], json!([]));
            assert_eq!(entry(&idle, &t).unwrap()["attempt_id"], attempt);

            // A late joiner gets the retained text at once, complete from offset 0.
            let late = poll(&control, None, &[&t], 0).await;
            assert_eq!(late["status"], "baseline");
            let tail = entry(&late, &t).unwrap();
            assert_eq!(tail["gap"], false);
            assert_eq!(tail["coverage_v1"]["projection"], "ordinary_text_v1");
            assert_eq!(tail["coverage_v1"]["attempt_id"], attempt);
            let pieces = tail["deltas"].as_array().unwrap();
            assert_eq!(pieces.len(), 2);
            for (index, (offset, text)) in [(0, "hello"), (5, " こんに😀")].into_iter().enumerate() {
                assert_eq!(pieces[index]["text"], text);
                assert_eq!(pieces[index]["offset"], offset);
                assert_eq!(pieces[index]["coverage_v1"]["sequence"], index.to_string());
                assert_eq!(pieces[index]["coverage_v1"]["scalar_offset"], offset.to_string());
                assert_eq!(pieces[index]["coverage_v1"]["stream_kind"], "text");
            }
            assert_eq!(joined(tail), "hello こんに😀");

            let saved_text = "hello こんに😀";
            provider.complete(saved_text);
            settled(&control, &t).await;
            // No delta after the attempt ended; a hint told the watcher; saved output is what stays.
            let done = poll(&control, Some(&at), &[&t], 30_000).await;
            assert_eq!(done["status"], "changed");
            assert_eq!(done["live_text"], json!([]));
            let after_end = poll(&control, None, &[], 0).await;
            assert_eq!(after_end["live_text"], json!([]));
            assert!(saved(&control, &t).await.contains(saved_text));
            fixture.close().await;
        },
    )
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_multibyte_character_split_across_network_reads_arrives_whole() {
    bounded(
        "a_multibyte_character_split_across_network_reads_arrives_whole",
        async {
            let mut fixture = Fixture::new().await;
            let control = fixture.control.clone();
            let t = thread(&control).await;
            send(&control, &t, "go").await;
            let provider = fixture.next_request().await;
            let mut at = cursor(&poll(&control, None, &[&t], 0).await);
            let body = sse(&json!({"type":"response.output_text.delta","delta":"日本語😀"}));
            let split = body
                .iter()
                .position(|byte| *byte == "本".as_bytes()[0])
                .unwrap()
                + 1;
            provider.raw(&body[..split]);
            tokio::time::sleep(Duration::from_millis(50)).await;
            // Half a character is on the wire: the decoder holds it and no reader sees it.
            let waiting = poll(&control, Some(&at), &[&t], 0).await;
            assert_eq!(waiting["status"], "timeout");
            provider.raw(&body[split..]);
            read_text(&control, &mut at, &t, 0, "日本語😀").await;
            provider.delta("!");
            read_text(&control, &mut at, &t, 4, "!").await;
            provider.complete("日本語😀!");
            settled(&control, &t).await;
            fixture.close().await;
        },
    )
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_retry_is_a_new_attempt_whose_offsets_restart() {
    bounded("a_retry_is_a_new_attempt_whose_offsets_restart", async {
        let mut fixture = Fixture::with_retries().await;
        let control = fixture.control.clone();
        let t = thread(&control).await;
        send(&control, &t, "go").await;
        let first = fixture.next_request().await;
        let mut at = cursor(&poll(&control, None, &[&t], 0).await);
        first.delta("first try");
        let failed = read_text(&control, &mut at, &t, 0, "first try").await;
        // Transport loss: the body ends without a terminal event.
        drop(first);
        let second = fixture.next_request().await;
        second.delta("second");
        let retried = read_text(&control, &mut at, &t, 0, "second").await;
        assert_ne!(failed, retried);
        assert_eq!(
            attempts(&control, &t).await,
            [failed.clone(), retried.clone()]
        );
        second.complete("second");
        settled(&control, &t).await;
        let saved_output = saved(&control, &t).await;
        assert!(saved_output.contains("second"));
        fixture.close().await;
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_slow_reader_gets_an_explicit_gap_and_the_retained_tail() {
    bounded(
        "a_slow_reader_gets_an_explicit_gap_and_the_retained_tail",
        async {
            let mut fixture = Fixture::new().await;
            let control = fixture.control.clone();
            let t = thread(&control).await;
            send(&control, &t, "go").await;
            let provider = fixture.next_request().await;
            let slow = cursor(&poll(&control, None, &[&t], 0).await);
            let piece = "あ".repeat(341); // 1023 bytes, 341 characters.
            let mut tracking = slow.clone();
            let pieces = 100u64;
            for _ in 0..pieces {
                provider.delta(&piece);
            }
            provider.delta("END");
            // A follower that keeps up after the burst eventually reads the last piece.
            loop {
                let value = poll(&control, Some(&tracking), &[&t], 30_000).await;
                tracking = cursor(&value);
                if entry(&value, &t).is_some_and(|item| joined(item).ends_with("END")) {
                    break;
                }
            }
            let value = poll(&control, Some(&slow), &[&t], 0).await;
            let item = entry(&value, &t).unwrap();
            assert_eq!(item["gap"], true);
            let deltas = item["deltas"].as_array().unwrap();
            let start = deltas[0]["offset"].as_u64().unwrap();
            assert!(start > 0);
            let text = joined(item);
            assert!(text.ends_with("END"));
            assert!(text.len() <= 64 * 1024, "{}", text.len());
            // Offsets stay exact across the gap: the tail ends at the total streamed so far.
            assert_eq!(start + text.chars().count() as u64, pieces * 341 + 3);
            // Whoever saw the newest text has no gap, and the gap never harmed the stream.
            let current = poll(&control, Some(&cursor(&value)), &[&t], 0).await;
            assert_eq!(entry(&current, &t).unwrap()["gap"], false);
            let saved_text = format!("{}END", piece.repeat(pieces as usize));
            provider.complete(&saved_text);
            settled(&control, &t).await;
            fixture.close().await;
        },
    )
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_filter_excludes_other_threads_and_the_cursor_moves_past_them() {
    bounded(
        "a_filter_excludes_other_threads_and_the_cursor_moves_past_them",
        async {
            let mut fixture = Fixture::new().await;
            let control = fixture.control.clone();
            let a = thread(&control).await;
            let b = thread(&control).await;
            send(&control, &a, "go").await;
            let provider_a = fixture.next_request().await;
            send(&control, &b, "go").await;
            let provider_b = fixture.next_request().await;
            let mut at = cursor(&poll(&control, None, &[&a], 0).await);
            provider_b.delta("not for you");
            // Everything is listed unfiltered; wait for the hidden delta to land.
            loop {
                let unfiltered = poll(&control, None, &[], 0).await;
                if entry(&unfiltered, &b).is_some() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            let filtered = poll(&control, Some(&at), &[&a], 0).await;
            assert_eq!(filtered["status"], "timeout");
            assert_eq!(filtered["live_text"], json!([]));
            assert_ne!(cursor(&filtered), at, "the cursor moves past hidden text");
            at = cursor(&filtered);
            provider_a.delta("for you");
            read_text(&control, &mut at, &a, 0, "for you").await;
            provider_a.complete("for you");
            provider_b.complete("not for you");
            settled(&control, &a).await;
            settled(&control, &b).await;
            fixture.close().await;
        },
    )
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn watching_text_keeps_change_hints_and_a_plain_poll_never_sees_text() {
    bounded(
        "watching_text_keeps_change_hints_and_a_plain_poll_never_sees_text",
        async {
            let mut fixture = Fixture::new().await;
            let control = fixture.control.clone();
            let t = thread(&control).await;
            let other = thread(&control).await;
            send(&control, &t, "go").await;
            let provider = fixture.next_request().await;
            let plain_at = cursor(&poll(&control, None, &[&other], 0).await);
            let plain = {
                let control = control.clone();
                let request = wait_request(
                    &control,
                    wait_command(Some(&plain_at), &[&other], 600_000, false),
                );
                tokio::spawn(async move { control.request(request).await })
            };
            let mut at = cursor(&poll(&control, None, &[], 0).await);
            provider.delta("words");
            read_text(&control, &mut at, &t, 0, "words").await;
            tokio::time::sleep(Duration::from_millis(50)).await;
            assert!(
                !plain.is_finished(),
                "text woke a plain poll into returning"
            );
            // A saved change during the stream reaches the text watcher as a hint with the text state.
            let update = wait_request(
                &control,
                Command::ThreadSet(Box::new(ThreadSetArgs {
                    thread_id: Some(other.clone()),
                    title: Some("renamed".into()),
                    ..Default::default()
                })),
            );
            assert!(control.request(update).await.ok);
            let value = poll(&control, Some(&at), &[], 30_000).await;
            assert_eq!(value["status"], "changed");
            assert!(
                value["changes"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|change| change["thread_id"] == other)
            );
            assert!(entry(&value, &t).is_some());
            let response = watchdog("plain poll", plain).await.unwrap().unwrap();
            let result = response.result.unwrap();
            assert_eq!(result["status"], "changed");
            assert!(result.get("live_text").is_none(), "{result}");
            provider.complete("words");
            settled(&control, &t).await;
            fixture.close().await;
        },
    )
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_pending_text_poll_ends_on_disconnect_and_on_owner_shutdown() {
    bounded(
        "a_pending_text_poll_ends_on_disconnect_and_on_owner_shutdown",
        async {
            let fixture = Fixture::new().await;
            let control = fixture.control.clone();
            let at = cursor(&poll(&control, None, &[], 0).await);
            let token = CancellationToken::new();
            let disconnected = {
                let control = control.clone();
                let request = wait_request(&control, wait_command(Some(&at), &[], 600_000, true));
                let token = token.clone();
                tokio::spawn(async move { control.request_cancellable(request, token).await })
            };
            tokio::time::sleep(Duration::from_millis(50)).await;
            assert!(!disconnected.is_finished());
            token.cancel();
            assert!(
                !watchdog("disconnect", disconnected)
                    .await
                    .unwrap()
                    .unwrap()
                    .ok
            );
            let held = {
                let control = control.clone();
                let request = wait_request(&control, wait_command(Some(&at), &[], 600_000, true));
                tokio::spawn(async move { control.request(request).await })
            };
            tokio::time::sleep(Duration::from_millis(50)).await;
            assert!(!held.is_finished());
            let Fixture {
                _root,
                owner,
                control: owned,
                server,
                requests: _requests,
            } = fixture;
            owned.shutdown().await.unwrap();
            assert!(!watchdog("shutdown", held).await.unwrap().unwrap().ok);
            owner.shutdown().await.unwrap();
            server.abort();
        },
    )
    .await;
}

/// A live attempt holds text until it is stopped or interrupted, then the entry is gone.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_entry_is_freed_when_the_attempt_is_stopped_or_interrupted() {
    bounded(
        "the_entry_is_freed_when_the_attempt_is_stopped_or_interrupted",
        async {
            let mut fixture = Fixture::new().await;
            let control = fixture.control.clone();
            for stop in [true, false] {
                let t = thread(&control).await;
                send(&control, &t, "go").await;
                let provider = fixture.next_request().await;
                let mut at = cursor(&poll(&control, None, &[&t], 0).await);
                provider.delta("held");
                read_text(&control, &mut at, &t, 0, "held").await;
                let (run, turn) = {
                    let thread = t.clone();
                    control
                        .store
                        .database
                        .call(move |db| {
                            Ok(db.query_row(
                                "SELECT run_id,id FROM turn WHERE thread_id=?1 ORDER BY rowid DESC LIMIT 1",
                                [thread],
                                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                            )?)
                        })
                        .await
                        .unwrap()
                };
                if stop {
                    let request = wait_request(&control, Command::RunStop(RunTarget { run_id: run }));
                    assert!(control.request(request).await.ok);
                } else {
                    control.interrupt_turn(t.clone(), turn).await.unwrap();
                }
                // The provider stream stays open: only the stop or interrupt can end the attempt.
                while entry(&poll(&control, None, &[&t], 0).await, &t).is_some() {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
                assert_eq!(poll(&control, None, &[], 0).await["live_text"], json!([]));
                drop(provider);
            }
            fixture.close().await;
        },
    )
    .await;
}

/// A reader that asked and then never reads holds nothing the driver waits on.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stalled_reader_does_not_block_the_driver() {
    bounded("a_stalled_reader_does_not_block_the_driver", async {
        use futures_util::FutureExt;
        let mut fixture = Fixture::new().await;
        let control = fixture.control.clone();
        let t = thread(&control).await;
        send(&control, &t, "go").await;
        let provider = fixture.next_request().await;
        let at = cursor(&poll(&control, None, &[&t], 0).await);
        // Polled once, parked in the clock's Notify and never polled again: nobody reads its reply.
        let mut stalled = Box::pin(control.request(wait_request(
            &control,
            wait_command(Some(&at), &[&t], 600_000, true),
        )));
        assert!((&mut stalled).now_or_never().is_none());
        let piece = "x".repeat(4096);
        let mut body = String::new();
        let mut write = |count: usize| {
            for _ in 0..count {
                provider.delta(&piece);
                body.push_str(&piece);
            }
        };
        // Retained text large enough that these replies can exceed a socket buffer. Whether a
        // server write really blocks depends on the OS and is not observed here.
        write(16);
        // 12 pieces: the budget charges 40 bytes per delta, so 16 x 4096 could never be retained.
        wait_retained(&control, &t, 12 * 4096).await;
        #[cfg(unix)]
        let sockets = stalled_sockets(&fixture, &t).await;
        write(496);
        provider.complete(&body);
        settled(&control, &t).await;
        assert!(saved(&control, &t).await.contains(&piece[..64]));
        drop(stalled);
        #[cfg(unix)]
        {
            let (sockets, server) = sockets;
            drop(sockets);
            server.shutdown().await.unwrap();
        }
        fixture.close().await;
    })
    .await;
}

/// Waits until the baseline view holds `wanted` retained bytes for `thread`.
async fn wait_retained(control: &Arc<Control>, thread: &str, wanted: usize) {
    let mut held = 0;
    let reached = watchdog("retained text", async {
        while held < wanted {
            let value = poll(control, None, &[thread], 0).await;
            held = entry(&value, thread).map_or(0, |item| joined(item).len());
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await;
    assert!(reached.is_ok(), "held {held} of {wanted} retained bytes");
}

/// Real socket clients that send an `events.wait` with text and never read the reply.
#[cfg(unix)]
async fn stalled_sockets(
    fixture: &Fixture,
    thread: &str,
) -> (Vec<tokio::net::UnixStream>, shellboy::ipc::LocalServer) {
    use shellboy::protocol::{PROTOCOL_VERSION, read_frame, write_frame};
    let (send, receive) = mpsc::channel(8);
    let server = shellboy::ipc::LocalServer::bind(&fixture.owner, send)
        .await
        .unwrap();
    tokio::spawn(fixture.control.clone().serve(receive));
    let endpoint: Value = serde_json::from_slice(
        &std::fs::read(fixture.owner.loaded.data_dir.join("endpoint.json")).unwrap(),
    )
    .unwrap();
    let address = endpoint["address"].as_str().unwrap().to_owned();
    let mut sockets = Vec::new();
    for _ in 0..2 {
        let mut stream = tokio::net::UnixStream::connect(&address).await.unwrap();
        write_frame(&mut stream, &json!({"protocol_version":PROTOCOL_VERSION}))
            .await
            .unwrap();
        let _: Option<Value> = read_frame(&mut stream).await.unwrap();
        let request = wait_request(&fixture.control, wait_command(None, &[thread], 0, true));
        write_frame(&mut stream, &request).await.unwrap();
        sockets.push(stream);
    }
    (sockets, server)
}

/// Facts for the validation record, not an assertion: time from the provider's socket write to
/// the reply reaching a client that loops one long-poll. Run with
/// `cargo test --test integration events_text::measure -- --ignored --nocapture`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "measurement; prints facts"]
async fn measure_delta_latency_on_the_mock() {
    bounded("measure_delta_latency_on_the_mock", async {
        for transport in ["in-process", "local-ipc"] {
            let mut fixture = Fixture::new().await;
            let control = fixture.control.clone();
            let t = thread(&control).await;
            send(&control, &t, "go").await;
            let provider = fixture.next_request().await;
            let (send_ipc, receive_ipc) = mpsc::channel(32);
            let server = shellboy::ipc::LocalServer::bind(&fixture.owner, send_ipc)
                .await
                .unwrap();
            tokio::spawn(control.clone().serve(receive_ipc));
            let data_dir = fixture.owner.loaded.data_dir.clone();
            let total = 400u64;
            let mut at = cursor(&poll(&control, None, &[&t], 0).await);
            let sent = Arc::new(std::sync::Mutex::new(Vec::<std::time::Instant>::new()));
            let reader = {
                let control = control.clone();
                let sent = sent.clone();
                let t = t.clone();
                let transport = transport.to_owned();
                tokio::spawn(async move {
                    let mut samples = Vec::new();
                    let mut polls = 0u64;
                    let mut offset = 0u64;
                    while offset < total {
                        let value = if transport == "in-process" {
                            poll(&control, Some(&at), &[&t], 30_000).await
                        } else {
                            shellboy::ipc::call(
                                &data_dir,
                                wait_command(Some(&at), &[&t], 30_000, true),
                                None,
                                None,
                            )
                            .await
                            .unwrap()
                            .result
                            .unwrap()
                        };
                        let now = std::time::Instant::now();
                        polls += 1;
                        at = cursor(&value);
                        if let Some(item) = entry(&value, &t) {
                            let text = joined(item).chars().count() as u64;
                            let sent = sent.lock().unwrap();
                            for index in offset..offset + text {
                                samples.push(now.duration_since(sent[index as usize]));
                            }
                            offset += text;
                        }
                    }
                    (samples, polls)
                })
            };
            for _ in 0..total {
                sent.lock().unwrap().push(std::time::Instant::now());
                provider.delta("x");
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            let (mut samples, polls) = watchdog("measure reader", reader).await.unwrap().unwrap();
            samples.sort();
            println!(
                "{transport}: deltas={} polls={polls} p50={:?} p95={:?} max={:?}",
                samples.len(),
                samples[samples.len() / 2],
                samples[samples.len() * 95 / 100],
                samples[samples.len() - 1],
            );
            provider.complete(&"x".repeat(total as usize));
            settled(&control, &t).await;
            server.shutdown().await.unwrap();
            fixture.close().await;
        }
    })
    .await;
}
