//! Command-boundary guards: helpers must not acquire a second read guard.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard, TryLockError};

static ACCESS: Access = Access::new();
const BUSY: &str = "数据库维护正在进行，请稍后重试。";
struct Access {
    lock: RwLock<()>,
    pending: AtomicBool,
}
impl Access {
    const fn new() -> Self {
        Self {
            lock: RwLock::new(()),
            pending: AtomicBool::new(false),
        }
    }
    fn read(&self) -> Result<RwLockReadGuard<'_, ()>, String> {
        if self.pending.load(Ordering::SeqCst) {
            return Err(BUSY.into());
        }
        let guard = self.lock.try_read().map_err(|_| BUSY.to_string())?;
        if self.pending.load(Ordering::SeqCst) {
            return Err(BUSY.into());
        }
        Ok(guard)
    }
    fn maintenance(&self, cancel: impl Fn()) -> Result<Maintenance<'_>, String> {
        self.pending
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .map_err(|_| BUSY.to_string())?;
        loop {
            // Repeat cancellation to catch indexers admitted immediately before pending.
            cancel();
            match self.lock.try_write() {
                Ok(guard) => {
                    return Ok(Maintenance {
                        _guard: guard,
                        access: self,
                    });
                }
                Err(TryLockError::WouldBlock) => {
                    std::thread::sleep(std::time::Duration::from_millis(25))
                }
                Err(TryLockError::Poisoned(_)) => {
                    self.pending.store(false, Ordering::SeqCst);
                    return Err("数据库任务异常结束，请重启应用。".into());
                }
            }
        }
    }
}
pub(crate) fn read() -> Result<RwLockReadGuard<'static, ()>, String> {
    ACCESS.read()
}
pub(crate) struct Maintenance<'a> {
    _guard: RwLockWriteGuard<'a, ()>,
    access: &'a Access,
}
impl Drop for Maintenance<'_> {
    fn drop(&mut self) {
        self.access.pending.store(false, Ordering::SeqCst);
    }
}
pub(crate) fn maintenance() -> Result<Maintenance<'static>, String> {
    ACCESS.maintenance(|| {
        crate::library::ai_search::cancel_all();
        crate::library::people::cancel_all();
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, mpsc};
    use std::time::Duration;
    #[test]
    fn maintenance_drains_index_and_edit_commands_and_rejects_new_commands() {
        let access = Arc::new(Access::new());
        let index = access.read().unwrap();
        let edit = access.read().unwrap();
        let (pending, waiting) = mpsc::channel();
        let (acquired, completed) = mpsc::channel();
        let worker_access = access.clone();
        let worker = std::thread::spawn(move || {
            let _exclusive = worker_access
                .maintenance(|| {
                    let _ = pending.send(());
                })
                .unwrap();
            assert!(worker_access.read().is_err());
            assert!(worker_access.maintenance(|| {}).is_err());
            acquired.send(()).unwrap();
        });
        waiting.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(access.read().is_err());
        assert!(completed.try_recv().is_err());
        drop(index);
        assert!(completed.try_recv().is_err());
        drop(edit);
        completed.recv_timeout(Duration::from_secs(2)).unwrap();
        worker.join().unwrap();
        assert!(access.read().is_ok());
    }
    #[test]
    fn cancellation_is_repeated_for_late_starting_indexers() {
        let access = Arc::new(Access::new());
        let held = access.read().unwrap();
        let (cancel, received) = mpsc::channel();
        let other = access.clone();
        let worker = std::thread::spawn(move || {
            let _guard = other
                .maintenance(|| {
                    let _ = cancel.send(());
                })
                .unwrap();
        });
        received.recv_timeout(Duration::from_secs(2)).unwrap();
        received.recv_timeout(Duration::from_secs(2)).unwrap();
        drop(held);
        worker.join().unwrap();
        assert!(access.read().is_ok());
    }
}
