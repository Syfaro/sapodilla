mod app;
mod protocol;
mod transports;
mod views;

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
