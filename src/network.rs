use std::{future::Future, sync::OnceLock};
use tokio::runtime::{Builder, Runtime};

const NETWORK_THREADS: usize = 2;

fn runtime() -> &'static Runtime {
    static RUNTIME: OnceLock<Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        Builder::new_multi_thread()
            .worker_threads(NETWORK_THREADS)
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
