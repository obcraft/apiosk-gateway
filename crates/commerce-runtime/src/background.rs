//! The single v2 process owns every recurring commerce task.

use std::{future::Future, sync::Mutex};

#[derive(Default)]
pub(crate) struct BackgroundTasks {
    tasks: Mutex<Vec<tokio::task::JoinHandle<()>>>,
}

impl BackgroundTasks {
    pub fn spawn(&self, future: impl Future<Output = ()> + Send + 'static) {
        self.tasks
            .lock()
            .expect("background task registry")
            .push(tokio::spawn(future));
    }

    pub async fn stop(&self) {
        let tasks = std::mem::take(&mut *self.tasks.lock().expect("background task registry"));
        for task in &tasks {
            task.abort();
        }
        for task in tasks {
            let _ = task.await;
        }
    }
}

impl Drop for BackgroundTasks {
    fn drop(&mut self) {
        for task in self
            .tasks
            .get_mut()
            .expect("background task registry")
            .drain(..)
        {
            task.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };

    #[tokio::test]
    async fn dropping_the_runtime_cancels_its_loops() {
        struct Dropped(Arc<AtomicBool>);
        impl Drop for Dropped {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }
        let finished = Arc::new(AtomicBool::new(false));
        let task_guard = Dropped(finished.clone());
        let tasks = BackgroundTasks::default();
        tasks.spawn(async move {
            let _guard = task_guard;
            std::future::pending::<()>().await;
        });
        tokio::task::yield_now().await;
        drop(tasks);
        tokio::task::yield_now().await;
        assert!(finished.load(Ordering::SeqCst));
    }
}
