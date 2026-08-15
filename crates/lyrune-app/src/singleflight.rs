use std::collections::HashMap;
use std::future::Future;
use std::hash::Hash;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use anyhow::Result;
use futures_util::future::{BoxFuture, FutureExt as _, Shared};
use tokio::sync::Mutex;

type SharedTask<V> = Shared<BoxFuture<'static, Result<V, Arc<str>>>>;

struct InFlight<V>
where
    V: Clone,
{
    id: u64,
    task: SharedTask<V>,
}

pub struct SingleFlight<K, V>
where
    V: Clone,
{
    in_flight: Arc<Mutex<HashMap<K, InFlight<V>>>>,
    next_id: Arc<AtomicU64>,
}

impl<K, V> Clone for SingleFlight<K, V>
where
    V: Clone,
{
    fn clone(&self) -> Self {
        Self {
            in_flight: self.in_flight.clone(),
            next_id: self.next_id.clone(),
        }
    }
}

impl<K, V> Default for SingleFlight<K, V>
where
    V: Clone,
{
    fn default() -> Self {
        Self {
            in_flight: Arc::new(Mutex::new(HashMap::new())),
            next_id: Arc::new(AtomicU64::new(0)),
        }
    }
}

impl<K, V> SingleFlight<K, V>
where
    K: Clone + Eq + Hash,
    V: Clone,
{
    pub async fn run<F, Fut>(&self, key: K, force: bool, operation: F) -> Result<V>
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = Result<V>> + Send + 'static,
    {
        let (id, task) = {
            let mut in_flight = self.in_flight.lock().await;
            if !force && let Some(request) = in_flight.get(&key) {
                (request.id, request.task.clone())
            } else {
                let id = self.next_id.fetch_add(1, Ordering::Relaxed);
                let task = async move {
                    operation()
                        .await
                        .map_err(|error| Arc::<str>::from(format!("{error:#}")))
                }
                .boxed()
                .shared();
                in_flight.insert(
                    key.clone(),
                    InFlight {
                        id,
                        task: task.clone(),
                    },
                );
                (id, task)
            }
        };

        let result = task
            .await
            .map_err(|error| anyhow::anyhow!(error.to_string()));
        let mut in_flight = self.in_flight.lock().await;
        if in_flight.get(&key).is_some_and(|request| request.id == id) {
            in_flight.remove(&key);
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use tokio::sync::Notify;

    use super::*;

    #[tokio::test]
    async fn shares_matching_requests() {
        let requests = Arc::new(AtomicUsize::new(0));
        let singleflight = SingleFlight::<u8, u8>::default();
        let first_requests = requests.clone();
        let second_requests = requests.clone();

        let (first, second) = tokio::join!(
            singleflight.run(1, false, move || async move {
                first_requests.fetch_add(1, Ordering::SeqCst);
                tokio::task::yield_now().await;
                Ok(7)
            }),
            singleflight.run(1, false, move || async move {
                second_requests.fetch_add(1, Ordering::SeqCst);
                Ok(8)
            })
        );

        assert_eq!(first.expect("first result"), 7);
        assert_eq!(second.expect("shared result"), 7);
        assert_eq!(requests.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn completed_request_does_not_remove_forced_replacement() {
        let requests = Arc::new(AtomicUsize::new(0));
        let first_release = Arc::new(Notify::new());
        let second_release = Arc::new(Notify::new());
        let (started_sender, started_receiver) = async_channel::unbounded();
        let singleflight = SingleFlight::<u8, u8>::default();

        let first = {
            let singleflight = singleflight.clone();
            let requests = requests.clone();
            let release = first_release.clone();
            let started = started_sender.clone();
            tokio::spawn(async move {
                singleflight
                    .run(1, false, move || async move {
                        requests.fetch_add(1, Ordering::SeqCst);
                        started.send(1).await.expect("report first start");
                        release.notified().await;
                        Ok(1)
                    })
                    .await
            })
        };
        assert_eq!(started_receiver.recv().await.expect("first start"), 1);

        let forced = {
            let singleflight = singleflight.clone();
            let requests = requests.clone();
            let release = second_release.clone();
            let started = started_sender.clone();
            tokio::spawn(async move {
                singleflight
                    .run(1, true, move || async move {
                        requests.fetch_add(1, Ordering::SeqCst);
                        started.send(2).await.expect("report forced start");
                        release.notified().await;
                        Ok(2)
                    })
                    .await
            })
        };
        assert_eq!(started_receiver.recv().await.expect("forced start"), 2);

        first_release.notify_one();
        assert_eq!(first.await.expect("join first").expect("first result"), 1);

        let attached = {
            let singleflight = singleflight.clone();
            let requests = requests.clone();
            tokio::spawn(async move {
                singleflight
                    .run(1, false, move || async move {
                        requests.fetch_add(1, Ordering::SeqCst);
                        Ok(3)
                    })
                    .await
            })
        };
        tokio::task::yield_now().await;
        assert_eq!(requests.load(Ordering::SeqCst), 2);

        second_release.notify_one();
        assert_eq!(
            forced.await.expect("join forced").expect("forced result"),
            2
        );
        assert_eq!(
            attached
                .await
                .expect("join attached")
                .expect("attached result"),
            2
        );
        assert_eq!(requests.load(Ordering::SeqCst), 2);
    }
}
