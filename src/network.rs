use std::{future::Future, sync::OnceLock};
use tokio::runtime::{Builder, Runtime};

const NETWORK_THREADS: usize = 2;

/// How many blocking threads the runtime may keep. The only thing that uses them is name
/// resolution (`getaddrinfo` is a blocking call), which is a handful of lookups per feed: a
/// browser-sized pool of them would be memory spent on threads that never run. Tokio's default
/// is 512 spares, and this runtime used to sit with seven of them parked.
const BLOCKING_THREADS: usize = 2;

fn runtime() -> &'static Runtime {
    static RUNTIME: OnceLock<Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        Builder::new_multi_thread()
            .worker_threads(NETWORK_THREADS)
            .max_blocking_threads(BLOCKING_THREADS)
            .thread_name("biliguga-net")
            .enable_all()
            .build()
            .expect("failed to create biliguga network runtime")
    })
}

pub(crate) fn detach<F>(future: F)
where
    F: Future<Output = ()> + Send + 'static,
{
    runtime().spawn(future);
}
