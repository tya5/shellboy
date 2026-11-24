use super::*;
use std::{
    future::Future,
    sync::{Mutex, mpsc},
    task::{Context, Wake, Waker},
};

#[test]
fn release_on_either_side_of_close_leaves_zero_or_wakes_the_drain() {
    let stop = CancellationToken::new();
    for close_first in [false, true] {
        let changed = Arc::new(Notify::new());
        let admission = RequestAdmission::new(changed.clone());
        let guard = admission.admit(&stop, false).unwrap();
        let mut wake = std::pin::pin!(changed.notified());
        assert!(!wake.as_mut().enable());
        if close_first {
            admission.close();
            assert!(!admission.is_idle());
        }
        drop(guard);
        assert_eq!(wake.as_mut().enable(), close_first);
        if !close_first {
            admission.close();
        }
        assert!(admission.is_idle());
    }
}

#[test]
fn second_check_refuses_close_between_checks_and_releases_both_counts() {
    let admission = RequestAdmission::new(Arc::new(Notify::new()));
    let mut checks = 0;
    let result = admission.admit_checking(true, || {
        checks += 1;
        if checks == 2 {
            assert_eq!(admission.active_count(), 1);
            assert_eq!(admission.context_count(), 1);
            admission.close();
        }
        false
    });
    assert_eq!(checks, 2);
    assert!(matches!(result, Err(error) if error.code == ErrorCode::InvalidState));
    assert!(admission.is_idle());
    assert_eq!(admission.context_count(), 0);
}

#[test]
fn context_drain_registers_and_guard_decrements_both_counts_before_waking() {
    enum Event {
        Wake,
        Dropped,
    }
    struct Probe {
        event: mpsc::Sender<Event>,
        resume: Mutex<mpsc::Receiver<()>>,
    }
    impl Wake for Probe {
        fn wake(self: Arc<Self>) {
            self.event.send(Event::Wake).unwrap();
            self.resume.lock().unwrap().recv().unwrap();
        }
    }

    let admission = RequestAdmission::new(Arc::new(Notify::new()));
    let guard = admission.admit(&CancellationToken::new(), true).unwrap();
    admission.close();
    let (event, events) = mpsc::channel();
    let (resume, paused) = mpsc::channel();
    let waker = Waker::from(Arc::new(Probe {
        event: event.clone(),
        resume: Mutex::new(paused),
    }));
    let mut drain = std::pin::pin!(admission.wait_context_zero());
    assert!(
        drain
            .as_mut()
            .poll(&mut Context::from_waker(&waker))
            .is_pending()
    );
    std::thread::scope(|scope| {
        scope.spawn(move || {
            drop(guard);
            event.send(Event::Dropped).unwrap();
        });
        // The wake callback pauses the dropping thread, so these reads cannot
        // mistake a subsequent decrement for one that preceded the wake.
        let first = events.recv().unwrap();
        let counts = (admission.active_count(), admission.context_count());
        if matches!(first, Event::Wake) {
            resume.send(()).unwrap();
        }
        assert!(matches!(first, Event::Wake));
        assert_eq!(counts, (0, 0));
    });
    assert!(
        drain
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
            .is_ready()
    );
}
