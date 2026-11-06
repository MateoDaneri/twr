//! One lazy search worker per window; one active and one replaceable pending job.
use super::editor::SearchJob;
use std::{
    path::PathBuf,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
pub struct Output {
    pub path: PathBuf,
    pub result: super::editor::SearchResult,
}
struct Shared {
    stop: AtomicBool,
    pending: Mutex<Option<(PathBuf, SearchJob)>>,
    ready: Condvar,
    output: Mutex<Option<Output>>,
    cancel: Mutex<Option<Arc<AtomicBool>>>,
}
pub struct Worker {
    shared: Arc<Shared>,
}
impl Worker {
    pub fn new(wake: Option<async_channel::Sender<()>>) -> Result<Self, String> {
        let shared = Arc::new(Shared {
            stop: AtomicBool::new(false),
            pending: Mutex::new(None),
            ready: Condvar::new(),
            output: Mutex::new(None),
            cancel: Mutex::new(None),
        });
        let state = shared.clone();
        std::thread::Builder::new()
            .name("editor-search".into())
            .spawn(move || {
                loop {
                    let request = {
                        let Ok(mut pending) = state.pending.lock() else {
                            break;
                        };
                        while pending.is_none() && !state.stop.load(Ordering::Acquire) {
                            pending = match state.ready.wait(pending) {
                                Ok(p) => p,
                                Err(_) => return,
                            };
                        }
                        if state.stop.load(Ordering::Acquire) {
                            break;
                        }
                        pending.take().unwrap()
                    };
                    let (path, job) = request;
                    let cancel = job.cancel.clone();
                    let result = job.run();
                    if !cancel.load(Ordering::Acquire) && !state.stop.load(Ordering::Acquire) {
                        if let Ok(mut output) = state.output.lock() {
                            *output = Some(Output { path, result });
                        }
                        if let Some(wake) = &wake {
                            let _ = wake.try_send(());
                        }
                    }
                }
            })
            .map_err(|e| e.to_string())?;
        Ok(Self { shared })
    }
    pub fn submit(&self, path: PathBuf, job: SearchJob) -> Result<(), String> {
        if let Some(old) = self
            .shared
            .cancel
            .lock()
            .map_err(|_| "Search cancel lock")?
            .replace(job.cancel.clone())
        {
            old.store(true, Ordering::Release);
        }
        self.shared
            .output
            .lock()
            .map_err(|_| "Search output lock")?
            .take();
        *self
            .shared
            .pending
            .lock()
            .map_err(|_| "Search queue lock")? = Some((path, job));
        self.shared.ready.notify_one();
        Ok(())
    }
    pub fn take(&self) -> Option<Output> {
        self.shared.output.lock().ok()?.take()
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
        if let Ok(cancel) = self.shared.cancel.lock()
            && let Some(cancel) = cancel.as_ref()
        {
            cancel.store(true, Ordering::Release);
        }
        if let Ok(_pending) = self.shared.pending.lock() {
            self.shared.ready.notify_one();
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::Editor;
    #[test]
    fn latest_request_wakes_and_old_buffer_identity_cannot_receive_result() {
        let (wake, receive) = async_channel::bounded(1);
        let worker = Worker::new(Some(wake)).unwrap();
        let mut e = Editor::new("uno UNO Uno".into());
        e.key("/");
        e.insert("uno");
        e.key("<Enter>");
        let old = e.take_search_job().unwrap();
        worker.submit(PathBuf::from("owned"), old).unwrap();
        e.key("/");
        e.insert("Uno");
        e.key("<Enter>");
        worker
            .submit(PathBuf::from("owned"), e.take_search_job().unwrap())
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if let Some(output) = worker.take()
                && e.apply_search(output.result)
            {
                break;
            }
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert!(receive.try_recv().is_ok());
        assert_eq!(e.cursor, 8);
        e.key("n");
        let old = e.take_search_job().unwrap().run();
        let mut reopened = Editor::new("unrelated".into());
        reopened.key("/");
        reopened.insert("rel");
        reopened.key("<Enter>");
        assert!(!reopened.apply_search(old));
        let result = reopened.take_search_job().unwrap().run();
        reopened.apply_search(result);
        assert_eq!(reopened.cursor, 2);
    }
}
