//! CPU and memory load for the resident bar, sampled with two cheap system calls.

pub const INTERVAL: f64 = 1.5;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Memory { pub percent: f32, pub used: u64, pub total: u64 }

#[derive(Default, Debug)]
pub struct Sampler {
    /// (idle, total) CPU ticks from the previous sample.
    last: Option<(u64, u64)>,
    pub cpu: Option<f32>,
    pub memory: Option<Memory>,
    at: f64,
}

/// CPU busy share between two (idle, total) readings, 0–100.
pub fn cpu_percent(prev: (u64, u64), cur: (u64, u64)) -> Option<f32> {
    let total = cur.1.checked_sub(prev.1)?;
    let idle = cur.0.checked_sub(prev.0)?;
    if total == 0 { return None; }
    Some((100.0 * (1.0 - idle.min(total) as f64 / total as f64)) as f32)
}

pub fn gb(bytes: u64) -> String { format!("{:.1}", bytes as f64 / (1u64 << 30) as f64) }

impl Sampler {
    /// Samples at most every `INTERVAL` seconds; returns true when values changed.
    pub fn tick(&mut self, t: f64) -> bool {
        if self.cpu.is_some() && t - self.at < INTERVAL { return false; }
        self.at = t;
        if let Some(cur) = cpu_ticks() {
            if let Some(prev) = self.last { if let Some(p) = cpu_percent(prev, cur) { self.cpu = Some(p); } }
            self.last = Some(cur);
        }
        self.memory = memory();
        true
    }
    /// Test hook.
    pub fn set(&mut self, cpu: f32, memory: Memory) { self.cpu = Some(cpu); self.memory = Some(memory); self.at = f64::INFINITY; }
    pub fn ready(&self) -> bool { self.cpu.is_some() || self.memory.is_some() }
}

#[cfg(windows)]
fn cpu_ticks() -> Option<(u64, u64)> {
    use windows::Win32::Foundation::FILETIME;
    use windows::Win32::System::Threading::GetSystemTimes;
    let (mut idle, mut kernel, mut user) = (FILETIME::default(), FILETIME::default(), FILETIME::default());
    unsafe { GetSystemTimes(Some(&mut idle), Some(&mut kernel), Some(&mut user)).ok()?; }
    let v = |f: FILETIME| ((f.dwHighDateTime as u64) << 32) | f.dwLowDateTime as u64;
    // Kernel time already includes idle time.
    Some((v(idle), v(kernel) + v(user)))
}
#[cfg(not(windows))]
fn cpu_ticks() -> Option<(u64, u64)> {
    let text = std::fs::read_to_string("/proc/stat").ok()?;
    let nums: Vec<u64> = text.lines().next()?.split_whitespace().skip(1).filter_map(|n| n.parse().ok()).collect();
    if nums.len() < 4 { return None; }
    Some((nums[3] + nums.get(4).copied().unwrap_or(0), nums.iter().sum()))
}

#[cfg(windows)]
fn memory() -> Option<Memory> {
    use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    let mut status = MEMORYSTATUSEX { dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32, ..Default::default() };
    unsafe { GlobalMemoryStatusEx(&mut status).ok()?; }
    let used = status.ullTotalPhys.saturating_sub(status.ullAvailPhys);
    Some(Memory { percent: status.dwMemoryLoad as f32, used, total: status.ullTotalPhys })
}
#[cfg(not(windows))]
fn memory() -> Option<Memory> {
    let text = std::fs::read_to_string("/proc/meminfo").ok()?;
    let field = |name: &str| text.lines().find(|l| l.starts_with(name))?.split_whitespace().nth(1)?.parse::<u64>().ok().map(|kb| kb * 1024);
    let (total, avail) = (field("MemTotal:")?, field("MemAvailable:")?);
    let used = total.saturating_sub(avail);
    Some(Memory { percent: used as f32 * 100.0 / total.max(1) as f32, used, total })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cpu_percent_uses_tick_deltas_and_rejects_bad_readings() {
        assert_eq!(cpu_percent((100, 1000), (150, 1200)), Some(75.0));
        assert_eq!(cpu_percent((100, 1000), (300, 1200)), Some(0.0));
        assert_eq!(cpu_percent((100, 1000), (100, 1000)), None);
        assert_eq!(cpu_percent((100, 1000), (50, 900)), None, "counter went backwards");
        assert_eq!(gb(17_179_869_184), "16.0");
    }
    #[test]
    fn sampler_waits_for_the_interval_and_gets_cpu_on_the_second_sample() {
        let mut s = Sampler::default();
        assert!(s.tick(0.0));
        assert!(s.memory.is_some_and(|m| m.total > 0 && m.percent >= 0.0 && m.percent <= 100.0));
        std::thread::sleep(std::time::Duration::from_millis(80));
        assert!(s.tick(0.1), "no CPU value yet, so sample again");
        assert!(s.cpu.is_some_and(|c| (0.0..=100.0).contains(&c)));
        assert!(!s.tick(0.5));
        assert!(s.tick(2.0));
    }
}
