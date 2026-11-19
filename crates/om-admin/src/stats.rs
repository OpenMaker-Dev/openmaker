//! The Dashboard's System Statistics and Volume Status
//! (`docs/re/admin-console.md` 4.1; `docs/admin-console.md`, "Statistics").
//!
//! A sampler started with the console reads the operating system once a
//! minute, on the minute, and keeps the last ten samples, as FileMaker
//! Server's charts show. Its first sample comes a second after the console
//! starts, so the chart is not empty for a minute. Each sample holds:
//!
//! - **CPU**: the share of all processors' time that was busy (not idle or
//!   waiting for I/O) since the previous sample, in percent.
//! - **Memory**: the share of memory in use (total less what the kernel
//!   reports as available), in percent.
//! - **Network**: bytes received and sent per second by every interface but
//!   loopback since the previous sample.
//! - **Disk**: megabytes (2^20 bytes) read and written per second by the
//!   machine's disks since the previous sample, counting each physical
//!   device once and not its partitions or the virtual devices above it.
//!
//! What counts towards each figure is OpenMaker's choice: FileMaker Server's
//! definitions are not documented. The figures come from Linux's `/proc`
//! (`stat`, `meminfo`, `net/dev`, `diskstats`); on other systems they are
//! not available. The sampler also measures the file system that holds the
//! backend's data folder, for Volume Status, on Unix systems.

use std::collections::VecDeque;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tokio::sync::watch;

use crate::backend::Backend;

/// Samples kept, as FileMaker Server shows.
pub(crate) const SAMPLES: usize = 10;

/// Time between samples.
const INTERVAL: Duration = Duration::from_secs(60);

/// The wait before the first sample, which measures rates over it.
const FIRST_SAMPLE: Duration = Duration::from_secs(1);

/// One sample; a figure the system did not give is `None`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Sample {
    pub(crate) time: SystemTime,
    pub(crate) cpu: Option<f64>,
    pub(crate) memory: Option<f64>,
    pub(crate) network: Option<f64>,
    pub(crate) disk: Option<f64>,
}

/// The data folder's file system as last read: `None` when there is no
/// data folder or the system cannot be asked, else the space or why it
/// could not be read.
pub(crate) type VolumeReading = Option<Result<Volume, String>>;

/// The file system that holds the hosted files, in bytes. `used` and
/// `free` add up to `total`; space reserved for the superuser is left out,
/// as `df` leaves it out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Volume {
    pub(crate) total: u64,
    pub(crate) used: u64,
    pub(crate) free: u64,
}

/// What the System Statistics chart can show, with FileMaker Server's tab,
/// chart and axis titles (`docs/re/admin-console.md` 4.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Metric {
    Cpu,
    Memory,
    Network,
    Disk,
}

impl Metric {
    pub(crate) const ALL: [Metric; 4] =
        [Metric::Cpu, Metric::Memory, Metric::Network, Metric::Disk];

    /// The query value that picks it: `?chart=memory`.
    pub(crate) fn key(self) -> &'static str {
        match self {
            Metric::Cpu => "cpu",
            Metric::Memory => "memory",
            Metric::Network => "network",
            Metric::Disk => "disk",
        }
    }

    pub(crate) fn from_key(key: &str) -> Option<Metric> {
        Metric::ALL.into_iter().find(|m| m.key() == key)
    }

    /// The tab's label.
    pub(crate) fn label(self) -> &'static str {
        match self {
            Metric::Cpu => "CPU",
            Metric::Memory => "Memory",
            Metric::Network => "Network",
            Metric::Disk => "Disk",
        }
    }

    /// The chart's title.
    pub(crate) fn title(self) -> &'static str {
        match self {
            Metric::Cpu => "CPU Usage",
            Metric::Memory => "Memory Usage",
            Metric::Network => "Network Throughput",
            Metric::Disk => "Disk Throughput",
        }
    }

    /// The y axis's title.
    pub(crate) fn unit(self) -> &'static str {
        match self {
            Metric::Cpu | Metric::Memory => "Percentage",
            Metric::Network => "Bytes per Second",
            Metric::Disk => "MB per Second",
        }
    }

    pub(crate) fn value(self, sample: &Sample) -> Option<f64> {
        match self {
            Metric::Cpu => sample.cpu,
            Metric::Memory => sample.memory,
            Metric::Network => sample.network,
            Metric::Disk => sample.disk,
        }
    }
}

/// The samples and the latest Volume Status, shared by the sampler and the
/// pages.
#[derive(Debug, Default)]
pub(crate) struct Statistics {
    inner: Mutex<Inner>,
}

#[derive(Debug, Default)]
struct Inner {
    /// Oldest first, at most [`SAMPLES`].
    samples: VecDeque<Sample>,
    volume: VolumeReading,
}

impl Statistics {
    // Each change is one push, pop or assignment, so a panic elsewhere
    // cannot leave the state half-changed.
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The samples, oldest first.
    pub(crate) fn samples(&self) -> Vec<Sample> {
        self.lock().samples.iter().copied().collect()
    }

    /// The file system of the data folder at the latest sample.
    pub(crate) fn volume(&self) -> VolumeReading {
        self.lock().volume.clone()
    }

    fn record(&self, sample: Sample, volume: VolumeReading) {
        let mut inner = self.lock();
        if inner.samples.len() == SAMPLES {
            inner.samples.pop_front();
        }
        inner.samples.push_back(sample);
        inner.volume = volume;
    }
}

/// Sample the system for `statistics` until `stopped` turns true: the file
/// system at once, then everything a second later and on every minute.
/// The reading runs on a blocking thread, so a slow file system cannot
/// hold up the console's pages; only one reading runs at a time.
pub(crate) async fn run(
    statistics: Arc<Statistics>,
    backend: Arc<dyn Backend>,
    mut stopped: watch::Receiver<bool>,
) {
    let mut previous: Option<Counters> = None;
    let mut wait = Duration::ZERO;
    loop {
        tokio::select! {
            _ = tokio::time::sleep(wait) => {}
            _ = stopped.wait_for(|stop| *stop) => return,
        }
        let folder = backend.data_folder();
        let reading = tokio::task::spawn_blocking(move || {
            (Counters::read(), folder.as_deref().and_then(volume_of))
        })
        .await;
        let Ok((counters, volume)) = reading else {
            return;
        };
        match previous.replace(counters) {
            // The first reading is the baseline the first sample's rates
            // are measured against; the file system is shown at once.
            None => {
                statistics.lock().volume = volume;
                wait = FIRST_SAMPLE;
            }
            Some(before) => {
                statistics.record(sample(&before, &counters), volume);
                wait = until_next_minute(SystemTime::now());
            }
        }
    }
}

/// The time from `now` to the next whole minute, at least a second.
fn until_next_minute(now: SystemTime) -> Duration {
    let since_epoch = now.duration_since(UNIX_EPOCH).unwrap_or_default();
    let into_minute = Duration::from_nanos(
        u64::try_from(since_epoch.as_nanos() % INTERVAL.as_nanos()).unwrap_or(0),
    );
    (INTERVAL - into_minute).max(Duration::from_secs(1))
}

/// What the operating system counts, read at `at`. Rates are the change
/// between two readings over the time between them.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Counters {
    at: Instant,
    time: SystemTime,
    /// All processors' busy and total time, in the kernel's ticks.
    cpu: Option<(u64, u64)>,
    /// The share of memory in use, in percent: not a counter.
    memory: Option<f64>,
    /// Bytes received and sent.
    network: Option<u64>,
    /// Bytes read and written.
    disk: Option<u64>,
}

/// The sample from the change between readings `before` and `after`.
fn sample(before: &Counters, after: &Counters) -> Sample {
    let seconds = after.at.saturating_duration_since(before.at).as_secs_f64();
    // A counter that went back (a device that went away) gives no rate.
    let rate = |a: Option<u64>, b: Option<u64>, scale: f64| match (a, b) {
        (Some(a), Some(b)) if b >= a && seconds > 0.0 => Some((b - a) as f64 / seconds / scale),
        _ => None,
    };
    let cpu = match (before.cpu, after.cpu) {
        (Some((busy0, total0)), Some((busy1, total1))) if total1 > total0 && busy1 >= busy0 => {
            Some(((busy1 - busy0) as f64 / (total1 - total0) as f64 * 100.0).min(100.0))
        }
        _ => None,
    };
    Sample {
        time: after.time,
        cpu,
        memory: after.memory,
        network: rate(before.network, after.network, 1.0),
        disk: rate(before.disk, after.disk, 1024.0 * 1024.0),
    }
}

impl Counters {
    fn read() -> Counters {
        #[cfg(target_os = "linux")]
        let (cpu, memory, network, disk) = (
            linux::read("/proc/stat").as_deref().and_then(linux::cpu),
            linux::read("/proc/meminfo")
                .as_deref()
                .and_then(linux::memory),
            linux::read("/proc/net/dev")
                .as_deref()
                .and_then(linux::network),
            linux::read("/proc/diskstats")
                .as_deref()
                .and_then(|text| linux::disk(text, linux::is_physical_disk)),
        );
        #[cfg(not(target_os = "linux"))]
        let (cpu, memory, network, disk) = (None, None, None, None);
        Counters {
            at: Instant::now(),
            time: SystemTime::now(),
            cpu,
            memory,
            network,
            disk,
        }
    }
}

/// The file system that holds `folder`, or why it cannot be read.
#[cfg(unix)]
fn volume_of(folder: &Path) -> VolumeReading {
    let stat = match rustix::fs::statvfs(folder) {
        Ok(stat) => stat,
        Err(e) => return Some(Err(std::io::Error::from(e).to_string())),
    };
    let unit = stat.f_frsize;
    // Blocks in use, and those free to anyone; the rest is reserved.
    let sizes = || {
        let used = stat.f_blocks.checked_sub(stat.f_bfree)?.checked_mul(unit)?;
        let free = stat.f_bavail.checked_mul(unit)?;
        Some(Volume {
            total: used.checked_add(free)?,
            used,
            free,
        })
    };
    Some(sizes().ok_or_else(|| "the file system reported impossible sizes".to_owned()))
}

#[cfg(not(unix))]
fn volume_of(_folder: &Path) -> VolumeReading {
    None
}

/// Reading Linux's counters. The files are the kernel's, but each is read
/// with a size cap and parsed without trusting its shape.
#[cfg(any(target_os = "linux", test))]
mod linux {
    #[cfg(target_os = "linux")]
    use std::io::Read;

    /// The largest `/proc` file read: `diskstats` on a machine with
    /// thousands of devices stays well under it.
    #[cfg(target_os = "linux")]
    const MAX_FILE: u64 = 4 * 1024 * 1024;

    #[cfg(target_os = "linux")]
    pub(super) fn read(path: &str) -> Option<String> {
        let mut text = String::new();
        std::fs::File::open(path)
            .ok()?
            .take(MAX_FILE)
            .read_to_string(&mut text)
            .ok()?;
        Some(text)
    }

    /// Busy and total ticks of all processors, from `/proc/stat`'s `cpu`
    /// line: user, nice, system, idle, iowait, irq, softirq, steal (guest
    /// time is already in user).
    pub(super) fn cpu(stat: &str) -> Option<(u64, u64)> {
        let line = stat.lines().find(|l| l.starts_with("cpu "))?;
        let ticks: Vec<u64> = line
            .split_whitespace()
            .skip(1)
            .take(8)
            .map(|t| t.parse().ok())
            .collect::<Option<_>>()?;
        let total = ticks.iter().try_fold(0u64, |sum, t| sum.checked_add(*t))?;
        let idle = ticks.get(3)?.checked_add(*ticks.get(4).unwrap_or(&0))?;
        Some((total.checked_sub(idle)?, total))
    }

    /// The share of memory in use, from `/proc/meminfo`.
    pub(super) fn memory(meminfo: &str) -> Option<f64> {
        let field = |name: &str| -> Option<u64> {
            let line = meminfo.lines().find(|l| l.starts_with(name))?;
            line.get(name.len()..)?
                .split_whitespace()
                .next()?
                .parse()
                .ok()
        };
        let total = field("MemTotal:")?;
        let available = field("MemAvailable:")?;
        (total > 0).then(|| total.saturating_sub(available) as f64 / total as f64 * 100.0)
    }

    /// Bytes received and sent by every interface but loopback, from
    /// `/proc/net/dev`.
    pub(super) fn network(dev: &str) -> Option<u64> {
        let mut sum = 0u64;
        let mut any = false;
        for line in dev.lines() {
            let Some((name, counters)) = line.split_once(':') else {
                continue;
            };
            if name.trim() == "lo" {
                continue;
            }
            let fields: Vec<&str> = counters.split_whitespace().collect();
            let (Some(received), Some(sent)) = (fields.first(), fields.get(8)) else {
                continue;
            };
            let (Ok(received), Ok(sent)) = (received.parse::<u64>(), sent.parse::<u64>()) else {
                continue;
            };
            sum = sum.wrapping_add(received).wrapping_add(sent);
            any = true;
        }
        any.then_some(sum)
    }

    /// Bytes read and written by the devices `physical` accepts, from
    /// `/proc/diskstats` (sectors of 512 bytes, whatever the device's).
    pub(super) fn disk(diskstats: &str, physical: impl Fn(&str) -> bool) -> Option<u64> {
        let mut sectors = 0u64;
        let mut any = false;
        for line in diskstats.lines() {
            let fields: Vec<&str> = line.split_whitespace().collect();
            let (Some(name), Some(read), Some(written)) =
                (fields.get(2), fields.get(5), fields.get(9))
            else {
                continue;
            };
            if !physical(name) {
                continue;
            }
            let (Ok(read), Ok(written)) = (read.parse::<u64>(), written.parse::<u64>()) else {
                continue;
            };
            sectors = sectors.wrapping_add(read).wrapping_add(written);
            any = true;
        }
        any.then(|| sectors.wrapping_mul(512))
    }

    /// Whether block device `name` is a whole physical disk: listed in
    /// `/sys/block` with a device behind it. Partitions are not in
    /// `/sys/block`; loop, device-mapper and RAID devices have no device.
    #[cfg(target_os = "linux")]
    pub(super) fn is_physical_disk(name: &str) -> bool {
        !name.is_empty()
            && !name.contains(['/', '\0'])
            && name != "."
            && name != ".."
            && std::path::Path::new("/sys/block")
                .join(name)
                .join("device")
                .exists()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linux_counters_parse_and_become_rates() {
        let stat = "cpu  100 0 50 800 50 0 0 0 0 0\ncpu0 1 2 3 4 5 6 7 8 9 10\n";
        assert_eq!(linux::cpu(stat), Some((150, 1000)));
        assert_eq!(linux::cpu("cpu  1 2 x"), None);
        let meminfo = "MemTotal:       1000 kB\nMemFree: 1 kB\nMemAvailable:    750 kB\n";
        assert_eq!(linux::memory(meminfo), Some(25.0));
        let dev = "Inter-|   Receive |  Transmit\n face |bytes packets|bytes\n    \
                   lo: 999 1 0 0 0 0 0 0 999 1 0 0 0 0 0 0\n  eth0: 1000 1 0 0 0 0 0 0 \
                   500 1 0 0 0 0 0 0\n";
        assert_eq!(linux::network(dev), Some(1500));
        let diskstats = "   8       0 sda 10 0 100 0 5 0 60 0 0 0 0\n   \
                         8       1 sda1 10 0 100 0 5 0 60 0 0 0 0\n   \
                         7       0 loop0 1 0 8 0 0 0 0 0 0 0 0\n";
        assert_eq!(
            linux::disk(diskstats, |name| name == "sda"),
            Some(160 * 512)
        );

        let start = Instant::now();
        let before = Counters {
            at: start,
            time: SystemTime::now(),
            cpu: Some((150, 1000)),
            memory: Some(20.0),
            network: Some(1_000),
            disk: Some(0),
        };
        let after = Counters {
            at: start + Duration::from_secs(60),
            cpu: Some((450, 2000)),
            memory: Some(25.0),
            network: Some(61_000),
            disk: Some(60 * 1024 * 1024),
            ..before
        };
        let s = sample(&before, &after);
        assert_eq!(s.cpu, Some(30.0));
        assert_eq!(s.memory, Some(25.0));
        assert_eq!(s.network, Some(1_000.0));
        assert_eq!(s.disk, Some(1.0));
        // A counter that went back gives no rate rather than a huge one.
        let reset = Counters {
            network: Some(10),
            ..after
        };
        assert_eq!(sample(&after, &reset).network, None);
    }
}
