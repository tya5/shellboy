//! Pure building blocks for the reviewed b822 single-request design.
//! Not yet connected to dispatch/adoption: no #A writer or settings storage.
use super::*;

pub(super) const DEFAULT_OUTPUT_CHARACTERS: u64 = 20_000;

/// Already-resolved policy. Validation/storage remain at the settings boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct OutputLimit(pub(super) u64);
impl OutputLimit {
    pub(super) fn resolve(thread: Option<Self>, connection: Option<Self>) -> Self {
        thread.or(connection).unwrap_or(Self(DEFAULT_OUTPUT_CHARACTERS))
    }
}

pub(super) fn request(snapshot: &Snapshot, selected: Range<usize>, limit: OutputLimit) -> Value {
    let text = snapshot.groups[selected].iter()
        .flat_map(|group| group.sources.iter())
        .chain(snapshot.extra_sources.iter())
        .map(|source| render(source, snapshot.epoch))
        .collect::<Vec<_>>().join("\n\n");
    model::response_request(&snapshot.model, vec![
        json!({"role":"developer","content":[{"type":"input_text","text":format!("{SUMMARY_PROMPT}{KEEP_INSTRUCTIONS} Maximum output length: {} characters.",limit.0)}]}),
        json!({"role":"user","content":[{"type":"input_text","text":text}]}),
    ], snapshot.effort.as_deref(), false)
}

/// Overflow final-summary range: path start, excluding latest and first unsafe.
/// Active selection has different rules and must not reuse this boundary.
pub(super) fn overflow_prefix(snapshot: &Snapshot) -> Range<usize> {
    let stop = snapshot.groups.len().saturating_sub(1);
    let end = (0..stop).find(|&i| !range::group_is_safe(snapshot, i)).unwrap_or(stop);
    0..end
}

/// USER-confirmed: measure saved selected-group projection characters only;
/// fixed instructions and extra_sources never participate in halving weights.
/// Only a classified context refusal calls this. Transient retry stays with
/// the existing HTTP request retry mechanism, never this range transformation.
pub(super) fn half_prefix(snapshot: &Snapshot, selected: Range<usize>) -> Option<Range<usize>> {
    if selected.len() <= 1 { return None; }
    let weights: Vec<u128> = snapshot.groups[selected.clone()].iter().map(|group| {
        group.sources.iter().map(|source| edit::projection_characters(&source.payload,snapshot.epoch) as u128).sum()
    }).collect();
    let half = weights.iter().sum::<u128>() / 2;
    let mut total = 0;
    let mut count = 0;
    // USER-confirmed: remove at least one trailing group even for zero weight.
    for weight in weights.iter().take(weights.len()-1) {
        if total + weight > half { break; }
        total += weight;
        count += 1;
    }
    Some(selected.start..selected.start + count.max(1))
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Next {
    Done,
    RetryPrefix(Range<usize>),
    Failed,
}

pub(super) fn after_result(
    snapshot: &Snapshot,
    selected: Range<usize>,
    result: Result<(), &model::ModelError>,
    eligible: impl FnOnce(Range<usize>) -> bool,
) -> Next {
    match result {
        Ok(()) => Next::Done, // No remainder scheduling in this trial.
        Err(error) if error.is_context_limit() => {
            match half_prefix(snapshot, selected).filter(|prefix| eligible(prefix.clone())) {
                Some(prefix) => Next::RetryPrefix(prefix),
                None => Next::Failed,
            }
        }
        Err(_) => Next::Failed, // Transport retries occur before this final result.
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct AdoptionSize {
    pub(super) exceeded_output_limit: bool,
}
/// Output cap is a request instruction, not a resend/quality gate.
pub(super) fn adoption_size(original: u64, summary: u64, limit: OutputLimit) -> Option<AdoptionSize> {
    (summary < original).then_some(AdoptionSize { exceeded_output_limit: summary > limit.0 })
}

/// Real HTTP execution path for the new summarizer. Entry-point/adoption
/// integration is separate: caller must snapshot/revalidate the selected range.
pub(super) struct Dispatched {
    pub(super) selected: Range<usize>,
    pub(super) response: model::ModelResponse,
    pub(super) size: AdoptionSize,
}
impl Compactor {
    pub(super) async fn summarize_single_closed(
        &self, snapshot:Snapshot, original:Range<usize>, cancel:CancellationToken,
        ledger:&mut usage::UsageLedger, boundary:Option<&mut boundary::Boundary<'_>>,
        automatic:Option<AutomaticStage>, drop_all_reasoning:bool,
    ) -> Result<CompactResult,ApiError> {
        range::validate(&snapshot,std::slice::from_ref(&original),range::RangeMode::Summary)?;
        let settings_thread=snapshot.thread.clone();
        let max_chars=self.database.call(move |db|Ok(db.query_row("SELECT summary_max_chars FROM thread WHERE id=?1",[settings_thread],|r|r.get::<_,Option<u64>>(0))?)).await.map_err(storage_error)?;
        let limit=OutputLimit(crate::config::summary_chars_for(max_chars,self.config.connections.get(&snapshot.profile)));
        let identity=failure_identity(&snapshot.thread,&snapshot,original.clone(),limit)
            .ok_or_else(||invalid("No summary range"))?;
        let thread=snapshot.thread.clone();let lookup=identity.clone();
        let blocked=self.database.call(move |db|same_failed_range(db,&thread,Region::Closed,&lookup)).await.map_err(storage_error)?;
        if blocked {return Err(invalid("This closed summary range already failed deterministically"));}
        let dispatched=self.dispatch_single_summary(&snapshot,original,limit,&cancel,ledger,boundary,|prefix| {
            range::validate(&snapshot,std::slice::from_ref(&prefix),range::RangeMode::Summary).is_ok()
                && snapshot.groups[prefix].iter().any(Group::eligible)
        }).await;
        let dispatched=match dispatched {
            Ok(result)=>result,
            Err(error)=>{
                let reason=error.details.as_ref().and_then(|v|v["compaction_discard_reason"].as_str());
                let deterministic=matches!(reason,Some("summary_refused"|"summary_empty"|"summary_not_shrinking"))
                    || error.details.as_ref().is_some_and(|v|v["single_summary_context_terminal"]==true);
                if deterministic {
                    let thread=snapshot.thread.clone();let proof=snapshot.clone();
                    self.database.call(move |db| {let tx=db.transaction()?;validate(&tx,&proof)?;remember_failure(&tx,&thread,Region::Closed,&identity)?;tx.commit()?;Ok(())}).await.map_err(storage_error)?;
                }
                return Err(error);
            }
        };
        let selected=dispatched.selected;
        let sources:Vec<Source>=snapshot.groups[selected.clone()].iter().flat_map(|g|g.sources.clone()).chain(snapshot.extra_sources.clone()).collect();
        let originals:Vec<String>=sources.iter().map(|s|s.id.clone()).collect();
        let text_chars=dispatched.response.text.chars().count();
        let source_chars=sources.iter().map(|s|edit::projection_characters(&s.payload,snapshot.epoch)).sum::<usize>();
        let payload=json!({"item":{"role":"developer","content":[{"type":"input_text","text":dispatched.response.text}]},"source_node_ids":originals,"summary_format":FORMAT,"summary_usage":ledger.usage(),"summary_output_limit_exceeded":dispatched.size.exceeded_output_limit});
        let projected=edit::projection_characters(&payload,snapshot.epoch);
        if projected>=source_chars {
            let thread=snapshot.thread.clone();let proof=snapshot.clone();
            self.database.call(move |db| {let tx=db.transaction()?;validate(&tx,&proof)?;remember_failure(&tx,&thread,Region::Closed,&identity)?;tx.commit()?;Ok(())}).await.map_err(storage_error)?;
            return Err(discard::rejected(discard::DiscardReason::SummaryNotShrinking,"Summary does not shrink its original source projection; original path retained"));
        }
        let adoption=SummaryAdoption {
            automatic,removed_characters:(source_chars-projected) as u64,drop_all_reasoning,snapshot,
            sources,payload,start:selected.start,count:selected.end,
            result:CompactResult {source_characters:Some(source_chars as u64),summary_characters:Some(text_chars as u64),summary_limit:Some(limit.0),model_requests:Some(ledger.usage().requests as usize),source_nodes:originals.len(),original_node_ids:originals,summary_usage:Some(ledger.usage().clone()),..Default::default()}
        };
        let config=self.config.clone();
        self.database.call(move |db| {let tx=db.transaction()?;let result=adoption.adopt_tx(&tx,&config,&cancel)?;tx.commit()?;Ok(result)}).await.map_err(storage_error)
    }

    pub(super) async fn dispatch_single_summary(
        &self, snapshot:&Snapshot, selected:Range<usize>, limit:OutputLimit,
        cancel:&CancellationToken, ledger:&mut usage::UsageLedger,
        mut boundary:Option<&mut boundary::Boundary<'_>>, eligible:impl Fn(Range<usize>)->bool,
    ) -> Result<Dispatched,ApiError> {
        let connection=self.config.connections.get(&snapshot.profile)
            .ok_or_else(||ApiError::new(ErrorCode::ConfigInvalid,"Connection not found"))?;
        let client=self.clients.get(&snapshot.profile).map_err(|e|invalid(&e.to_string()))?;
        let mut trial=Trial::new(&snapshot.thread,snapshot,selected,limit)
            .ok_or_else(||invalid("No summary range"))?;
        let mut retries=0u32;
        loop {
            if cancel.is_cancelled() {return Err(discard::rejected(discard::DiscardReason::Cancelled,"Compaction cancelled; original path retained"));}
            let selected=trial.selected().ok_or_else(||invalid("Summary trial already ended"))?;
            let payload=request(snapshot,selected.clone(),limit);
            // Preserve existing transport admission and durable usage accounting.
            // Legacy usage counters remain until public schema retirement step.
            let admission=self.summary_admission(&client,snapshot,cancel,boundary.as_deref_mut()).await?;
            let observation=ledger.begin(&self.database,false).await?;
            let result=Self::summary_response(&client,&snapshot.thread,payload,cancel.clone(),admission).await;
            let response=match result {
                Ok(response)=>{
                    ledger.observe_http(&self.database,observation,response.response_facts).await?;
                    ledger.observe(&self.database,usage::usage_of(Some(&response.raw))).await?;
                    response
                }
                Err(failure)=>{
                    ledger.observe_http(&self.database,observation,failure.facts).await?;
                    ledger.observe(&self.database,usage::usage_of(failure.raw_response.as_ref())).await?;
                    if cancel.is_cancelled() {return Err(discard::rejected(discard::DiscardReason::Cancelled,"Compaction cancelled; original path retained"));}
                    if !failure.error.is_context_limit() {
                        if let Some(delay)=retry::retry_delay(connection,&failure,retries) {
                            retries+=1;
                            self.summary_backoff(snapshot,delay,cancel,boundary.as_deref_mut()).await?;
                            continue;
                        }
                        return Err(invalid("Summary request failed; original path retained"));
                    }
                    match trial.finish_request(snapshot,Err(&failure.error),&eligible) {
                        Next::RetryPrefix(_)=>{retries=0;continue;}
                        Next::Done|Next::Failed=>return Err(invalid("Summary context refusal at final eligible prefix; original path retained").details(json!({"single_summary_context_terminal":true}))),
                    }
                }
            };
            if summary_refused(&response.output) {return Err(discard::rejected(discard::DiscardReason::SummaryRefused,"Summary was refused; original path retained"));}
            if response.text.trim().is_empty() {return Err(discard::rejected(discard::DiscardReason::SummaryEmpty,"Summary was empty; original path retained"));}
            // The adoption caller checks the final saved projection, including
            // fixed sources and metadata, against the actual replaced sources.
            // Raw response text length is not a second, different shrink gate.
            let size=AdoptionSize { exceeded_output_limit:response.text.chars().count() as u64>limit.0 };
            trial.finish_request(snapshot,Ok(()),&eligible);
            return Ok(Dispatched{selected,response,size});
        }
    }
}

/// Claude recommendation adopted by leader: two latest slots, not a history.
#[derive(Clone, Copy)]
pub(super) enum Region { Closed, Active }
impl Region {
    fn key(self, thread: &str) -> String {
        let kind=match self {Self::Closed=>"closed",Self::Active=>"active"};
        format!("compact.deterministic_range_failure.{kind}.{thread}")
    }
}
/// Capture ONCE before halving: original endpoints and request settings.
#[derive(Clone)]
pub(super) struct FailureIdentity(String);
pub(super) fn failure_identity(thread: &str, snapshot: &Snapshot, original: Range<usize>, limit: OutputLimit) -> Option<FailureIdentity> {
    let groups=snapshot.groups.get(original)?;
    let first=groups.iter().flat_map(|g|g.sources.iter()).next()?;
    let last=groups.iter().rev().flat_map(|g|g.sources.iter().rev()).next()?;
    Some(FailureIdentity(digest(&serde_json::to_vec(&json!([
        thread,first.id,last.id,snapshot.profile,snapshot.model,limit.0
    ])).expect("identity serializes"))))
}

/// Pure execution cursor: original failure identity cannot drift while halving.
/// Dispatch, source adoption and #A transactions remain external/unconnected.
pub(super) struct Trial {
    pub(super) identity: FailureIdentity,
    selected: Range<usize>,
    terminal: bool,
}
impl Trial {
    pub(super) fn new(thread: &str, snapshot: &Snapshot, original: Range<usize>, limit: OutputLimit) -> Option<Self> {
        Some(Self { identity: failure_identity(thread,snapshot,original.clone(),limit)?, selected: original, terminal:false })
    }
    pub(super) fn selected(&self) -> Option<Range<usize>> {
        (!self.terminal).then(||self.selected.clone())
    }
    pub(super) fn finish_request(&mut self, snapshot: &Snapshot, result: Result<(),&model::ModelError>, eligible:impl FnOnce(Range<usize>)->bool) -> Next {
        if self.terminal { return Next::Done; }
        let next=after_result(snapshot,self.selected.clone(),result,eligible);
        match &next {
            Next::RetryPrefix(prefix)=> self.selected=prefix.clone(),
            Next::Done|Next::Failed=>self.terminal=true,
        }
        next
    }
}

pub(super) fn remember_failure(tx: &rusqlite::Transaction<'_>, thread: &str, region: Region, identity: &FailureIdentity) -> Result<(), StorageError> {
    tx.execute("INSERT INTO app_meta(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        params![region.key(thread),identity.0])?;
    Ok(())
}
pub(super) fn same_failed_range(db: &rusqlite::Connection, thread: &str, region: Region, identity: &FailureIdentity) -> Result<bool, StorageError> {
    let saved:Option<String>=db.query_row("SELECT value FROM app_meta WHERE key=?1",
        [region.key(thread)],|r|r.get(0)).optional()?;
    Ok(saved.as_deref()==Some(identity.0.as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture(lengths: &[usize]) -> Snapshot {
        super::super::tests::fixture_snapshot(lengths.iter().enumerate().map(|(i,&n)| Group {
            sources: vec![super::super::tests::source(&format!("source{i}"),None,json!({"type":"message","role":"user","content":"x".repeat(n)}))],
            complete: true, tool: false,
        }).collect())
    }
    #[test]
    fn one_request_contains_whole_selected_range_and_output_instruction() {
        let snapshot=fixture(&[40,60,80]);
        let body=request(&snapshot,0..2,OutputLimit::resolve(None,None));
        let text=body.to_string();
        assert!(text.contains("source0") && text.contains("source1"));
        assert!(!text.contains("source2"));
        assert!(text.contains("20000") && text.contains("User instructions to keep"));
        assert_eq!(OutputLimit::resolve(Some(OutputLimit(37)),Some(OutputLimit(80))),OutputLimit(37));
        assert_eq!(OutputLimit::resolve(None,Some(OutputLimit(80))),OutputLimit(80));
    }
    #[test]
    fn half_is_whole_prefix_strictly_smaller_and_success_never_schedules_remainder() {
        let snapshot=fixture(&[40,40,40,40]);
        let half=half_prefix(&snapshot,0..4).unwrap();
        assert_eq!(half,0..2);
        assert_eq!(after_result(&snapshot,half,Ok(()),|_|panic!("success must not inspect another range")),Next::Done);
        assert_eq!(half_prefix(&snapshot,0..1),None);
        let mut selected=0..4;
        while let Some(next)=half_prefix(&snapshot,selected.clone()) {
            assert!(next.len()<selected.len());selected=next;
        }
        assert_eq!(selected.len(),1);
    }
    #[test]
    fn overflow_stops_before_unsafe_or_latest_and_never_splits_a_group() {
        let mut snapshot=fixture(&[10,10,10,10]);
        assert_eq!(overflow_prefix(&snapshot),0..3);
        snapshot.groups[1].complete=false;
        assert_eq!(overflow_prefix(&snapshot),0..1);
        snapshot.groups[1].complete=true;
        snapshot.protected.insert("source0".into());
        assert_eq!(overflow_prefix(&snapshot),0..0);
        assert_eq!(overflow_prefix(&fixture(&[10])),0..0);
    }
    #[test]
    fn oversize_but_shrinking_is_adoptable_without_resend() {
        assert_eq!(adoption_size(100000,25000,OutputLimit(20000)),Some(AdoptionSize{exceeded_output_limit:true}));
        assert!(adoption_size(25000,25000,OutputLimit(20000)).is_none());
        assert!(adoption_size(25000,25001,OutputLimit(20000)).is_none());
        assert_eq!(adoption_size(100,50,OutputLimit(20000)),Some(AdoptionSize{exceeded_output_limit:false}));
    }
    #[test]
    fn only_context_refusal_halves_and_ineligible_or_single_group_stops() {
        let snapshot=fixture(&[40,40,40,40]);
        let mut context=model::ModelError::new("provider","synthetic overflow");
        context.provider_code=Some("context_length_exceeded".into());
        assert!(context.is_context_limit());
        assert_eq!(after_result(&snapshot,0..4,Err(&context),|prefix|prefix== (0..2)),Next::RetryPrefix(0..2));
        assert_eq!(after_result(&snapshot,0..4,Err(&context),|_|false),Next::Failed);
        assert_eq!(after_result(&snapshot,0..1,Err(&context),|_|true),Next::Failed);
        for kind in ["network","http","provider"] {
            let error=model::ModelError::new(kind,"synthetic non-context failure");
            assert_eq!(after_result(&snapshot,0..4,Err(&error),|_|panic!("transient failure must not select a shorter prefix")),Next::Failed);
        }
    }

    #[test]
    fn zero_weights_and_fixed_context_still_terminate_at_one_group() {
        let mut snapshot=fixture(&[0,0,0,0]);
        // Empty projections are measured by the same existing replay API.
        for group in &mut snapshot.groups { for source in &mut group.sources {source.payload=Value::Null;} }
        assert_eq!(half_prefix(&snapshot,0..4),Some(0..3));
        assert_eq!(half_prefix(&snapshot,0..3),Some(0..2));
        assert_eq!(half_prefix(&snapshot,0..2),Some(0..1));
        assert_eq!(half_prefix(&snapshot,0..1),None);
        snapshot.groups[0].sources[0].payload=json!({"type":"message","role":"user","content":"actual"});
        let without=half_prefix(&snapshot,0..4);
        snapshot.extra_sources.push(super::super::tests::source("fixed",None,json!({"type":"message","role":"user","content":"x".repeat(100000)})));
        assert_eq!(half_prefix(&snapshot,0..4),without,"fixed context must not alter weights");
        let mut overflow=model::ModelError::new("provider","fixture");
        overflow.provider_code=Some("context_length_exceeded".into());
        assert_eq!(after_result(&snapshot,0..1,Err(&overflow),|_|true),Next::Failed);
    }
    #[test]
    fn original_failure_identity_two_slots_reopen_settings_and_overwrite() {
        let root=tempfile::tempdir().unwrap();let path=root.path().join("failure.sqlite");
        let snapshot=fixture(&[1,2,3]);let limit=OutputLimit(20000);
        let original=failure_identity("t",&snapshot,0..3,limit).unwrap();
        let smaller=failure_identity("t",&snapshot,0..1,limit).unwrap();
        assert_ne!(original.0,smaller.0);
        {
            let mut db=rusqlite::Connection::open(&path).unwrap();
            db.execute_batch("CREATE TABLE app_meta(key TEXT PRIMARY KEY,value TEXT NOT NULL);").unwrap();
            let tx=db.transaction().unwrap();
            // A failed reduced request records the captured ORIGINAL identity.
            remember_failure(&tx,"t",Region::Closed,&original).unwrap();
            remember_failure(&tx,"t",Region::Active,&smaller).unwrap();
            tx.commit().unwrap();
        }
        let mut db=rusqlite::Connection::open(path).unwrap();
        assert!(same_failed_range(&db,"t",Region::Closed,&original).unwrap());
        assert!(same_failed_range(&db,"t",Region::Active,&smaller).unwrap());
        assert!(!same_failed_range(&db,"t",Region::Closed,&smaller).unwrap());
        assert!(!same_failed_range(&db,"other",Region::Closed,&original).unwrap());
        let changed_limit=failure_identity("t",&snapshot,0..3,OutputLimit(10000)).unwrap();
        assert!(!same_failed_range(&db,"t",Region::Closed,&changed_limit).unwrap());
        let mut changed=snapshot.clone();changed.model="different-model".into();
        assert!(!same_failed_range(&db,"t",Region::Closed,&failure_identity("t",&changed,0..3,limit).unwrap()).unwrap());
        changed=snapshot.clone();changed.profile="different-connection".into();
        assert!(!same_failed_range(&db,"t",Region::Closed,&failure_identity("t",&changed,0..3,limit).unwrap()).unwrap());
        assert_eq!(after_result(&snapshot,0..1,Ok(()),|_|false),Next::Done);
        assert!(same_failed_range(&db,"t",Region::Closed,&original).unwrap(),"success does not invent a failure or overwrite prior evidence");
        let tx=db.transaction().unwrap();remember_failure(&tx,"t",Region::Closed,&smaller).unwrap();tx.commit().unwrap();
        assert!(!same_failed_range(&db,"t",Region::Closed,&original).unwrap(),"same-kind overwrite deliberately has no permanent-history guarantee");
        assert!(same_failed_range(&db,"t",Region::Active,&smaller).unwrap());
        assert_eq!(db.query_row("SELECT count(*) FROM app_meta",[],|r|r.get::<_,i64>(0)).unwrap(),2);
    }
    #[test]
    fn trial_keeps_original_identity_across_half_then_success_or_failure() {
        let snapshot=fixture(&[40,40,40,40]);
        let mut trial=Trial::new("t",&snapshot,0..4,OutputLimit(20000)).unwrap();
        let original=trial.identity.0.clone();
        let mut error=model::ModelError::new("provider","synthetic context refusal");
        error.provider_code=Some("context_length_exceeded".into());
        assert_eq!(trial.finish_request(&snapshot,Err(&error),|_|true),Next::RetryPrefix(0..2));
        assert_eq!(trial.identity.0,original);
        assert_eq!(trial.selected(),Some(0..2));
        assert_eq!(trial.finish_request(&snapshot,Ok(()),|_|panic!("success is terminal")),Next::Done);
        assert_eq!(trial.selected(),None);
        assert_eq!(trial.finish_request(&snapshot,Err(&error),|_|panic!("no remainder retry")),Next::Done);
        let mut failed=Trial::new("t",&snapshot,0..4,OutputLimit(20000)).unwrap();
        while matches!(failed.finish_request(&snapshot,Err(&error),|_|true),Next::RetryPrefix(_)) {}
        assert_eq!(failed.selected(),None);
        assert_eq!(failed.identity.0,original,"terminal one-group failure belongs to original request range");
    }

}
