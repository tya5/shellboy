//! Finite task permit lifetimes. Refusal targets and validation stay at dispatch.
use std::{future::Future, sync::Arc};
use tokio::{
    sync::{OwnedSemaphorePermit, Semaphore},
    task::JoinSet,
};
use tokio_util::sync::CancellationToken;
pub(super) fn spawn_read<F>(
    workers: &mut JoinSet<()>,
    slots: &Arc<Semaphore>,
    cancel: &CancellationToken,
    future: F,
) -> bool
where
    F: Future<Output = ()> + Send + 'static,
{
    let Ok(permit) = slots.clone().try_acquire_owned() else {
        return false;
    };
    let cancel = cancel.clone();
    workers.spawn(async move {
        let _permit = permit;
        tokio::select! {biased;_=cancel.cancelled()=>{},_=future=>{}}
    });
    true
}

pub(super) fn spawn_work<F>(workers: &mut JoinSet<()>, permit: OwnedSemaphorePermit, future: F)
where
    F: Future<Output = ()> + Send + 'static,
{
    workers.spawn(async move {
        let _permit = permit;
        future.await;
    });
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn accepted_task_holds_its_slot_until_completion_or_abort() {
        let slots = Arc::new(Semaphore::new(1));
        let mut workers = JoinSet::new();
        let permit = slots.clone().try_acquire_owned().unwrap();
        spawn_work(&mut workers, permit, std::future::pending());
        assert!(slots.clone().try_acquire_owned().is_err());
        workers.abort_all();
        while workers.join_next().await.is_some() {}
        assert_eq!(slots.available_permits(), 1);
        let permit = slots.clone().try_acquire_owned().unwrap();
        spawn_work(&mut workers, permit, async {});
        workers.join_next().await.unwrap().unwrap();
        assert_eq!(slots.available_permits(), 1);
    }
}
