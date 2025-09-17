mod app;
mod cut;
mod protocol;
mod transports;
mod views;

use futures::Stream;
#[cfg(not(target_arch = "wasm32"))]
use futures::StreamExt;
use std::time::Duration;

pub use app::SapodillaApp;

#[cfg(target_arch = "wasm32")]
type Rc<T> = std::rc::Rc<T>;
#[cfg(not(target_arch = "wasm32"))]
type Rc<T> = std::sync::Arc<T>;

#[cfg(target_arch = "wasm32")]
#[inline]
fn spawn<F>(future: F)
where
    F: Future<Output = ()> + 'static,
{
    wasm_bindgen_futures::spawn_local(future);
}

#[cfg(not(target_arch = "wasm32"))]
#[inline]
fn spawn<F>(future: F)
where
    F: Future<Output = ()> + Send + 'static,
{
    tokio::task::spawn(future);
}

/// Create a stream that resolves every given interval.
///
/// Will panic on WASM targets if `duration`'s milliseconds is greater than
/// `u32::MAX`.
fn interval(duration: Duration) -> impl Stream<Item = ()> {
    #[cfg(target_arch = "wasm32")]
    let s = gloo_timers::future::IntervalStream::new(u32::try_from(duration.as_millis()).unwrap());

    #[cfg(not(target_arch = "wasm32"))]
    let s =
        tokio_stream::wrappers::IntervalStream::new(tokio::time::interval(duration)).map(|_| ());

    s
}
