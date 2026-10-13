use std::sync::LazyLock;

use tokio::runtime::{Builder, Runtime};

pub(crate) static RUNTIME: LazyLock<Runtime> = LazyLock::new(|| {
    Builder::new_multi_thread()
        .enable_all()
        .thread_name("lyrune-worker")
        .build()
        .expect("create Lyrune Tokio runtime")
});
