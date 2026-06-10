// nib-core 自持的 tokio runtime(RFC v2 §3 / critic V2):
// 两个宿主(过渡期 src-tauri、原生 nib-app)的 core 工作都必须跑在这一个 runtime 上,
// 保证过渡期在旧 app 上日用验证到的并发拓扑与切换后完全一致。
// 宿主侧规则:不要用宿主自己的 runtime 直接 poll core 的重活——经 run()/on_worker() 进来。

use std::future::Future;
use std::sync::OnceLock;

static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();

fn runtime() -> &'static tokio::runtime::Runtime {
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .thread_name("nib-core")
            .enable_all()
            .build()
            .expect("failed to build nib-core runtime")
    })
}

/// 把 async 工作提交到 core runtime 执行。返回的 JoinHandle 是 runtime 无关的
/// future,宿主在自己的运行时里 await 它即可。
pub async fn run<F>(fut: F) -> F::Output
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    runtime()
        .spawn(fut)
        .await
        .expect("nib-core task panicked")
}

/// fire-and-forget:把后台任务交给 core runtime,不关心结果(如会话落盘)
pub fn detach<F>(fut: F)
where
    F: Future<Output = ()> + Send + 'static,
{
    runtime().spawn(fut);
}

/// spawn_blocking 的 core-runtime 版。core 内禁止裸用 tokio::task::spawn_blocking——
/// 它抓「环境 runtime」,从非 tokio 线程(如 gpui 主线程)调用会直接 panic。
pub(crate) fn spawn_blocking<T: Send + 'static>(
    f: impl FnOnce() -> T + Send + 'static,
) -> tokio::task::JoinHandle<T> {
    runtime().spawn_blocking(f)
}

// 同步 GUI/Tauri 主线程执行阻塞 IO 会冻住整个 UI——所有磁盘/子进程等待
// 统一经这里搬到 core runtime 的阻塞线程池(原 src-tauri commands::on_worker)。
pub(crate) async fn on_worker<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    runtime()
        .spawn_blocking(f)
        .await
        .map_err(|e| format!("Task failed: {}", e))?
}
