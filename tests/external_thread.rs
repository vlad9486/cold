use core::{
    time::Duration,
    future::Future,
    task::{Context, Poll},
    sync::atomic::{AtomicBool, Ordering},
};
use std::{sync::Arc, io, thread};
use cold::rt::Executor;

#[test]
fn other_thread() -> io::Result<()> {
    struct S(Arc<AtomicBool>);

    impl Future for S {
        type Output = ();

        fn poll(self: std::pin::Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
            if self.0.load(Ordering::SeqCst) {
                Poll::Ready(())
            } else {
                let val = self.0.clone();
                let _ = cx.waker().clone();
                let _ = cx.waker().clone();
                let waker = cx.waker().clone();

                thread::spawn(move || {
                    thread::sleep(Duration::from_millis(500));
                    val.store(true, Ordering::SeqCst);
                    waker.wake_by_ref();
                    waker.wake();
                });

                Poll::Pending
            }
        }
    }

    let s = S(Arc::new(AtomicBool::new(false)));
    Executor::<usize>::run(0, move |_, _| async { async { s.await }.await })
}
