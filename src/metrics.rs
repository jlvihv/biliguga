//! Reading this process's own memory and CPU, for the benchmarks.
//!
//! `#[cfg(test)]` at its declaration: the load tests are the only caller, and a released
//! binary has no reason to carry a `/proc` reader it never uses.
//!
//! Linux only, from `/proc/self`. `procwatch` from Rune's tree does this on macOS, but it is a
//! path dependency on a repo that would have to be vendored, and this needs three numbers
//! rather than a watcher thread and a JSON endpoint.
//!
//! The numbers come from the kernel, not from the allocator. jemalloc's `stats.allocated`
//! would be the more precise figure — it counts live allocations rather than pages — but the
//! system jemalloc on this machine is built without `--enable-stats`, so `mallctl` answers
//! EINVAL for every counter and returns zero. Measuring RSS is coarser but it is true.

/// One reading of the process.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Sample {
    /// Resident set size, in bytes: what the kernel has actually given the process, freed-but-
    /// unreleased pages included. This is the number a leak shows up in.
    pub(crate) rss: u64,
    /// High-water RSS over this process's life. RSS falling back to a lower value does not
    /// lower this, which is what makes it the one to compare across iterations of a benchmark.
    pub(crate) rss_peak: u64,
    /// User plus system CPU seconds over the whole process.
    pub(crate) cpu_seconds: f64,
    /// Live threads.
    pub(crate) threads: u32,
}

#[cfg(all(target_os = "linux", target_env = "gnu"))]
mod imp {
    use super::Sample;
    use std::ffi::c_int;

    pub(super) fn sample() -> Option<Sample> {
        let status = std::fs::read_to_string("/proc/self/status").ok()?;
        let mut sample = Sample::default();
        for line in status.lines() {
            if let Some(rest) = line.strip_prefix("VmRSS:") {
                sample.rss = parse_kb(rest) * 1024;
            } else if let Some(rest) = line.strip_prefix("VmHWM:") {
                // Peak resident set, which the kernel tracks without any help from us.
                sample.rss_peak = parse_kb(rest) * 1024;
            } else if let Some(rest) = line.strip_prefix("Threads:") {
                sample.threads = rest.trim().parse().unwrap_or(0);
            }
        }
        let stat_line = std::fs::read_to_string("/proc/self/stat").ok()?;
        // Fields 14 and 15 (1-based) are utime and stime, in clock ticks. The comm field can
        // contain spaces and parentheses, so counting starts after its closing bracket.
        if let Some(after_comm) = stat_line.rsplit_once(')').map(|(_, rest)| rest) {
            let fields: Vec<&str> = after_comm.split_whitespace().collect();
            // After the comm, `after_comm` starts at field 3, so utime is index 11 and stime 12.
            if fields.len() > 12 {
                let ticks: u64 = fields[11].parse().unwrap_or(0) + fields[12].parse().unwrap_or(0);
                // `sysconf(_SC_CLK_TCK)` is 100 on every Linux this targets, but read it rather
                // than assume, since a wrong divisor silently reports the wrong CPU.
                let hz = clock_ticks().max(1);
                sample.cpu_seconds = ticks as f64 / hz as f64;
            }
        }
        Some(sample)
    }

    fn clock_ticks() -> u64 {
        unsafe extern "C" {
            fn sysconf(name: c_int) -> std::os::raw::c_long;
        }
        const SC_CLK_TCK: c_int = 2;
        let value = unsafe { sysconf(SC_CLK_TCK) };
        if value > 0 { value as u64 } else { 100 }
    }

    fn parse_kb(rest: &str) -> u64 {
        rest.split_whitespace()
            .next()
            .and_then(|value| value.parse().ok())
            .unwrap_or(0)
    }
}

#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
mod imp {
    use super::Sample;
    pub(super) fn sample() -> Option<Sample> {
        None
    }
}

/// This process's memory and CPU now, or `None` where it cannot be read.
pub(crate) fn sample() -> Option<Sample> {
    imp::sample()
}

/// A one-line summary, for the log a benchmark prints.
pub(crate) fn summary(sample: &Sample) -> String {
    format!(
        "rss {:.1} MiB (peak {:.1}), cpu {:.2}s, {} threads",
        mib(sample.rss),
        mib(sample.rss_peak),
        sample.cpu_seconds,
        sample.threads,
    )
}

pub(crate) fn mib(bytes: u64) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The numbers have to be plausible, or every benchmark built on them is fiction.
    #[test]
    fn a_sample_reads_plausible_numbers() {
        let Some(sample) = sample() else {
            eprintln!("skipping: this platform has no /proc");
            return;
        };
        assert!(sample.rss > 0, "a running process has resident memory");
        assert!(
            sample.rss_peak >= sample.rss,
            "peak {} is below current {}",
            sample.rss_peak,
            sample.rss
        );
        assert!(sample.threads > 0);
        assert!(sample.cpu_seconds >= 0.);
        eprintln!("{}", summary(&sample));
    }

    /// RSS tracks the heap both ways: a leak check built on a counter that does not move would
    /// pass every test it was given.
    #[test]
    fn allocation_moves_rss_and_freeing_moves_it_back() {
        let Some(before) = sample() else { return };
        let block: Vec<u8> = vec![7; 64 * 1024 * 1024];
        let during = sample().expect("sample after allocating");
        std::hint::black_box(&block);
        drop(block);
        let after = sample().expect("sample after freeing");
        assert!(
            during.rss > before.rss + 32 * 1024 * 1024,
            "64 MiB did not show in RSS: before {:.1} MiB, during {:.1} MiB",
            mib(before.rss),
            mib(during.rss)
        );
        assert!(
            after.rss < during.rss,
            "freeing 64 MiB did not lower RSS: during {:.1} MiB, after {:.1} MiB",
            mib(during.rss),
            mib(after.rss)
        );
    }
}
