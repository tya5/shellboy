//! Immutable refresh inputs and the existing finite read/progress admission.
use super::*;
pub(super) struct Query {
    pub thread: String,
    pub ids: Vec<String>,
    pub generation: Option<u64>,
    pub visible: bool,
    pub epoch: u64,
    pub render_width: u16,
}
pub(super) async fn run(
    query: Query,
    control: Arc<Control>,
    updates: SyncSender<UiUpdate>,
    sender: mpsc::Sender<BackgroundResult>,
    stop: CancellationToken,
    progress_slots: Arc<Semaphore>,
    permit: tokio::sync::OwnedSemaphorePermit,
) {
    let Query {
        thread,
        ids,
        generation,
        visible,
        epoch,
        render_width,
    } = query;
    let _permit = permit;
    tokio::select! {biased;_=stop.cancelled()=>{},_=async{
        let receipts=receipt_states(&control,ids).await;
        let mut running=None;
        let mut progress_permit=None;
        if visible && let Some(generation)=generation {
            let target=thread.clone();
            let metadata=control.store.database.call(move|db| {
                crate::control::db::thread_details(db,&target).map_err(crate::storage::StorageError::from)
            }).await.map_err(storage_error);
            match checked_storage(&control,metadata).await {
                Ok(details)=>update(&updates,UiUpdate::Thread{generation,details,draft:None}).await,
                Err(error)=>load_failed(&updates,thread.clone(),generation,error).await,
            }
            if let Err(error)=HistoryReader::new(&control,&updates,render_width).page(&Reply::new(&thread,generation),None,None).await{load_failed(&updates,thread.clone(),generation,error).await;}
            progress_permit=progress_slots.acquire_owned().await.ok();
            if progress_permit.is_some(){running=Some(activity::read(&control,&thread).await);}
        }
        let _=sender.send(BackgroundResult::Refreshed{thread_id:thread,receipts,progress:running,epoch,generation}).await;
        drop(progress_permit);
    }=>{}}
}
