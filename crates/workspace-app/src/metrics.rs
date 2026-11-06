//! Opt-in aggregates only. No keys, paths, terminal text, transcripts or network.
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, VecDeque},
    sync::{Arc, Mutex},
    time::Instant,
};
#[derive(Clone)]
pub struct Metrics(Arc<Mutex<Data>>);
struct Data {
    enabled: bool,
    start: Instant,
    cpu_start: Option<f64>,
    inputs: u64,
    paints: u64,
    polls: u64,
    pending: Option<Instant>,
    last_paint: Option<Instant>,
    input_paint: VecDeque<f64>,
    frames: VecDeque<f64>,
    prepare: VecDeque<f64>,
    terminal: VecDeque<f64>,
    revisions: BTreeMap<usize, u64>,
}
fn sample(out: &mut VecDeque<f64>, v: f64) {
    if out.len() == 512 {
        out.pop_front();
    }
    out.push_back(v);
}
fn stats(values: &VecDeque<f64>) -> Value {
    let mut v = values.iter().copied().collect::<Vec<_>>();
    v.sort_by(f64::total_cmp);
    if v.is_empty() {
        return json!({"samples":0});
    }
    json!({"samples":v.len(),"median_ms":v[v.len()/2],"p95_ms":v[(v.len()*95/100).min(v.len()-1)],"max_ms":v.last()})
}
fn usage() -> Option<(f64, i64)> {
    // SAFETY: zeroed rusage is a valid output buffer and RUSAGE_SELF queries this process only.
    let mut r = unsafe { std::mem::zeroed::<libc::rusage>() };
    if unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut r) } != 0 {
        return None;
    }
    Some((
        r.ru_utime.tv_sec as f64
            + r.ru_stime.tv_sec as f64
            + (r.ru_utime.tv_usec + r.ru_stime.tv_usec) as f64 / 1_000_000.,
        r.ru_maxrss as i64,
    ))
}
#[cfg(target_os = "macos")]
fn resident_bytes() -> Option<u64> {
    // Query only this process. libc 0.2.191 exposes the Darwin proc_taskinfo ABI.
    let mut info = std::mem::MaybeUninit::<libc::proc_taskinfo>::zeroed();
    let size = std::mem::size_of::<libc::proc_taskinfo>();
    // SAFETY: valid aligned output buffer, exact ABI size, self PID, no retained pointer.
    let written = unsafe {
        libc::proc_pidinfo(
            libc::getpid(),
            libc::PROC_PIDTASKINFO,
            0,
            info.as_mut_ptr().cast(),
            size as i32,
        )
    };
    if written as usize != size {
        return None;
    }
    // SAFETY: proc_pidinfo initialized exactly the complete output struct.
    Some(unsafe { info.assume_init() }.pti_resident_size)
}
#[cfg(not(target_os = "macos"))]
fn resident_bytes() -> Option<u64> {
    None
}
fn peak_bytes(raw: i64) -> Option<u64> {
    // Installed Apple getrusage(2): ru_maxrss is bytes, not Linux kilobytes.
    if cfg!(target_os = "macos") {
        u64::try_from(raw).ok()
    } else {
        None
    }
}
impl Default for Metrics {
    fn default() -> Self {
        Self(Arc::new(Mutex::new(Data {
            enabled: false,
            start: Instant::now(),
            cpu_start: None,
            inputs: 0,
            paints: 0,
            polls: 0,
            pending: None,
            last_paint: None,
            input_paint: VecDeque::new(),
            frames: VecDeque::new(),
            prepare: VecDeque::new(),
            terminal: VecDeque::new(),
            revisions: BTreeMap::new(),
        })))
    }
}
impl Metrics {
    pub fn poll(&self) {
        if let Ok(mut data) = self.0.lock()
            && data.enabled
        {
            data.polls += 1;
        }
    }
    pub fn input(&self) {
        if let Ok(mut d) = self.0.lock()
            && d.enabled
        {
            d.inputs += 1;
            if d.pending.is_none() {
                d.pending = Some(Instant::now());
            }
        }
    }
    pub fn paint(&self) {
        if let Ok(mut d) = self.0.lock()
            && d.enabled
        {
            let now = Instant::now();
            d.paints += 1;
            if let Some(t) = d.pending.take() {
                sample(
                    &mut d.input_paint,
                    now.duration_since(t).as_secs_f64() * 1000.,
                );
            }
            if let Some(t) = d.last_paint.replace(now) {
                sample(&mut d.frames, now.duration_since(t).as_secs_f64() * 1000.);
            }
        }
    }
    pub fn preparation(&self, started: Instant) {
        if let Ok(mut d) = self.0.lock()
            && d.enabled
        {
            sample(&mut d.prepare, started.elapsed().as_secs_f64() * 1000.);
        }
    }
    pub fn terminal(&self, index: usize, revision: u64, published: Instant) {
        if let Ok(mut d) = self.0.lock()
            && d.enabled
            && published >= d.start
            && d.revisions.get(&index) != Some(&revision)
        {
            d.revisions.insert(index, revision);
            sample(&mut d.terminal, published.elapsed().as_secs_f64() * 1000.);
        }
    }
    pub fn report(&self) -> Value {
        let usage = usage();
        let Ok(mut d) = self.0.lock() else {
            return json!({"error":"metrics lock unavailable"});
        };
        if !d.enabled {
            d.enabled = true;
            d.start = Instant::now();
            d.cpu_start = usage.map(|(cpu, _)| cpu);
            d.inputs = 0;
            d.paints = 0;
            d.polls = 0;
            d.pending = None;
            d.last_paint = None;
            d.input_paint.clear();
            d.frames.clear();
            d.prepare.clear();
            d.terminal.clear();
            d.revisions.clear();
        }
        let elapsed = d.start.elapsed().as_secs_f64();
        json!({"enabled":true,"scope":"bounded in-memory aggregates, this app process only","window_seconds":elapsed,"input_events":d.inputs,"paint_callbacks":d.paints,"poll_ticks":d.polls,"cpu_one_core_percent_since_enabled":if elapsed>0.01{usage.zip(d.cpu_start).map(|((cpu,_),start)|(cpu-start)*100./elapsed)}else{None},"resident_bytes":resident_bytes(),"peak_resident_bytes":usage.and_then(|(_,rss)|peak_bytes(rss)),"maxrss_getrusage_raw":usage.map(|(_,rss)|rss),"maxrss_units":"macOS bytes, verified against installed getrusage(2); other platforms remain native raw","handler_to_next_paint_callback":stats(&d.input_paint),"snapshot_publish_to_terminal_paint_callback":stats(&d.terminal),"paint_callback_intervals_including_idle":stats(&d.frames),"render_preparation":stats(&d.prepare),"limits":"Paint callbacks do not measure physical presentation, visible echo or input-to-pixel. Intervals include idle; do not call them frame pacing under load. RSS covers this process, not child processes. No transcript recorded."})
    }
    pub fn enabled(&self) -> bool {
        self.0.lock().map(|data| data.enabled).unwrap_or(false)
    }
    pub fn disable(&self) {
        if let Ok(mut d) = self.0.lock() {
            d.enabled = false;
            d.pending = None;
        }
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    #[test]
    fn self_memory_queries_have_byte_units_and_reports_keep_only_aggregates() {
        let metrics = Metrics::default();
        let old_snapshot = Instant::now();
        let report = metrics.report();
        assert!(report["resident_bytes"].as_u64().unwrap() > 0);
        assert_eq!(
            report["peak_resident_bytes"],
            report["maxrss_getrusage_raw"]
        );
        assert!(report["peak_resident_bytes"].as_u64().unwrap() > 0);
        assert!(report["handler_to_next_paint_callback"]["samples"] == 0);
        assert!(report.get("paths").is_none() && report.get("terminal_text").is_none());
        metrics.terminal(0, 1, old_snapshot);
        assert_eq!(
            metrics.report()["snapshot_publish_to_terminal_paint_callback"]["samples"],
            0
        );
        metrics.input();
        metrics.paint();
        assert_eq!(metrics.report()["input_events"], 1);
        metrics.disable();
        metrics.report();
        let reset = metrics.report();
        assert_eq!(reset["input_events"], 0);
        assert_eq!(reset["paint_callbacks"], 0);
        assert_eq!(reset["handler_to_next_paint_callback"]["samples"], 0);
    }
}
