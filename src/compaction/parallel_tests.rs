//! #224 stage 3: retry, parallel launch, ordering, gate queueing, cancellation.
use super::parallel::{Job, JobContext, Kind};
use super::tests::{database, seed_895, selected_path};
use super::*;
use crate::model::http::RateAdmission;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::{mpsc, oneshot},
};

enum Reply {
    Text(String),
    Status(u16, Value),
}
struct Pending {
    marker: String,
    reply: oneshot::Sender<Reply>,
}
/// A loopback provider whose replies the test controls.
struct Fake {
    url: String,
    arrivals: mpsc::UnboundedReceiver<Pending>,
    stash: Vec<Pending>,
    server: tokio::task::JoinHandle<()>,
}
impl Drop for Fake {
    fn drop(&mut self) {
        self.server.abort();
    }
}
fn marker_of(body: &Value) -> String {
    let text = body["input"][1]["content"][0]["text"]
        .as_str()
        .unwrap_or("");
    text.find("SEG")
        .map(|at| {
            text[at..]
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric())
                .collect()
        })
        .unwrap_or_else(|| "other".into())
}
fn completed(text: &str) -> Value {
    json!({"status":"completed","output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":text}]}]})
}
async fn read_request(stream: &mut tokio::net::TcpStream) -> Value {
    let mut bytes = Vec::new();
    let mut chunk = [0; 8192];
    loop {
        let n = stream.read(&mut chunk).await.unwrap();
        assert!(n > 0);
        bytes.extend_from_slice(&chunk[..n]);
        if let Some(at) = bytes.windows(4).position(|p| p == b"\r\n\r\n") {
            let headers = String::from_utf8_lossy(&bytes[..at]).to_lowercase();
            let size: usize = headers
                .lines()
                .find_map(|line| {
                    line.strip_prefix("content-length:")
                        .map(|v| v.trim().parse().unwrap())
                })
                .unwrap();
            if bytes.len() >= at + 4 + size {
                return serde_json::from_slice(&bytes[at + 4..at + 4 + size]).unwrap();
            }
        }
    }
}
async fn write_response(stream: &mut tokio::net::TcpStream, status: u16, body: &Value) {
    let body = body.to_string();
    let reason = if status == 200 { "OK" } else { "Error" };
    let _ = stream
        .write_all(format!("HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes())
        .await;
}
impl Fake {
    async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/v1", listener.local_addr().unwrap());
        let (tx, arrivals) = mpsc::unbounded_channel();
        let server = tokio::spawn(async move {
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                let tx = tx.clone();
                tokio::spawn(async move {
                    let body = read_request(&mut stream).await;
                    let (reply, wait) = oneshot::channel();
                    let _ = tx.send(Pending {
                        marker: marker_of(&body),
                        reply,
                    });
                    match wait.await {
                        Ok(Reply::Text(text)) => {
                            write_response(&mut stream, 200, &completed(&text)).await
                        }
                        Ok(Reply::Status(status, body)) => {
                            write_response(&mut stream, status, &body).await
                        }
                        Err(_) => {}
                    }
                });
            }
        });
        Self {
            url,
            arrivals,
            stash: Vec::new(),
            server,
        }
    }
    /// The next request for `marker`, buffering any other that arrives first.
    async fn take(&mut self, marker: &str) -> Pending {
        if let Some(at) = self.stash.iter().position(|p| p.marker == marker) {
            return self.stash.remove(at);
        }
        loop {
            let next = self.arrivals.recv().await.expect("server is running");
            if next.marker == marker {
                return next;
            }
            self.stash.push(next);
        }
    }
    fn nothing_else_arrived(&mut self) -> bool {
        self.stash.is_empty() && self.arrivals.try_recv().is_err()
    }
}
impl Pending {
    fn ok(self, text: &str) {
        let _ = self.reply.send(Reply::Text(text.into()));
    }
    fn status(self, status: u16, body: Value) {
        let _ = self.reply.send(Reply::Status(status, body));
    }
}

fn config(url: &str) -> Arc<Config> {
    let mut config = Config::default();
    config.connections.insert(
        "fixture".into(),
        crate::config::Connection {
            base_url: url.into(),
            api_key_env: None,
            headers: Default::default(),
            default_effort: None,
            ssl_verify: true,
            ..Default::default()
        },
    );
    Arc::new(config)
}
fn request(marker: &str) -> Value {
    model::response_request(
        "fixture",
        vec![
            json!({"role":"developer","content":[{"type":"input_text","text":"summarize"}]}),
            json!({"role":"user","content":[{"type":"input_text","text":marker}]}),
        ],
        None,
        false,
    )
}
fn jobs(count: usize) -> Vec<Job> {
    (0..count)
        .map(|i| {
            Job::new(
                vec![i],
                CompactionKeys::summary_part(&format!("parallel-test-{i}")),
                request(&format!("SEG{i}")),
                vec![format!("n{i}")],
                (i, i + 1, 0),
            )
        })
        .collect()
}
struct Rig {
    _root: tempfile::TempDir,
    db: DbWorker,
    compactor: Compactor,
    snapshot: Snapshot,
}
async fn rig(url: &str) -> Rig {
    let (root, db) = database().await;
    let compactor = Compactor::new(db.clone(), config(url));
    // A stored Thread: the post-wait fence revalidates the snapshot.
    seed_895(&db).await;
    let snapshot = compactor.snapshot("t", true).await.unwrap();
    Rig {
        _root: root,
        db,
        compactor,
        snapshot,
    }
}
fn budget(
    ledger: &mut usage::UsageLedger,
    map_limit: usize,
    reduce_limit: usize,
) -> SummaryBudget<'_> {
    SummaryBudget {
        source_characters: 1000,
        summary_characters: 0,
        limit: 500,
        requests: 0,
        map_requests: 0,
        reduce_requests: 0,
        map_limit,
        reduce_limit,
        segment_chars: 64_000,
        reduce_rounds: 4,
        usage: ledger,
    }
}
async fn run(
    rig: &Rig,
    kind: Kind,
    jobs: Vec<Job>,
    budget: &mut SummaryBudget<'_>,
    cancel: &CancellationToken,
) -> Result<parallel::JobParts, ApiError> {
    run_split(rig, kind, jobs, budget, cancel, None).await
}
async fn run_split(
    rig: &Rig,
    kind: Kind,
    jobs: Vec<Job>,
    budget: &mut SummaryBudget<'_>,
    cancel: &CancellationToken,
    split: Option<parallel::Split<'_>>,
) -> Result<parallel::JobParts, ApiError> {
    let client = rig.compactor.clients.get("fixture").unwrap();
    let connection = &rig.compactor.config.connections["fixture"];
    rig.compactor
        .run_jobs(
            &JobContext {
                kind,
                snapshot: &rig.snapshot,
                connection,
                client: &client,
                cancel,
                split,
            },
            jobs,
            budget,
            None,
        )
        .await
}
/// Puts the profile into a cooldown with a real 429 and returns once seen.
async fn throttle(rig: &Rig, fake: &mut Fake) {
    let client = rig.compactor.clients.get("fixture").unwrap();
    let RateReservation::Ready(admission) = client.reserve_rate_admission(None).unwrap() else {
        panic!("a fresh gate admits immediately");
    };
    let call = tokio::spawn(async move {
        Compactor::summary_response(
            &client,
            "other-thread",
            request("SEGPRE"),
            CancellationToken::new(),
            (admission, 0),
        )
        .await
        .err()
        .map(|failure| failure.facts.status)
    });
    fake.take("SEGPRE")
        .await
        .status(429, json!({"error":{"message":"slow down"}}));
    assert_eq!(call.await.unwrap(), Some(Some(429)));
}
use crate::model::http::RateReservation;

async fn guarded<T: Send + 'static>(
    name: &'static str,
    future: impl std::future::Future<Output = T> + Send + 'static,
) -> T {
    crate::test_watchdog::watchdog(name, future).await.unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn requests_launch_together_and_return_in_original_order() {
    guarded("parallel order", async {
        for kind in [Kind::Map, Kind::Reduce] {
            let mut fake = Fake::start().await;
            let rig = rig(&fake.url).await;
            let mut ledger = usage::UsageLedger::new(&rig.snapshot);
            let cancel = CancellationToken::new();
            let work = async {
                let mut budget = budget(&mut ledger, 8, 8);
                run(&rig, kind, jobs(3), &mut budget, &cancel).await
            };
            let drive = async {
                // All three are in flight before any answer: not serial.
                let (a, b, c) = (
                    fake.take("SEG0").await,
                    fake.take("SEG1").await,
                    fake.take("SEG2").await,
                );
                c.ok("T2");
                a.ok("T0");
                b.ok("T1");
            };
            let (parts, ()) = tokio::join!(work, drive);
            let parts = parts.unwrap();
            let texts: Vec<_> = parts.parts.iter().map(|p| p.text.as_str()).collect();
            assert_eq!(texts, ["T0", "T1", "T2"]);
            assert_eq!(parts.created, 3);
            rig.db.shutdown().await.unwrap();
        }
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_429_is_retried_inside_the_budget_and_a_cached_part_is_not_requested_again() {
    guarded("retry on 429", async {
        let mut fake = Fake::start().await;
        let rig = rig(&fake.url).await;
        let mut ledger = usage::UsageLedger::new(&rig.snapshot);
        let cancel = CancellationToken::new();
        let mut first = budget(&mut ledger, 3, 3);
        let drive = async {
            fake.take("SEG0")
                .await
                .status(429, json!({"error":{"message":"slow down"}}));
            // The retry waits in the gate's cooldown, then is sent again.
            fake.take("SEG0").await.ok("done");
        };
        let (parts, ()) = tokio::join!(run(&rig, Kind::Map, jobs(1), &mut first, &cancel), drive);
        let parts = parts.unwrap();
        assert_eq!(parts.parts[0].text, "done");
        assert_eq!(first.map_requests, 2, "the retry consumed one slot");
        let mut second_ledger = usage::UsageLedger::new(&rig.snapshot);
        let mut second = budget(&mut second_ledger, 3, 3);
        let again = run(&rig, Kind::Map, jobs(1), &mut second, &cancel)
            .await
            .unwrap();
        assert_eq!((again.reused, again.created), (1, 0));
        assert_eq!(
            second.map_requests, 0,
            "a cached part is never re-requested"
        );
        assert!(fake.nothing_else_arrived());
        rig.db.shutdown().await.unwrap();
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retry_never_exceeds_the_existing_budget() {
    guarded("retry budget", async {
        let mut fake = Fake::start().await;
        let rig = rig(&fake.url).await;
        let mut ledger = usage::UsageLedger::new(&rig.snapshot);
        let mut budget = budget(&mut ledger, 1, 1);
        let cancel = CancellationToken::new();
        let drive = async {
            fake.take("SEG0")
                .await
                .status(503, json!({"error":{"message":"unavailable"}}));
        };
        let (result, ()) = tokio::join!(run(&rig, Kind::Map, jobs(1), &mut budget, &cancel), drive);
        let error = result.err().expect("budget exhausted");
        assert!(error.hint.as_deref().unwrap().contains("budget exhausted"));
        assert_eq!(budget.map_requests, 1);
        assert!(fake.nothing_else_arrived());
        rig.db.shutdown().await.unwrap();
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_usage_limit_is_not_retried() {
    guarded("usage limit", async {
        let mut fake = Fake::start().await;
        let rig = rig(&fake.url).await;
        let mut ledger = usage::UsageLedger::new(&rig.snapshot);
        let mut budget = budget(&mut ledger, 5, 5);
        let cancel = CancellationToken::new();
        let resets = chrono::Utc::now().timestamp() + 3600;
        let wrapped = format!(
            "litellm.RateLimitError: RateLimitError: ChatgptException - {}\nReceived Model Group=m",
            json!({"error":{"type":"usage_limit_reached","resets_at":resets,"limit_window_minutes":10080}})
        );
        let drive = async {
            fake.take("SEG0")
                .await
                .status(429, json!({"error":{"message":wrapped}}));
        };
        let (result, ()) = tokio::join!(run(&rig, Kind::Map, jobs(1), &mut budget, &cancel), drive);
        assert!(result.is_err());
        assert_eq!(budget.map_requests, 1, "no resend before the reset");
        assert!(fake.nothing_else_arrived());
        rig.db.shutdown().await.unwrap();
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn after_a_429_the_others_wait_in_the_gate_and_all_complete() {
    guarded("gate wait", async {
        let mut fake = Fake::start().await;
        let rig = rig(&fake.url).await;
        throttle(&rig, &mut fake).await;
        let mut ledger = usage::UsageLedger::new(&rig.snapshot);
        let mut budget = budget(&mut ledger, 8, 8);
        let cancel = CancellationToken::new();
        let drive = async {
            // Only the probe is admitted until it answers at headers.
            let first = fake.take("SEG0").await;
            assert!(fake.nothing_else_arrived(), "waiters stay in the gate");
            first.ok("T0");
            fake.take("SEG1").await.ok("T1");
            fake.take("SEG2").await.ok("T2");
        };
        let (parts, ()) = tokio::join!(run(&rig, Kind::Map, jobs(3), &mut budget, &cancel), drive);
        let texts: Vec<_> = parts.unwrap().parts.into_iter().map(|p| p.text).collect();
        assert_eq!(texts, ["T0", "T1", "T2"]);
        rig.db.shutdown().await.unwrap();
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn another_threads_request_is_queued_between_the_summarys_requests() {
    guarded("interleaving", async {
        let mut fake = Fake::start().await;
        let rig = rig(&fake.url).await;
        throttle(&rig, &mut fake).await;
        let client = rig.compactor.clients.get("fixture").unwrap();
        let mut ledger = usage::UsageLedger::new(&rig.snapshot);
        let mut budget = budget(&mut ledger, 8, 8);
        let cancel = CancellationToken::new();
        let conversation = {
            let client = client.clone();
            async move {
                // Ready at the same time as the summary; takes its own ticket.
                let RateReservation::Waiting(waiter) = client.reserve_rate_admission(None).unwrap()
                else {
                    panic!("the cooling profile queues");
                };
                let lease = waiter.wait(&CancellationToken::new()).await.unwrap();
                Compactor::summary_response(
                    &client,
                    "other-thread",
                    request("SEGCONV"),
                    CancellationToken::new(),
                    (RateAdmission::Lease(lease), 0),
                )
                .await
                .unwrap();
            }
        };
        let drive = async {
            let mut order = Vec::new();
            for _ in 0..4 {
                let next = fake.arrivals.recv().await.unwrap();
                order.push(next.marker.clone());
                next.ok("T");
            }
            order
        };
        let (parts, (), order) = tokio::join!(
            run(&rig, Kind::Map, jobs(3), &mut budget, &cancel),
            conversation,
            drive
        );
        parts.unwrap();
        let conv = order.iter().position(|m| m == "SEGCONV").unwrap();
        let last = order.iter().position(|m| m == "SEG2").unwrap();
        assert!(
            conv < last,
            "conversation sent before the last map: {order:?}"
        );
        rig.db.shutdown().await.unwrap();
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancellation_with_requests_in_flight_adopts_nothing() {
    guarded("cancellation", async {
        let mut fake = Fake::start().await;
        let (_root, db) = database().await;
        seed_895(&db).await;
        let compactor = Compactor::new(db.clone(), config(&fake.url));
        let snapshot = compactor.snapshot("t", true).await.unwrap();
        let range = policy::unsummarized_prefix(&snapshot).unwrap();
        let before = db.call(|db| Ok(selected_path(db, "t")?)).await.unwrap();
        let cancel = CancellationToken::new();
        let summary = tokio::spawn({
            let (compactor, cancel) = (compactor.clone(), cancel.clone());
            async move {
                compactor
                    .summarize_snapshot(snapshot, range, cancel, None, None)
                    .await
            }
        });
        let _first = fake.arrivals.recv().await.unwrap();
        let _second = fake.arrivals.recv().await.unwrap();
        cancel.cancel();
        assert!(summary.await.unwrap().is_err());
        let (path, replacements, ledger) = db
            .call(move |db| {
                let replacements: i64 =
                    db.query_row("SELECT count(*) FROM replacement", [], |r| r.get(0))?;
                let ledger: String = db.query_row(
                    "SELECT value FROM app_meta WHERE key LIKE 'summary.usage.t.%'",
                    [],
                    |r| r.get(0),
                )?;
                Ok((selected_path(db, "t")?, replacements, ledger))
            })
            .await
            .unwrap();
        assert_eq!(path, before);
        assert_eq!(replacements, 0);
        let ledger: Value = serde_json::from_str(&ledger).unwrap();
        let observations = ledger["rate_observations"].as_array().unwrap();
        assert_eq!(
            observations.len() as u64,
            ledger["requests"].as_u64().unwrap()
        );
        assert!(observations.iter().all(|o| !o.is_null()), "{ledger}");
        db.shutdown().await.unwrap();
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_fatal_failure_cancels_and_drains_the_request_in_flight() {
    guarded("fatal sibling", async {
        let mut fake = Fake::start().await;
        let rig = rig(&fake.url).await;
        let mut ledger = usage::UsageLedger::new(&rig.snapshot);
        let mut budget = budget(&mut ledger, 8, 8);
        let cancel = CancellationToken::new();
        let drive = async {
            let (bad, _sibling) = (fake.take("SEG0").await, fake.take("SEG1").await);
            // The sibling is never answered: only cancellation can end it.
            bad.status(400, json!({"error":{"message":"bad request"}}));
            _sibling
        };
        let (result, _sibling) =
            tokio::join!(run(&rig, Kind::Map, jobs(2), &mut budget, &cancel), drive);
        assert!(result.is_err());
        assert_eq!(budget.map_requests, 2);
        let ledger: Value = serde_json::from_str(
            &rig.db
                .call(|db| {
                    Ok(db.query_row(
                        "SELECT value FROM app_meta WHERE key LIKE 'summary.usage.t.%'",
                        [],
                        |r| r.get::<_, String>(0),
                    )?)
                })
                .await
                .unwrap(),
        )
        .unwrap();
        // Both requests were accounted before the run returned (drained).
        let observations = ledger["rate_observations"].as_array().unwrap();
        assert_eq!(observations.len(), 2, "{ledger}");
        assert!(observations.iter().all(|o| !o.is_null()), "{ledger}");
        rig.db.shutdown().await.unwrap();
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_context_limit_split_keeps_the_original_order_with_out_of_order_replies() {
    guarded("split order", async {
        let mut fake = Fake::start().await;
        let mut rig = rig(&fake.url).await;
        // "t" would be redacted from the error text as a session header secret.
        rig.snapshot.thread = "thread-long-id".into();
        let mut ledger = usage::UsageLedger::new(&rig.snapshot);
        let mut budget = budget(&mut ledger, 8, 8);
        let cancel = CancellationToken::new();
        let build = |span: (usize, usize, u8), path: Vec<usize>| {
            Job::new(
                path,
                CompactionKeys::summary_part(&format!("split-test-{}-{}", span.0, span.1)),
                request(&format!("SEG{}x{}", span.0, span.1)),
                vec![format!("n{}", span.0)],
                span,
            )
        };
        let jobs = vec![build((0, 2, 0), vec![0]), build((2, 3, 0), vec![1])];
        let drive = async {
            let (whole, b) = (fake.take("SEG0x2").await, fake.take("SEG2x3").await);
            whole.status(
                400,
                json!({"error":{"code":"context_length_exceeded","message":"too long"}}),
            );
            b.ok("B");
            let (h1, h2) = (fake.take("SEG0x1").await, fake.take("SEG1x2").await);
            h2.ok("H2");
            h1.ok("H1");
        };
        let (parts, ()) = tokio::join!(
            run_split(&rig, Kind::Map, jobs, &mut budget, &cancel, Some(&build)),
            drive
        );
        let texts: Vec<_> = parts.unwrap().parts.into_iter().map(|p| p.text).collect();
        assert_eq!(texts, ["H1", "H2", "B"]);
        rig.db.shutdown().await.unwrap();
    })
    .await;
}
