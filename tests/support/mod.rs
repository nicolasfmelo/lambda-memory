use std::future::Future;
/// Immediately-ready mock executor; never used as a production async runtime.
pub fn ready_mock<F: Future>(future: F) -> F::Output {
    let mut context = std::task::Context::from_waker(std::task::Waker::noop());
    let mut future = std::pin::pin!(future);
    match future.as_mut().poll(&mut context) {
        std::task::Poll::Ready(value) => value,
        std::task::Poll::Pending => panic!("mock returned Pending; runtime required"),
    }
}
