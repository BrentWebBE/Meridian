//! Debug statistics: what the compositor is doing, once per second.
//!
//! Off by default. Start with `MERIDIAN_STATS=1` to log a line per second,
//! or `MERIDIAN_STATS=/path/file.csv` to append machine-readable rows.
//!
//! When off, `Stats` holds `None` and every call is a single branch: no
//! clock is read, nothing is allocated and no timer exists.

use std::{
    collections::BTreeMap,
    fs::File,
    io::Write,
    time::{Duration, Instant},
};

/// One reporting window's worth of numbers.
struct Inner {
    /// CSV output, if a path was given; otherwise we log.
    file: Option<File>,
    started: Instant,
    window_start: Instant,
    /// CPU time spent building and submitting each frame, in milliseconds.
    frame_ms: Vec<f32>,
    /// Redraws that showed a frame / that found nothing changed.
    redraws: u32,
    empty_redraws: u32,
    frame_callbacks: u32,
    /// Why redraws were requested, with counts.
    reasons: BTreeMap<&'static str, u32>,
    /// Our total CPU ticks at the start of the window.
    cpu_ticks: u64,
}

pub struct Stats(Option<Box<Inner>>);

/// The interval at which `report` should be called when enabled.
pub const REPORT_EVERY: Duration = Duration::from_secs(1);

impl Stats {
    pub fn from_env() -> Self {
        let Some(value) = std::env::var_os("MERIDIAN_STATS") else {
            return Self(None);
        };
        if value.is_empty() || value == "0" {
            return Self(None);
        }
        // Anything with a slash is a file to append CSV rows to.
        let file = value.to_str().filter(|v| v.contains('/')).and_then(|path| {
            let mut file = File::options().create(true).append(true).open(path).ok()?;
            let _ = writeln!(
                file,
                "seconds,redraws,empty_redraws,frame_p50_ms,frame_p95_ms,frame_max_ms,frame_callbacks,surfaces,rss_kb,cpu_percent,reasons"
            );
            Some(file)
        });
        let now = Instant::now();
        Self(Some(Box::new(Inner {
            file,
            started: now,
            window_start: now,
            frame_ms: Vec::new(),
            redraws: 0,
            empty_redraws: 0,
            frame_callbacks: 0,
            reasons: BTreeMap::new(),
            cpu_ticks: cpu_ticks(),
        })))
    }

    pub fn enabled(&self) -> bool {
        self.0.is_some()
    }

    /// The time now, but only if someone will use it.
    pub fn start_frame(&self) -> Option<Instant> {
        self.0.as_ref().map(|_| Instant::now())
    }

    /// A redraw finished. `started` comes from `start_frame`; `empty` means
    /// nothing had changed and no frame was shown.
    pub fn end_frame(&mut self, started: Option<Instant>, empty: bool) {
        if let (Some(inner), Some(started)) = (&mut self.0, started) {
            inner.frame_ms.push(started.elapsed().as_secs_f32() * 1000.0);
            if empty {
                inner.empty_redraws += 1;
            } else {
                inner.redraws += 1;
            }
        }
    }

    /// Something asked for a redraw.
    pub fn reason(&mut self, why: &'static str) {
        if let Some(inner) = &mut self.0 {
            *inner.reasons.entry(why).or_default() += 1;
        }
    }

    pub fn frame_callbacks(&mut self, count: usize) {
        if let Some(inner) = &mut self.0 {
            inner.frame_callbacks += count as u32;
        }
    }

    /// Write out the window that just ended and start a new one.
    /// `surfaces`: how many windows and layer surfaces exist right now.
    pub fn report(&mut self, surfaces: usize) {
        let Some(inner) = &mut self.0 else { return };
        let elapsed = inner.window_start.elapsed().as_secs_f64().max(0.001);

        inner.frame_ms.sort_by(|a, b| a.total_cmp(b));
        let percentile = |p: f64| -> f32 {
            match inner.frame_ms.len() {
                0 => 0.0,
                len => inner.frame_ms[((len - 1) as f64 * p).round() as usize],
            }
        };
        let (p50, p95, max) = (percentile(0.50), percentile(0.95), percentile(1.0));

        let ticks = cpu_ticks();
        // Linux counts process CPU time in ticks of 1/100 s.
        let cpu_percent = (ticks - inner.cpu_ticks) as f64 / 100.0 / elapsed * 100.0;
        let rss = rss_kb();
        let reasons = inner
            .reasons
            .iter()
            .map(|(why, count)| format!("{why}={count}"))
            .collect::<Vec<_>>()
            .join(" ");

        match &mut inner.file {
            Some(file) => {
                let _ = writeln!(
                    file,
                    "{:.0},{},{},{p50:.2},{p95:.2},{max:.2},{},{surfaces},{rss},{cpu_percent:.1},{reasons}",
                    inner.started.elapsed().as_secs_f64(),
                    inner.redraws,
                    inner.empty_redraws,
                    inner.frame_callbacks,
                );
            }
            None => tracing::info!(
                "stats: {} redraws + {} empty, frame p50 {p50:.2} ms p95 {p95:.2} ms max {max:.2} ms, \
                 {} frame callbacks, {surfaces} surfaces, rss {} MB, cpu {cpu_percent:.1}% [{reasons}]",
                inner.redraws,
                inner.empty_redraws,
                inner.frame_callbacks,
                rss / 1024,
            ),
        }

        inner.window_start = Instant::now();
        inner.frame_ms.clear();
        inner.redraws = 0;
        inner.empty_redraws = 0;
        inner.frame_callbacks = 0;
        inner.reasons.clear();
        inner.cpu_ticks = ticks;
    }
}

/// CPU time this process has used so far (user + system), in clock ticks.
/// Fields 14 and 15 of /proc/self/stat; the process name in field 2 may
/// contain spaces, so we count from the closing parenthesis.
fn cpu_ticks() -> u64 {
    let Ok(stat) = std::fs::read_to_string("/proc/self/stat") else {
        return 0;
    };
    parse_cpu_ticks(&stat).unwrap_or(0)
}

fn parse_cpu_ticks(stat: &str) -> Option<u64> {
    let after_name = &stat[stat.rfind(')')? + 1..];
    let mut fields = after_name.split_whitespace();
    // After the name come state (field 3) ... utime is field 14, stime 15.
    let utime: u64 = fields.nth(11)?.parse().ok()?;
    let stime: u64 = fields.next()?.parse().ok()?;
    Some(utime + stime)
}

/// Resident memory in kilobytes, from /proc/self/statm (pages).
fn rss_kb() -> u64 {
    std::fs::read_to_string("/proc/self/statm")
        .ok()
        .and_then(|statm| statm.split_whitespace().nth(1)?.parse::<u64>().ok())
        .map_or(0, |pages| pages * 4)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_ticks_are_read_past_a_name_with_spaces() {
        let stat = "1234 (my (odd) name) S 1 1234 1234 0 -1 4194304 500 0 0 0 73 21 0 0 20 0 8 0 100 1000 200";
        assert_eq!(parse_cpu_ticks(stat), Some(94));
        assert_eq!(parse_cpu_ticks("garbage"), None);
    }

    #[test]
    fn disabled_stats_do_nothing() {
        let mut stats = Stats(None);
        assert!(!stats.enabled());
        assert!(stats.start_frame().is_none());
        stats.end_frame(None, false);
        stats.reason("test");
        stats.report(3); // must not panic or allocate
    }
}
