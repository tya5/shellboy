// The caller awaits this only after its HTTP operation completes. No quiet window.
use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};
use tokio::{
    sync::oneshot,
    task::{JoinError, JoinHandle},
};
pub struct CountAfter {
    task: JoinHandle<usize>,
    finish: Option<oneshot::Sender<()>>,
}
impl CountAfter {
    pub fn new(task: JoinHandle<usize>, finish: oneshot::Sender<()>) -> Self {
        Self {
            task,
            finish: Some(finish),
        }
    }
    #[allow(dead_code)]
    pub fn abort(&self) {
        self.task.abort();
    }
}
impl Future for CountAfter {
    type Output = Result<usize, JoinError>;
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        if let Some(finish) = self.finish.take() {
            let _ = finish.send(());
        }
        Pin::new(&mut self.task).poll(cx)
    }
}
impl Drop for CountAfter {
    fn drop(&mut self) {
        self.task.abort();
    }
}
