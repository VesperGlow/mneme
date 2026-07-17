//! 优雅停机：SIGTERM/Ctrl-C 信号广播 + 在途关键写入（历史落库、记忆、摘要、情绪）
//! 的计数追踪，让 main 在退出前把这些写入等完，避免容器 stop 时丢数据。

use std::future::Future;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use tokio::sync::{watch, Notify};

/// 停机信号广播端。克隆 [`Signal::subscribe`] 出来的接收端可在任意任务里等待。
pub struct Signal {
    tx: watch::Sender<bool>,
}

/// 停机信号接收端；`wait` 在信号触发后返回（已触发则立即返回）。
#[derive(Clone)]
pub struct Listener {
    rx: watch::Receiver<bool>,
}

impl Signal {
    /// 安装进程信号监听：Unix 下 SIGTERM + SIGINT，Windows 下 Ctrl-C。
    pub fn install() -> Self {
        let (tx, _) = watch::channel(false);
        let sender = tx.clone();
        tokio::spawn(async move {
            wait_for_process_signal().await;
            // send_replace 在暂无接收端时也会更新值，避免信号早于 subscribe 到达而丢失。
            sender.send_replace(true);
        });
        Self { tx }
    }

    pub fn subscribe(&self) -> Listener {
        Listener {
            rx: self.tx.subscribe(),
        }
    }
}

impl Listener {
    pub async fn wait(mut self) {
        // 发送端常驻 Signal；即使被丢弃，changed 返回 Err 也视为该停了。
        while !*self.rx.borrow() {
            if self.rx.changed().await.is_err() {
                return;
            }
        }
    }
}

async fn wait_for_process_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut sigterm = match signal(SignalKind::terminate()) {
            Ok(stream) => stream,
            Err(error) => {
                tracing::warn!("安装 SIGTERM 监听失败，仅响应 Ctrl-C：{error}");
                let _ = tokio::signal::ctrl_c().await;
                return;
            }
        };
        tokio::select! {
            _ = sigterm.recv() => tracing::info!("收到 SIGTERM"),
            _ = tokio::signal::ctrl_c() => tracing::info!("收到 SIGINT"),
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
        tracing::info!("收到 Ctrl-C");
    }
}

/// 在途关键写入计数器：spawn 出去的落库任务持有 guard，
/// 停机时 `wait_idle` 等计数归零再放行退出。
#[derive(Clone, Default)]
pub struct Pending {
    inner: Arc<PendingInner>,
}

#[derive(Default)]
struct PendingInner {
    count: AtomicUsize,
    idle: Notify,
}

impl Pending {
    pub fn guard(&self) -> PendingGuard {
        self.inner.count.fetch_add(1, Ordering::SeqCst);
        PendingGuard {
            inner: self.inner.clone(),
        }
    }

    /// 用 guard 包住一个后台任务再 spawn，任务结束（含 panic 时的栈展开）自动销账。
    pub fn spawn<F>(&self, task: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let guard = self.guard();
        tokio::spawn(async move {
            let _guard = guard;
            task.await;
        });
    }

    pub async fn wait_idle(&self) {
        loop {
            // 先登记再查计数，防止在查完与登记之间恰好归零而漏掉通知。
            let notified = self.inner.idle.notified();
            if self.inner.count.load(Ordering::SeqCst) == 0 {
                return;
            }
            notified.await;
        }
    }
}

pub struct PendingGuard {
    inner: Arc<PendingInner>,
}

impl Drop for PendingGuard {
    fn drop(&mut self) {
        if self.inner.count.fetch_sub(1, Ordering::SeqCst) == 1 {
            self.inner.idle.notify_waiters();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn wait_idle_returns_immediately_when_empty() {
        Pending::default().wait_idle().await;
    }

    #[tokio::test]
    async fn wait_idle_blocks_until_guards_dropped() {
        let pending = Pending::default();
        let guard = pending.guard();
        let waiter = {
            let pending = pending.clone();
            tokio::spawn(async move { pending.wait_idle().await })
        };
        tokio::task::yield_now().await;
        assert!(!waiter.is_finished());
        drop(guard);
        tokio::time::timeout(std::time::Duration::from_secs(1), waiter)
            .await
            .expect("wait_idle 应在 guard 释放后返回")
            .unwrap();
    }

    #[tokio::test]
    async fn spawn_releases_guard_after_task() {
        let pending = Pending::default();
        pending.spawn(async {});
        tokio::time::timeout(std::time::Duration::from_secs(1), pending.wait_idle())
            .await
            .expect("spawn 的任务结束后应归零");
    }
}
