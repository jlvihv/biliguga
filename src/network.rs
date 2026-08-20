use futures::channel::oneshot;
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

pub(crate) async fn run<F, T>(future: F) -> T
where
    F: Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    let (sender, receiver) = oneshot::channel();
    detach(async move {
        let _ = sender.send(future.await);
    });
    receiver
        .await
        .expect("biliguga network task stopped unexpectedly")
}
