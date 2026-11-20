use std::{
    collections::{HashMap, VecDeque},
    sync::{
        Arc, Condvar, Mutex, OnceLock, Weak,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};

use image::RgbImage;
use tauri::AppHandle;

use super::{ModelKind, catalog, device, error};
use crate::duplicates::Candidate;

static JOBS: Slots = Slots::new(2);
static IMAGES: Slots = Slots::new(2);
static GPU: Slots = Slots::new(1);
pub(super) static GPU_QUERIES: GpuQueries = GpuQueries::new();
const CANCEL_POLL: Duration = Duration::from_millis(25);
const MAX_QUERY_YIELD: Duration = Duration::from_millis(60);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Mode {
    Serial,
    #[default]
    Pipeline,
    ConcurrentCpu,
    ConcurrentDirectMl,
}

pub(crate) struct Run {
    pub(crate) mode: Mode,
    #[cfg(test)]
    pub(crate) timings: Option<Timings>,
}

impl From<Mode> for Run {
    fn from(mode: Mode) -> Self {
        Self {
            mode,
            #[cfg(test)]
            timings: None,
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) enum Stage {
    Decode,
    Prepare,
    Inference,
    Write,
    Group,
    DecodeWait,
    InferenceWait,
    ModelLoad,
}

impl Run {
    pub(crate) fn inference<'a>(&'a self, mode: Mode, cancel: &'a AtomicBool) -> Inference<'a> {
        Inference {
            run: self,
            mode,
            cancel,
        }
    }

    fn count_decode(&self, _shared: bool) {
        #[cfg(test)]
        if let Some(timings) = &self.timings {
            timings.decodes[usize::from(_shared)].fetch_add(1, Ordering::Relaxed);
        }
    }

    #[inline]
    pub(crate) fn measure<T>(&self, _stage: Stage, operation: impl FnOnce() -> T) -> T {
        #[cfg(test)]
        if let Some(timings) = &self.timings {
            let started = std::time::Instant::now();
            let result = operation();
            timings.stages[_stage as usize]
                .fetch_add(started.elapsed().as_nanos() as u64, Ordering::Relaxed);
            return result;
        }
        operation()
    }
}

#[derive(Clone, Copy)]
pub(crate) struct Inference<'a> {
    run: &'a Run,
    mode: Mode,
    cancel: &'a AtomicBool,
}

impl Inference<'_> {
    pub(super) fn execute<T>(
        self,
        directml: bool,
        operation: impl FnOnce() -> T,
    ) -> Result<Option<T>, String> {
        let _gpu = if directml && self.mode == Mode::ConcurrentDirectMl {
            let permit = self
                .run
                .measure(Stage::InferenceWait, || GPU.acquire(1, self.cancel, || {}))?;
            let Some(permit) = permit else {
                return Ok(None);
            };
            Some(permit)
        } else {
            None
        };
        if directml && self.mode != Mode::Serial {
            self.run.measure(Stage::InferenceWait, || {
                GPU_QUERIES.yield_to_search(self.cancel)
            })?;
        }
        Ok((!self.cancel.load(Ordering::Relaxed))
            .then(|| self.run.measure(Stage::Inference, operation)))
    }
}

pub(super) struct GpuQueries {
    active: Mutex<usize>,
    idle: Condvar,
}

impl GpuQueries {
    const fn new() -> Self {
        Self {
            active: Mutex::new(0),
            idle: Condvar::new(),
        }
    }

    pub(super) fn start(&self) -> Result<GpuQuery<'_>, String> {
        *self.active.lock().map_err(error)? += 1;
        Ok(GpuQuery(self))
    }

    fn yield_to_search(&self, cancel: &AtomicBool) -> Result<(), String> {
        let mut active = self.active.lock().map_err(error)?;
        if *active == 0 {
            return Ok(());
        }
        let started = std::time::Instant::now();
        while *active > 0 && !cancel.load(Ordering::Relaxed) {
            let remaining = MAX_QUERY_YIELD.saturating_sub(started.elapsed());
            if remaining.is_zero() {
                break;
            }
            active = self
                .idle
                .wait_timeout(active, remaining.min(CANCEL_POLL))
                .map_err(error)?
                .0;
        }
        Ok(())
    }
}

pub(super) struct GpuQuery<'a>(&'a GpuQueries);

impl Drop for GpuQuery<'_> {
    fn drop(&mut self) {
        let mut active = self
            .0
            .active
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        *active -= 1;
        if *active == 0 {
            self.0.idle.notify_all();
        }
    }
}

#[cfg(test)]
#[derive(Default)]
pub(crate) struct Timings {
    stages: [std::sync::atomic::AtomicU64; 8],
    decodes: [std::sync::atomic::AtomicU64; 2],
}

#[cfg(test)]
#[derive(Debug)]
pub(crate) struct Measurements {
    pub(crate) millis: [f64; 8],
    pub(crate) decodes: [u64; 2],
}

#[cfg(test)]
impl Timings {
    pub(crate) fn measurements(&self) -> Measurements {
        Measurements {
            millis: self
                .stages
                .each_ref()
                .map(|stage| stage.load(Ordering::Relaxed) as f64 / 1e6),
            decodes: self
                .decodes
                .each_ref()
                .map(|count| count.load(Ordering::Relaxed)),
        }
    }
}

impl Mode {
    pub(crate) fn optimized(
        self,
        uses_directml: impl FnOnce() -> Result<bool, String>,
    ) -> Result<bool, String> {
        if self == Self::Serial {
            return Ok(false);
        }
        #[cfg(test)]
        if std::env::var_os("PHOTOSYS_AI_BENCHMARK_OPTIMIZED_CPU").is_some_and(|value| value == "1")
        {
            return Ok(true);
        }
        if self == Self::ConcurrentCpu {
            return Ok(true);
        }
        uses_directml()
    }

    pub(crate) fn configured(app: &AppHandle) -> Self {
        if std::env::var_os("PHOTOSYS_AI_INDEX_SERIAL").is_some_and(|value| value == "1") {
            return Self::Serial;
        }
        let concurrent = std::env::var_os("PHOTOSYS_AI_INDEX_CONCURRENT");
        if concurrent.as_deref().is_some_and(|value| value == "0") {
            return Self::Pipeline;
        }
        let threads = thread::available_parallelism().map_or(1, usize::from);
        let prefers_directml = device::prefers_directml(app);
        let directml_ready = prefers_directml && device::indexing_uses_directml();
        let cpu_opt_in = !prefers_directml && concurrent.is_some_and(|value| value == "1");
        Self::for_device(threads, directml_ready, cpu_opt_in)
    }

    fn for_device(threads: usize, directml_ready: bool, cpu_opt_in: bool) -> Self {
        match (threads >= 12, directml_ready, cpu_opt_in) {
            (true, true, _) => Self::ConcurrentDirectMl,
            (true, false, true) => Self::ConcurrentCpu,
            _ => Self::Pipeline,
        }
    }

    pub(crate) fn for_active_models(self) -> Self {
        if matches!(self, Self::ConcurrentCpu | Self::ConcurrentDirectMl)
            && (catalog::active(ModelKind::Search).id != "chinese-clip-vit-b16-fp32-v1"
                || catalog::active(ModelKind::People).id != "yunet-sface-fp32-v1")
        {
            Self::Pipeline
        } else {
            self
        }
    }

    pub(crate) fn admit(
        self,
        job: &Job,
        on_queued: impl FnOnce(),
    ) -> Result<Option<Permit<'static>>, String> {
        let count = match self {
            Self::Serial | Self::Pipeline => 2,
            Self::ConcurrentCpu | Self::ConcurrentDirectMl => 1,
        };
        let permit = JOBS.acquire(count, &job.cancel, || {
            job.queued.store(true, Ordering::Relaxed);
            on_queued();
        });
        job.queued.store(false, Ordering::Relaxed);
        permit
    }

    pub(crate) fn load_model<T>(
        self,
        cancel: &AtomicBool,
        run: impl FnOnce() -> T,
    ) -> Result<Option<T>, String> {
        if self == Self::ConcurrentDirectMl {
            let Some(_permit) = GPU.acquire(1, cancel, || {})? else {
                return Ok(None);
            };
            return Ok(Some(run()));
        }
        Ok((!cancel.load(Ordering::Relaxed)).then(run))
    }

    /// A rendezvous leaves at most one prepared photo waiting with the producer.
    pub(crate) fn with_prepared<J, T, R>(
        self,
        optimized: bool,
        jobs: impl Iterator<Item = J> + Send,
        prepare: impl FnMut(J) -> Option<T> + Send,
        consume: impl FnOnce(&mut dyn Iterator<Item = T>) -> Result<R, String>,
    ) -> Result<R, String>
    where
        T: Send,
    {
        let mut prepared = jobs.map_while(prepare);
        if self == Self::Serial {
            return consume(&mut prepared);
        }
        let _background = optimized.then(BackgroundThread::enter);
        thread::scope(|scope| {
            let (send, receive) = mpsc::sync_channel(0);
            let worker = thread::Builder::new()
                .name("photosys-index-prepare".into())
                .spawn_scoped(scope, move || {
                    let _background = optimized.then(BackgroundThread::enter);
                    for photo in prepared {
                        if send.send(photo).is_err() {
                            break;
                        }
                    }
                })
                .map_err(error)?;
            let mut photos = receive.into_iter();
            let result = consume(&mut photos);
            drop(photos);
            worker.join().map_err(|_| "AI 照片预处理异常结束。")?;
            result
        })
    }
}

// Tauri reuses the index consumer's blocking thread for other commands.
struct BackgroundThread {
    #[cfg(windows)]
    previous: Option<i32>,
}

impl BackgroundThread {
    fn enter() -> Self {
        #[cfg(windows)]
        {
            use windows::Win32::System::Threading::{
                GetCurrentThread, GetThreadPriority, SetThreadPriority,
                THREAD_PRIORITY_BELOW_NORMAL,
            };
            // The pseudo-handle refers to the thread running this scope.
            let previous = unsafe {
                let thread = GetCurrentThread();
                let previous = GetThreadPriority(thread);
                (previous != i32::MAX
                    && previous > THREAD_PRIORITY_BELOW_NORMAL.0
                    && SetThreadPriority(thread, THREAD_PRIORITY_BELOW_NORMAL).is_ok())
                .then_some(previous)
            };
            Self { previous }
        }
        #[cfg(not(windows))]
        Self {}
    }
}

impl Drop for BackgroundThread {
    fn drop(&mut self) {
        #[cfg(windows)]
        if let Some(previous) = self.previous {
            use windows::Win32::System::Threading::{
                GetCurrentThread, SetThreadPriority, THREAD_PRIORITY,
            };
            // The scope ends on the same thread that changed the priority.
            let _ = unsafe { SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY(previous)) };
        }
    }
}

type DecodeKey = (usize, Vec<u8>);

#[derive(Default)]
pub(crate) struct DecodePool {
    state: Mutex<DecodeState>,
}

#[derive(Default)]
struct DecodeState {
    admitted: usize,
    photos: HashMap<DecodeKey, Weak<Flight>>,
}

pub(crate) struct DecodeAccess<'a>(Option<&'a DecodePool>);

pub(crate) struct Decoded(Arc<Flight>);

impl Decoded {
    pub(crate) fn image(&self) -> &RgbImage {
        &self.0.outcome.get().unwrap().as_ref().unwrap().image
    }
}

struct DecodedImage {
    image: RgbImage,
    _slot: Permit<'static>,
}

#[derive(Default)]
struct Flight {
    outcome: OnceLock<Result<DecodedImage, ()>>,
    waiting: Mutex<()>,
    changed: Condvar,
}

impl Flight {
    fn finish(&self, outcome: Result<DecodedImage, ()>) {
        let _guard = self
            .waiting
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let _ = self.outcome.set(outcome);
        self.changed.notify_all();
    }

    fn wait(&self, cancel: &AtomicBool) -> Result<Option<bool>, String> {
        let mut guard = self.waiting.lock().map_err(error)?;
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Ok(None);
            }
            if let Some(outcome) = self.outcome.get() {
                return Ok(Some(outcome.is_ok()));
            }
            guard = self
                .changed
                .wait_timeout(guard, CANCEL_POLL)
                .map_err(error)?
                .0;
        }
    }
}

struct DecodeOwner<'a>(&'a Flight);

impl Drop for DecodeOwner<'_> {
    fn drop(&mut self) {
        if self.0.outcome.get().is_none() {
            self.0.finish(Err(()));
        }
    }
}

impl DecodePool {
    pub(crate) fn join(&self, mode: Mode) -> Result<DecodeAccess<'_>, String> {
        if !matches!(mode, Mode::ConcurrentCpu | Mode::ConcurrentDirectMl) {
            return Ok(DecodeAccess(None));
        }
        self.state.lock().map_err(error)?.admitted += 1;
        Ok(DecodeAccess(Some(self)))
    }

    fn claim(&self, key: &DecodeKey) -> Result<(Arc<Flight>, bool), String> {
        let mut state = self.state.lock().map_err(error)?;
        state.photos.retain(|_, flight| flight.strong_count() > 0);
        if state.admitted == 2
            && let Some(flight) = state.photos.get(key).and_then(Weak::upgrade)
            && !flight.outcome.get().is_some_and(Result::is_err)
        {
            return Ok((flight, false));
        }
        let flight = Arc::new(Flight::default());
        state.photos.insert(key.clone(), Arc::downgrade(&flight));
        Ok((flight, true))
    }
}

impl DecodeAccess<'_> {
    pub(crate) fn decode(
        &self,
        file: &Candidate,
        cancel: &AtomicBool,
        run: &Run,
        verify: impl FnOnce() -> Result<(), String>,
        decode: impl FnOnce() -> Result<RgbImage, String>,
    ) -> Option<Result<Decoded, String>> {
        let key = self.0.map(|_| (file.id, file.stamp.cache_key()));
        let flight = loop {
            let Some((pool, key)) = self.0.zip(key.as_ref()) else {
                break Arc::new(Flight::default());
            };
            let (flight, owner) = match pool.claim(key) {
                Ok(claim) => claim,
                Err(error) => return Some(Err(error)),
            };
            if owner {
                break flight;
            }
            match run.measure(Stage::DecodeWait, || flight.wait(cancel)) {
                Ok(Some(true)) => {
                    run.count_decode(true);
                    return Some(verify().map(|()| Decoded(flight)));
                }
                // The other job may have stopped during decode. Retry with this
                // job's cancellation flag and keep each job's failure reporting.
                Ok(Some(false)) => continue,
                Ok(None) => return None,
                Err(error) => return Some(Err(error)),
            }
        };
        let owner = DecodeOwner(&flight);
        let slot = match run.measure(Stage::DecodeWait, || IMAGES.acquire(1, cancel, || {})) {
            Ok(Some(slot)) => slot,
            Ok(None) => return None,
            Err(error) => return Some(Err(error)),
        };
        run.count_decode(false);
        let image = match run.measure(Stage::Decode, decode) {
            Ok(image) => image,
            Err(error) => return Some(Err(error)),
        };
        flight.finish(Ok(DecodedImage { image, _slot: slot }));
        drop(owner);
        Some(Ok(Decoded(flight)))
    }
}

impl Drop for DecodeAccess<'_> {
    fn drop(&mut self) {
        if let Some(pool) = self.0 {
            let mut state = pool.state.lock().unwrap_or_else(|error| error.into_inner());
            state.admitted -= 1;
            if state.admitted == 0 {
                state.photos.clear();
            }
        }
    }
}

pub(crate) struct Job {
    scan_id: u64,
    pub(crate) cancel: AtomicBool,
    queued: AtomicBool,
}

pub(crate) struct Active(Mutex<Option<Arc<Job>>>);

impl Active {
    pub(crate) const fn new() -> Self {
        Self(Mutex::new(None))
    }

    pub(crate) fn start(&self, scan_id: u64) -> Result<ActiveJob<'_>, String> {
        let job = Arc::new(Job {
            scan_id,
            cancel: AtomicBool::new(false),
            queued: AtomicBool::new(false),
        });
        *self.0.lock().map_err(error)? = Some(job.clone());
        Ok(ActiveJob { active: self, job })
    }

    pub(crate) fn status(&self, scan_id: u64) -> Result<(bool, bool), String> {
        Ok(self
            .0
            .lock()
            .map_err(error)?
            .as_ref()
            .filter(|job| job.scan_id == scan_id)
            .map_or((false, false), |job| {
                (true, job.queued.load(Ordering::Relaxed))
            }))
    }

    pub(crate) fn cancel(&self, scan_id: Option<u64>) {
        if let Ok(active) = self.0.lock()
            && let Some(job) = active.as_ref()
            && scan_id.is_none_or(|id| id == job.scan_id)
        {
            job.cancel.store(true, Ordering::Relaxed);
        }
    }
}

pub(crate) struct ActiveJob<'a> {
    active: &'a Active,
    pub(crate) job: Arc<Job>,
}

impl Drop for ActiveJob<'_> {
    fn drop(&mut self) {
        if let Ok(mut active) = self.active.0.lock() {
            *active = None;
        }
    }
}

struct Slots {
    capacity: usize,
    state: Mutex<SlotState>,
    changed: Condvar,
}

struct SlotState {
    used: usize,
    next: u64,
    waiting: VecDeque<u64>,
}

impl Slots {
    const fn new(capacity: usize) -> Self {
        Self {
            capacity,
            state: Mutex::new(SlotState {
                used: 0,
                next: 0,
                waiting: VecDeque::new(),
            }),
            changed: Condvar::new(),
        }
    }

    fn acquire(
        &self,
        count: usize,
        cancel: &AtomicBool,
        on_queued: impl FnOnce(),
    ) -> Result<Option<Permit<'_>>, String> {
        let mut state = self.state.lock().map_err(error)?;
        if cancel.load(Ordering::Relaxed) {
            return Ok(None);
        }
        if state.waiting.is_empty() && state.used + count <= self.capacity {
            state.used += count;
            return Ok(Some(Permit { slots: self, count }));
        }
        let ticket = state.next;
        state.next = state.next.wrapping_add(1);
        state.waiting.push_back(ticket);
        drop(state);
        on_queued();
        let mut state = self.state.lock().map_err(error)?;
        loop {
            if cancel.load(Ordering::Relaxed) {
                state.waiting.retain(|&queued| queued != ticket);
                self.changed.notify_all();
                return Ok(None);
            }
            if state.waiting.front() == Some(&ticket) && state.used + count <= self.capacity {
                state.waiting.pop_front();
                state.used += count;
                self.changed.notify_all();
                return Ok(Some(Permit { slots: self, count }));
            }
            state = self
                .changed
                .wait_timeout(state, CANCEL_POLL)
                .map_err(error)?
                .0;
        }
    }
}

pub(crate) struct Permit<'a> {
    slots: &'a Slots,
    count: usize,
}

impl Drop for Permit<'_> {
    fn drop(&mut self) {
        if let Ok(mut state) = self.slots.state.lock() {
            state.used -= self.count;
            self.slots.changed.notify_all();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs::File, path::PathBuf};

    #[test]
    fn optimized_index_requires_directml_or_cpu_opt_in() {
        assert!(!Mode::Serial.optimized(|| Ok(true)).unwrap());
        assert!(!Mode::Pipeline.optimized(|| Ok(false)).unwrap());
        assert!(Mode::Pipeline.optimized(|| Ok(true)).unwrap());
        assert!(Mode::ConcurrentCpu.optimized(|| Ok(false)).unwrap());
    }

    #[test]
    fn gpu_index_yields_for_a_bounded_time_while_search_is_active() {
        let queries = GpuQueries::new();
        let running = AtomicBool::new(false);
        let search = queries.start().unwrap();
        let started = std::time::Instant::now();
        queries.yield_to_search(&running).unwrap();
        assert!(started.elapsed() >= MAX_QUERY_YIELD);
        drop(search);
        assert_eq!(*queries.active.lock().unwrap(), 0);
    }

    fn candidate(file: &File) -> Candidate {
        Candidate {
            id: 0,
            relative: PathBuf::from("photo.jpg"),
            stamp: crate::duplicates::FileStamp::read(file).unwrap(),
        }
    }

    #[test]
    fn concurrent_decodes_reuse_the_live_image_without_retaining_it() {
        let file = tempfile::tempfile().unwrap();
        let candidate = candidate(&file);
        let pool = DecodePool::default();
        let cancel = AtomicBool::new(false);
        let run = Mode::ConcurrentCpu.into();
        let first = pool.join(Mode::ConcurrentCpu).unwrap();
        let second = pool.join(Mode::ConcurrentCpu).unwrap();
        let original = first
            .decode(
                &candidate,
                &cancel,
                &run,
                || Ok(()),
                || Ok(RgbImage::new(2, 3)),
            )
            .unwrap()
            .unwrap();
        let shared = second
            .decode(
                &candidate,
                &cancel,
                &run,
                || Ok(()),
                || panic!("decoded twice"),
            )
            .unwrap()
            .unwrap();
        assert!(Arc::ptr_eq(&original.0, &shared.0));
        assert_eq!(shared.image().dimensions(), (2, 3));
        let stale = second.decode(
            &candidate,
            &cancel,
            &run,
            || Err("stale".into()),
            || panic!("decoded twice"),
        );
        assert!(matches!(stale, Some(Err(error)) if error == "stale"));
        drop((original, shared));
        let new = second
            .decode(
                &candidate,
                &cancel,
                &run,
                || Ok(()),
                || Ok(RgbImage::new(4, 5)),
            )
            .unwrap()
            .unwrap();
        assert_eq!(new.image().dimensions(), (4, 5));
    }

    #[test]
    fn decode_wait_can_cancel_or_retry_an_abandoned_flight() {
        let file = tempfile::tempfile().unwrap();
        let candidate = candidate(&file);
        let pool = DecodePool::default();
        let mode = Mode::ConcurrentCpu;
        let run = mode.into();
        let first = pool.join(mode).unwrap();
        let second = pool.join(mode).unwrap();
        let cancelled = AtomicBool::new(true);
        let running = AtomicBool::new(false);
        let key = (candidate.id, candidate.stamp.cache_key());
        let (flight, owner) = pool.claim(&key).unwrap();
        assert!(owner);
        let owner = DecodeOwner(&flight);
        assert!(
            second
                .decode(
                    &candidate,
                    &cancelled,
                    &run,
                    || panic!("verified after cancellation"),
                    || panic!("decoded after cancellation"),
                )
                .is_none()
        );
        thread::scope(|scope| {
            let retry = scope.spawn(|| {
                second
                    .decode(
                        &candidate,
                        &running,
                        &run,
                        || panic!("reused failed decode"),
                        || Ok(RgbImage::new(1, 2)),
                    )
                    .unwrap()
                    .unwrap()
            });
            let start = std::time::Instant::now();
            while Arc::strong_count(&flight) == 1 {
                assert!(start.elapsed() < Duration::from_secs(2));
                thread::yield_now();
            }
            drop(owner);
            let image = retry.join().unwrap();
            assert_eq!(image.image().dimensions(), (1, 2));
        });
        drop(first);
    }

    #[test]
    fn concurrent_default_is_limited_to_validated_directml_configuration() {
        assert_eq!(Mode::for_device(12, true, false), Mode::ConcurrentDirectMl);
        assert_eq!(Mode::for_device(8, true, false), Mode::Pipeline);
        assert_eq!(Mode::for_device(12, false, false), Mode::Pipeline);
        assert_eq!(Mode::for_device(12, false, true), Mode::ConcurrentCpu);
        assert_eq!(Mode::for_device(8, false, true), Mode::Pipeline);
    }

    #[test]
    fn queued_job_reports_status_and_cancels_before_admission() {
        let active = Active::new();
        let job = active.start(7).unwrap();
        let held = Mode::Pipeline.admit(&job.job, || {}).unwrap().unwrap();
        let queued = Active::new();
        let other = queued.start(7).unwrap();
        assert!(
            Mode::Pipeline
                .admit(&other.job, || {
                    assert_eq!(queued.status(7).unwrap(), (true, true));
                    queued.cancel(Some(8));
                    assert!(!other.job.cancel.load(Ordering::Relaxed));
                    queued.cancel(Some(7));
                })
                .unwrap()
                .is_none()
        );
        drop(other);
        assert_eq!(queued.status(7).unwrap(), (false, false));
        drop(held);
    }

    #[test]
    fn two_concurrent_jobs_admit_while_serial_job_waits() {
        let slots = Slots::new(2);
        let cancel = AtomicBool::new(false);
        let search = slots.acquire(1, &cancel, || {}).unwrap().unwrap();
        let people = slots.acquire(1, &cancel, || {}).unwrap().unwrap();
        assert!(
            slots
                .acquire(2, &cancel, || cancel.store(true, Ordering::Relaxed))
                .unwrap()
                .is_none()
        );
        drop((search, people));
        cancel.store(false, Ordering::Relaxed);
        assert!(
            slots
                .acquire(2, &cancel, || panic!("abandoned ticket"))
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn prefetch_overlaps_and_drains_when_consumer_stops() {
        let (send, prepared) = mpsc::channel();
        Mode::Pipeline
            .with_prepared(
                true,
                0..3,
                |photo| {
                    send.send(photo).unwrap();
                    Some(photo)
                },
                |photos| {
                    assert_eq!(photos.next(), Some(0));
                    assert_eq!(prepared.recv_timeout(Duration::from_secs(2)).unwrap(), 0);
                    assert_eq!(prepared.recv_timeout(Duration::from_secs(2)).unwrap(), 1);
                    assert!(prepared.try_recv().is_err());
                    Ok(())
                },
            )
            .unwrap();
        assert!(prepared.try_recv().is_err());
    }
}
