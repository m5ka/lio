//! Bridge between the GTK main loop and a small Tokio runtime.
//!
//! GObjects stay on the main thread; network, database and file I/O run on Tokio.
//! UI code awaits Tokio work from `glib::spawn_future_local` via [`run`].

use std::future::Future;
use std::sync::OnceLock;

use tokio::runtime::Runtime;
use tokio::task::JoinHandle;

/// When the process started (for the startup-time log).
pub fn process_start() -> std::time::Instant {
    static START: OnceLock<std::time::Instant> = OnceLock::new();
    *START.get_or_init(std::time::Instant::now)
}

pub fn runtime() -> &'static Runtime {
    static RUNTIME: OnceLock<Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .max_blocking_threads(8)
            .thread_name("lio-worker")
            .enable_all()
            .build()
            .expect("failed to start the Tokio runtime")
    })
}

/// Spawns `future` on the Tokio runtime.
pub fn spawn<F>(future: F) -> JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    runtime().spawn(future)
}

/// Runs `future` on Tokio and awaits its result from any executor (e.g. the GLib main context).
pub async fn run<F, T>(future: F) -> T
where
    F: Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    match runtime().spawn(future).await {
        Ok(value) => value,
        Err(e) if e.is_panic() => std::panic::resume_unwind(e.into_panic()),
        Err(e) => panic!("Tokio task failed: {e}"),
    }
}
