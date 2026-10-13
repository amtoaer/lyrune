use std::sync::LazyLock;

use tokio::runtime::{Builder, Runtime};

// 网络和阻塞准备任务共享 Tokio runtime；涉及 GPUI 实体的结果由 cx.spawn 接回主线程。
pub(crate) static RUNTIME: LazyLock<Runtime> = LazyLock::new(|| {
    Builder::new_multi_thread()
        .enable_all()
        .thread_name("lyrune-worker")
        .build()
        .expect("create Lyrune Tokio runtime")
});
