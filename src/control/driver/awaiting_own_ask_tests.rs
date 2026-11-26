//! #433 witnesses through real ask, reply, cancel and Run-end paths: an ask back
//! keeps a reply duty open, and the Run that receives the answer owes it again.
use super::*;
use crate::app::DataOwner;
use crate::protocol::{
    Command, Deliver, MessageAskArgs, MessageCancelArgs, MessageListArgs, MessageReplyArgs, Origin,
    Request,
};

struct Fixture {
    _root: tempfile::TempDir,
    owner: DataOwner,
    control: Arc<Control>,
}

async fn fixture(threads: &[&str]) -> Fixture {
    let root = tempfile::tempdir().unwrap();
    let config = root.path().join("config.toml");
    std::fs::write(&config, "default_connection='fixture'\ndefault_model='fixture'\n[connections.fixture]\nbase_url='http://127.0.0.1:9/v1'\n").unwrap();
    let owner = DataOwner::start(
        Some(config),
        Some(root.path().join("data")),
        root.path().into(),
    )
    .await
    .unwrap();
    let control = Control::new(&owner, root.path().into());
    let threads: Vec<String> = threads.iter().map(|t| (*t).to_owned()).collect();
    control.store.database.call(move |db| {
        for thread in &threads {
            db.execute("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES(?1,'/','/AGENTS.md','fixture','fixture','now')", [thread])?;
        }
        Ok(())
    }).await.unwrap();
    Fixture {
        _root: root,
        owner,
        control,
    }
}

fn origin(thread: &str, run: &str, turn: &str) -> Origin {
    Origin {
        instance_id: "fixture".into(),
        thread_id: thread.into(),
        run_id: run.into(),
        turn_id: turn.into(),
        execution_id: format!("{turn}-shell"),
        token: "fixture-token".into(),
    }
}

impl Fixture {
    async fn execute(&self, command: Command, caller: Option<Origin>) -> Value {
        self.control
            .store
            .execute(
                Request::new(
                    format!(
                        "{}:{}",
                        self.control.store.instance_id,
                        uuid::Uuid::new_v4()
                    ),
                    command,
                ),
                caller,
            )
            .await
            .unwrap()
            .result
    }

    /// Returns (message_id, assigned run_id).
    async fn ask(
        &self,
        caller: Option<Origin>,
        thread: Option<&str>,
        to: Option<&str>,
    ) -> (String, String) {
        let result = self
            .execute(
                Command::MessageAsk(MessageAskArgs {
                    human: false,
                    sender_label: None,
                    thread_id: thread.map(str::to_owned),
                    to: to.map(str::to_owned),
                    deliver: Deliver::Next,
                    text: "please answer".into(),
                }),
                caller,
            )
            .await;
        (
            result["message_id"].as_str().unwrap().to_owned(),
            result["run_id"].as_str().unwrap_or_default().to_owned(),
        )
    }

    async fn reply(&self, caller: Option<Origin>, message: &str) {
        self.execute(
            Command::MessageReply(MessageReplyArgs {
                message_id: message.into(),
                text: "answer".into(),
                sender_label: None,
            }),
            caller,
        )
        .await;
    }

    /// Prepares the next request of a live Run: path admission of its input.
    async fn admit(&self, thread: &str, run: &str) -> Box<Snapshot> {
        match next(&self.control, thread, run).await.unwrap() {
            Next::Ready(snapshot) => snapshot,
            _ => panic!("{thread} must have a runnable Turn"),
        }
    }

    /// The model's answer is adopted (input applied), then its Turn completes.
    async fn adopt(&self, snapshot: &Snapshot) {
        let (receipts, run) = (snapshot.receipts.clone(), snapshot.run.clone());
        self.control
            .store
            .database
            .call(move |db| {
                for receipt in &receipts {
                    db.execute(
                        "UPDATE receipt SET state='applied' WHERE id=?1 AND state='accepted'",
                        [receipt],
                    )?;
                }
                db.execute(
                    "UPDATE delivery SET state='processed' WHERE recipient_id=?1",
                    [&run],
                )?;
                Ok(())
            })
            .await
            .unwrap();
    }

    async fn end_turn(&self, snapshot: &Snapshot) {
        let turn = snapshot.turn.clone();
        self.control
            .store
            .database
            .call(move |db| {
                db.execute(
                    "UPDATE turn SET state='completed',ended_at='later' WHERE id=?1",
                    [turn],
                )?;
                Ok(())
            })
            .await
            .unwrap();
    }

    async fn reply_fact(&self, message: &str) -> (Value, Option<String>) {
        let message = message.to_owned();
        self.control
            .store
            .database
            .call(move |db| {
                let (reply, run): (String, Option<String>) = db.query_row(
                    "SELECT json_extract(result_json,'$.reply'),run_id FROM receipt WHERE message_node_id=?1",
                    [message],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )?;
                Ok((serde_json::from_str(&reply).unwrap(), run))
            })
            .await
            .unwrap()
    }

    async fn run_state(&self, run: &str) -> String {
        let run = run.to_owned();
        self.control
            .store
            .database
            .call(move |db| {
                Ok(db.query_row("SELECT state FROM run WHERE id=?1", [run], |r| r.get(0))?)
            })
            .await
            .unwrap()
    }

    async fn live_run(&self, thread: &str) -> String {
        let thread = thread.to_owned();
        self.control
            .store
            .database
            .call(move |db| {
                Ok(db.query_row(
                    "SELECT id FROM run WHERE thread_id=?1 AND state='live'",
                    [thread],
                    |r| r.get(0),
                )?)
            })
            .await
            .unwrap()
    }

    /// Every maintenance sweep for terminal Runs, as at startup.
    async fn sweep_old_runs(&self, old: &str) {
        let old = old.to_owned();
        self.control
            .store
            .database
            .call(move |db| {
                let tx = db.transaction()?;
                super::super::reply::terminal_tx(&tx, &old, "sweep", &mut Vec::new())?;
                let mut cursor = None;
                loop {
                    let batch = super::super::reply::terminal_batch_tx(&tx, cursor, "sweep")?;
                    cursor = batch.cursor;
                    if cursor.is_none() {
                        break;
                    }
                }
                tx.commit()?;
                Ok(())
            })
            .await
            .unwrap();
    }

    /// A human message to the worker: starts a Run, resuming held input.
    async fn send(&self, thread: &str) -> String {
        self.execute(
            Command::MessageSend(crate::protocol::MessageSendArgs {
                max_model_requests: None,
                resume_recovery: false,
                human: true,
                sender_label: None,
                thread_id: Some(thread.into()),
                text: "carry on".into(),
                deliver: Deliver::Next,
            }),
            None,
        )
        .await["run_id"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    /// Explicit Stop: admission, then the driver's drained-stop settlement.
    async fn begin_stop(&self, run: &str) {
        let run = run.to_owned();
        self.control
            .store
            .database
            .call(move |db| {
                let tx = db.transaction()?;
                super::super::state::admit_run_stop_tx(
                    &tx,
                    &run,
                    super::super::state::RunStopAdmission::Explicit { at: "stop" },
                )?;
                tx.commit()?;
                Ok(())
            })
            .await
            .unwrap();
    }

    async fn finish_stop(&self, run: &str, thread: &str) {
        let owned = vec![(run.to_owned(), thread.to_owned())];
        self.control
            .store
            .database
            .call(move |db| {
                let tx = db.transaction()?;
                let mut keys = Vec::new();
                super::super::state::stop_drained_runs(&tx, &owned, false, &mut keys)?;
                super::super::reply::terminal_tx(&tx, &owned[0].0, "stopped", &mut keys)?;
                tx.commit()?;
                Ok(())
            })
            .await
            .unwrap();
    }

    async fn shutdown(self) {
        self.control.shutdown().await.unwrap();
        self.owner.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn ask_back_keeps_duty_open_and_the_answering_run_owes_it_once() {
    // A Thread ask back and a named ask (the `--to NAME` leader flow) both block.
    for named in [false, true] {
        let f = fixture(&["worker", "helper"]).await;
        let (duty, first_run) = f.ask(None, Some("worker"), None).await;
        let first = f.admit("worker", &first_run).await;
        f.adopt(&first).await;
        let asker = origin("worker", &first_run, &first.turn);
        let (question, helper_run) = if named {
            f.ask(Some(asker), None, Some("claude")).await
        } else {
            f.ask(Some(asker), Some("helper"), None).await
        };
        f.end_turn(&first).await;
        assert!(matches!(
            next(&f.control, "worker", &first_run).await.unwrap(),
            Next::Done
        ));
        assert_eq!(f.run_state(&first_run).await, "completed");
        let (reply, delivered) = f.reply_fact(&duty).await;
        assert_eq!(reply["state"], "awaiting_own_ask");
        assert_eq!(reply["blocking_message_id"], question.as_str());
        assert!(
            reply.get("reminded_at").is_none(),
            "no reminder while awaiting"
        );
        assert_eq!(delivered.as_deref(), Some(first_run.as_str()));

        // Every read surface names the waiting duty and its blocking ask.
        let listed = f
            .execute(
                Command::MessageList(MessageListArgs {
                    owed_by_thread_id: Some("worker".into()),
                    asked_by_thread_id: None,
                    to: None,
                    limit: 10,
                    cursor: None,
                }),
                None,
            )
            .await;
        assert_eq!(listed["items"][0]["message_id"], duty.as_str());
        assert_eq!(listed["items"][0]["reply"]["state"], "awaiting_own_ask");
        assert_eq!(
            listed["items"][0]["reply"]["blocking_message_id"],
            question.as_str()
        );
        let got = f
            .execute(
                Command::MessageGet(crate::protocol::MessageTarget {
                    message_id: duty.clone(),
                }),
                None,
            )
            .await;
        assert_eq!(got["reply"]["blocking_message_id"], question.as_str());
        let summary = f
            .control
            .store
            .database
            .call(|db| Ok(super::super::reply::pending_counts(db, "worker")?))
            .await
            .unwrap();
        assert_eq!(summary.owed_by.count, 1, "an awaiting duty is still owed");
        assert_eq!(summary.awaiting_own_ask.len(), 1);
        assert_eq!(summary.awaiting_own_ask[0].message_id, duty);
        assert_eq!(summary.awaiting_own_ask[0].blocking_message_id, question);

        // The old Run's sweeps never settle the open duty.
        f.sweep_old_runs(&first_run).await;
        assert_eq!(f.reply_fact(&duty).await.0["state"], "awaiting_own_ask");

        // The answer arrives in a later Run, which now owes the duty.
        if named {
            f.reply(None, &question).await;
        } else {
            let helper = f.admit("helper", &helper_run).await;
            f.adopt(&helper).await;
            f.reply(Some(origin("helper", &helper_run, &helper.turn)), &question)
                .await;
        }
        let second_run = f.live_run("worker").await;
        assert_ne!(second_run, first_run);
        let (reply, delivered) = f.reply_fact(&duty).await;
        assert_eq!(reply["state"], "pending");
        assert!(reply.get("blocking_message_id").is_none());
        assert_eq!(reply["reopened_after"], question.as_str());
        assert!(
            reply.get("owner_run_id").is_none(),
            "owned only once admitted"
        );
        assert_eq!(
            delivered.as_deref(),
            Some(first_run.as_str()),
            "delivery provenance is unchanged"
        );
        f.sweep_old_runs(&first_run).await;
        assert_eq!(f.reply_fact(&duty).await.0["state"], "pending");

        // The Run admitting the answer owns the duty; its end without a reply
        // ignores it, without a reminder.
        let second = f.admit("worker", &second_run).await;
        assert_eq!(
            f.reply_fact(&duty).await.0["owner_run_id"],
            second_run.as_str()
        );
        f.adopt(&second).await;
        f.end_turn(&second).await;
        assert!(matches!(
            next(&f.control, "worker", &second_run).await.unwrap(),
            Next::Done
        ));
        let reply = f.reply_fact(&duty).await.0;
        assert_eq!(reply["state"], "ignored");
        assert!(reply.get("reminded_at").is_none());
        f.shutdown().await;
    }
}

#[tokio::test]
async fn cancelled_own_ask_reowes_the_duty_to_the_run_that_receives_the_outcome() {
    let f = fixture(&["worker", "helper"]).await;
    let (duty, first_run) = f.ask(None, Some("worker"), None).await;
    let first = f.admit("worker", &first_run).await;
    f.adopt(&first).await;
    let (question, _) = f
        .ask(
            Some(origin("worker", &first_run, &first.turn)),
            Some("helper"),
            None,
        )
        .await;
    f.end_turn(&first).await;
    assert!(matches!(
        next(&f.control, "worker", &first_run).await.unwrap(),
        Next::Done
    ));
    assert_eq!(f.reply_fact(&duty).await.0["state"], "awaiting_own_ask");

    // Withdrawing the ask delivers its cancellation outcome to a new Run.
    let cancelled = f
        .execute(
            Command::MessageCancel(MessageCancelArgs {
                message_ids: vec![question.clone()],
            }),
            None,
        )
        .await;
    assert_eq!(cancelled["items"][0]["message_id"], question.as_str());
    assert_eq!(f.reply_fact(&question).await.0["state"], "cancelled");
    let outcome_run = f.live_run("worker").await;
    let reply = f.reply_fact(&duty).await.0;
    assert_eq!(reply["state"], "pending");
    assert!(reply.get("owner_run_id").is_none());

    // That Run admits the outcome, owns the duty and can settle it.
    let handling = f.admit("worker", &outcome_run).await;
    assert_eq!(
        f.reply_fact(&duty).await.0["owner_run_id"],
        outcome_run.as_str()
    );
    assert!(
        handling
            .events
            .iter()
            .any(|event| *event == format!("reply-outcome-{question}"))
    );
    f.adopt(&handling).await;
    f.reply(Some(origin("worker", &outcome_run, &handling.turn)), &duty)
        .await;
    assert_eq!(f.reply_fact(&duty).await.0["state"], "replied");
    f.shutdown().await;
}

#[tokio::test]
async fn older_ask_and_pre_upgrade_duty_are_never_blocked() {
    let f = fixture(&["older", "upgraded", "helper"]).await;
    // An ask sent before the duty was admitted does not block it.
    let (_, _) = f
        .ask(
            Some(origin("older", "earlier", "earlier")),
            Some("helper"),
            None,
        )
        .await;
    let (duty, run) = f.ask(None, Some("older"), None).await;
    let turn = f.admit("older", &run).await;
    f.adopt(&turn).await;
    f.end_turn(&turn).await;
    assert!(matches!(
        next(&f.control, "older", &run).await.unwrap(),
        Next::Ready(_)
    ));
    let reply = f.reply_fact(&duty).await.0;
    assert_eq!(reply["state"], "pending");
    assert!(reply["reminded_at"].is_string());

    // A duty admitted before admission times were saved has no evidence that a
    // later ask followed it, so the ordinary reminder applies.
    let (duty, run) = f.ask(None, Some("upgraded"), None).await;
    let turn = f.admit("upgraded", &run).await;
    let saved = duty.clone();
    f.control
        .store
        .database
        .call(move |db| {
            db.execute(
                "UPDATE receipt SET result_json=json_remove(result_json,'$.reply.admitted_at') WHERE message_node_id=?1",
                [saved],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    f.adopt(&turn).await;
    f.ask(
        Some(origin("upgraded", &run, &turn.turn)),
        Some("helper"),
        None,
    )
    .await;
    f.end_turn(&turn).await;
    assert!(matches!(
        next(&f.control, "upgraded", &run).await.unwrap(),
        Next::Ready(_)
    ));
    let reply = f.reply_fact(&duty).await.0;
    assert_eq!(reply["state"], "pending");
    assert!(reply["reminded_at"].is_string());
    f.shutdown().await;
}

#[tokio::test]
async fn held_answer_or_outcome_across_stop_is_owned_by_the_run_that_admits_it() {
    // Held answer and untargeted outcome arrive while Stop is in progress; the
    // committed outcome arrives before Stop and is never admitted by its Run.
    for case in ["held_answer", "outcome_during_stop", "outcome_before_stop"] {
        let outcome = case != "held_answer";
        let f = fixture(&["worker", "helper"]).await;
        let (duty, first_run) = f.ask(None, Some("worker"), None).await;
        let first = f.admit("worker", &first_run).await;
        f.adopt(&first).await;
        let (question, helper_run) = f
            .ask(
                Some(origin("worker", &first_run, &first.turn)),
                Some("helper"),
                None,
            )
            .await;
        f.end_turn(&first).await;
        assert!(matches!(
            next(&f.control, "worker", &first_run).await.unwrap(),
            Next::Done
        ));
        assert_eq!(f.reply_fact(&duty).await.0["state"], "awaiting_own_ask");

        let stopping = f.send("worker").await;
        if case != "outcome_before_stop" {
            f.begin_stop(&stopping).await;
        }
        if outcome {
            f.execute(
                Command::MessageCancel(MessageCancelArgs {
                    message_ids: vec![question.clone()],
                }),
                None,
            )
            .await;
        } else {
            let helper = f.admit("helper", &helper_run).await;
            f.adopt(&helper).await;
            f.reply(Some(origin("helper", &helper_run, &helper.turn)), &question)
                .await;
        }
        let reply = f.reply_fact(&duty).await.0;
        assert_eq!(reply["state"], "pending");
        assert!(reply.get("owner_run_id").is_none());
        if case == "outcome_before_stop" {
            // The live Run was woken for the notice, but Stop wins before it
            // prepares a Turn.
            f.begin_stop(&stopping).await;
        }

        // Stop and every sweep leave the reopened duty open.
        f.finish_stop(&stopping, "worker").await;
        assert_eq!(f.run_state(&stopping).await, "stopped");
        f.sweep_old_runs(&stopping).await;
        f.sweep_old_runs(&first_run).await;
        let (reply, delivered) = f.reply_fact(&duty).await;
        assert_eq!(reply["state"], "pending");
        assert!(reply.get("owner_run_id").is_none());
        assert_eq!(delivered.as_deref(), Some(first_run.as_str()));

        // The next Run admits the held answer or notice and owes the duty.
        let later = f.send("worker").await;
        let handling = f.admit("worker", &later).await;
        if outcome {
            assert!(
                handling
                    .events
                    .iter()
                    .any(|event| *event == format!("reply-outcome-{question}"))
            );
        }
        assert_eq!(f.reply_fact(&duty).await.0["owner_run_id"], later.as_str());
        f.adopt(&handling).await;
        f.end_turn(&handling).await;
        assert!(matches!(
            next(&f.control, "worker", &later).await.unwrap(),
            Next::Done
        ));
        assert_eq!(f.reply_fact(&duty).await.0["state"], "ignored");
        f.shutdown().await;
    }
}
