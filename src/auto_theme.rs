//! 系统深浅色自动切换：按日出日落或自定义时间，在切换点把 Windows 应用与系统主题改一次。
//!
//! - 日出日落在本机按坐标计算（NOAA 简化公式，误差约 1–2 分钟），不联网；
//!   坐标取天气城市，未设置城市或极昼极夜时改用自定义时间。
//! - 只在越过切换点时改一次：之后用户手动改回，岛仔不会反复覆盖，直到下一个切换点。
//! - 只写当前用户的 `AppsUseLightTheme` / `SystemUsesLightTheme`，与系统设置里的开关是同一处。

use chrono::{DateTime, Duration, NaiveDate, TimeZone};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode { #[default] Off, Sun, Custom }

impl Mode {
    pub fn key(self) -> &'static str { match self { Self::Off => "off", Self::Sun => "sun", Self::Custom => "custom" } }
    pub fn parse(value: &str) -> Self { match value { "sun" => Self::Sun, "custom" => Self::Custom, _ => Self::Off } }
}

/// 计划使用的时间来源；`Custom` 也可能是日出日落模式下缺坐标或极昼极夜时的回退。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source { Sun, Custom }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Plan {
    /// 当前应处于深色。
    pub dark: bool,
    /// 最近一次切换点（Unix 秒）；大于已处理的切换点时才需要动手。
    pub since: i64,
    /// 下一次切换：时间与切换后的深浅。
    pub next: Option<(i64, bool)>,
    pub source: Source,
}

pub const DEFAULT_LIGHT: u32 = 7 * 60;
pub const DEFAULT_DARK: u32 = 19 * 60;

/// 某个 UTC 日期的日出、日落时刻（Unix 秒）；极昼或极夜返回 `None`。
pub fn sun_times(lat: f64, lon: f64, date: NaiveDate) -> Option<(i64, i64)> {
    use chrono::Datelike;
    use std::f64::consts::PI;
    if !(lat.is_finite() && lon.is_finite()) || lat.abs() > 90.0 { return None; }
    let days = if date.leap_year() { 366.0 } else { 365.0 };
    let g = 2.0 * PI / days * (date.ordinal0() as f64);
    let eqtime = 229.18 * (0.000075 + 0.001868 * g.cos() - 0.032077 * g.sin() - 0.014615 * (2.0 * g).cos() - 0.040849 * (2.0 * g).sin());
    let decl = 0.006918 - 0.399912 * g.cos() + 0.070257 * g.sin() - 0.006758 * (2.0 * g).cos() + 0.000907 * (2.0 * g).sin()
        - 0.002697 * (3.0 * g).cos() + 0.00148 * (3.0 * g).sin();
    let lat = lat.to_radians();
    // 90.833° 含大气折射与太阳视半径。
    let cos_ha = 90.833f64.to_radians().cos() / (lat.cos() * decl.cos()) - lat.tan() * decl.tan();
    if !(-1.0..=1.0).contains(&cos_ha) { return None; }
    let ha = cos_ha.acos().to_degrees();
    let midnight = date.and_hms_opt(0, 0, 0)?.and_utc().timestamp();
    let at = |minutes: f64| midnight + (minutes * 60.0).round() as i64;
    Some((at(720.0 - 4.0 * (lon + ha) - eqtime), at(720.0 - 4.0 * (lon - ha) - eqtime)))
}

/// 前后各一天的切换点，按时间排序：(时刻, 切换后是否深色)。
fn sun_events<Tz: TimeZone>(lat: f64, lon: f64, now: &DateTime<Tz>) -> Option<Vec<(i64, bool)>> {
    let today = now.naive_utc().date();
    let mut events = Vec::new();
    for offset in -2..=2 {
        let (rise, set) = sun_times(lat, lon, today + Duration::days(offset))?;
        events.extend([(rise, false), (set, true)]);
    }
    events.sort_unstable();
    Some(events)
}

fn custom_events<Tz: TimeZone>(light_min: u32, dark_min: u32, now: &DateTime<Tz>) -> Vec<(i64, bool)> {
    let (light_min, dark_min) = (light_min.min(1439), dark_min.min(1439));
    if light_min == dark_min { return Vec::new(); }
    let today = now.naive_local().date();
    let mut events = Vec::new();
    for offset in -1..=1 {
        let day = today + Duration::days(offset);
        for (minutes, dark) in [(light_min, false), (dark_min, true)] {
            let Some(local) = day.and_hms_opt(minutes / 60, minutes % 60, 0) else { continue };
            // 夏令时跳过的那一刻没有对应时间，当天就不切换。
            if let Some(at) = now.timezone().from_local_datetime(&local).earliest() { events.push((at.timestamp(), dark)); }
        }
    }
    events.sort_unstable();
    events
}

fn pick(events: &[(i64, bool)], now: i64, source: Source) -> Option<Plan> {
    let last = events.iter().rposition(|(at, _)| *at <= now)?;
    let (since, dark) = events[last];
    Some(Plan { dark, since, next: events.get(last + 1).copied(), source })
}

/// 计算此刻应有的深浅色；关闭时返回 `None`。
pub fn plan<Tz: TimeZone>(mode: Mode, position: Option<(f64, f64)>, light_min: u32, dark_min: u32, now: DateTime<Tz>) -> Option<Plan> {
    let ts = now.timestamp();
    let sun = match (mode, position) {
        (Mode::Off, _) => return None,
        (Mode::Sun, Some((lat, lon))) => sun_events(lat, lon, &now).and_then(|events| pick(&events, ts, Source::Sun)),
        _ => None,
    };
    sun.or_else(|| pick(&custom_events(light_min, dark_min, &now), ts, Source::Custom))
}

/// 在后台线程里把系统切到指定深浅；已是目标状态时不写注册表、不广播。
#[cfg(windows)]
pub fn apply(dark: bool) {
    let _ = std::thread::Builder::new().name("daozai-auto-theme".into()).spawn(move || unsafe {
        use windows::{core::{w, PCWSTR}, Win32::{Foundation::{LPARAM, WPARAM}, System::Registry::*, UI::WindowsAndMessaging::*}};
        let key = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize");
        let light = u32::from(!dark);
        let mut changed = false;
        for name in [w!("AppsUseLightTheme"), w!("SystemUsesLightTheme")] {
            let (mut value, mut size) = (u32::MAX, 4u32);
            let read = RegGetValueW(HKEY_CURRENT_USER, key, name, RRF_RT_REG_DWORD, None, Some((&mut value as *mut u32).cast()), Some(&mut size));
            if read.is_ok() && value == light { continue; }
            let written = RegSetKeyValueW(HKEY_CURRENT_USER, key, name, REG_DWORD.0, Some((&light as *const u32).cast()), 4);
            changed |= written.is_ok();
        }
        if changed {
            let topic: PCWSTR = w!("ImmersiveColorSet");
            let _ = SendMessageTimeoutW(HWND_BROADCAST, WM_SETTINGCHANGE, WPARAM(0), LPARAM(topic.as_ptr() as isize), SMTO_ABORTIFHUNG, 2000, None);
        }
    });
}

#[cfg(not(windows))]
pub fn apply(_dark: bool) {}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{FixedOffset, NaiveDate, TimeZone};

    fn tz(hours: i32) -> FixedOffset { FixedOffset::east_opt(hours * 3600).unwrap() }
    fn hhmm(at: i64, offset: &FixedOffset) -> String { offset.timestamp_opt(at, 0).unwrap().format("%H:%M").to_string() }
    fn minutes(text: &str) -> i64 { let (h, m) = text.split_once(':').unwrap(); h.parse::<i64>().unwrap() * 60 + m.parse::<i64>().unwrap() }
    fn near(actual: &str, expected: &str) { assert!((minutes(actual) - minutes(expected)).abs() <= 3, "{actual} 与 {expected} 相差超过 3 分钟"); }

    #[test]
    fn sun_times_match_published_almanac_values() {
        // 北京夏至、上海冬至、伦敦春分（当地时间，天文台公布值）。
        let cases = [
            (39.9042, 116.4074, (2026, 6, 21), 8, "04:46", "19:46"),
            (31.2304, 121.4737, (2026, 12, 21), 8, "06:50", "16:54"),
            (51.5074, -0.1278, (2026, 3, 20), 0, "06:02", "18:14"),
        ];
        for (lat, lon, (y, m, d), offset, rise, set) in cases {
            let (r, s) = sun_times(lat, lon, NaiveDate::from_ymd_opt(y, m, d).unwrap()).unwrap();
            near(&hhmm(r, &tz(offset)), rise);
            near(&hhmm(s, &tz(offset)), set);
        }
    }

    #[test]
    fn polar_day_and_night_have_no_sun_times() {
        assert!(sun_times(69.65, 18.96, NaiveDate::from_ymd_opt(2026, 12, 21).unwrap()).is_none());
        assert!(sun_times(69.65, 18.96, NaiveDate::from_ymd_opt(2026, 6, 21).unwrap()).is_none());
        assert!(sun_times(f64::NAN, 0.0, NaiveDate::from_ymd_opt(2026, 6, 21).unwrap()).is_none());
    }

    #[test]
    fn plan_follows_sun_through_the_day_independent_of_machine_timezone() {
        let beijing = Some((39.9042, 116.4074));
        let at = |h, m| tz(8).with_ymd_and_hms(2026, 6, 21, h, m, 0).unwrap();
        let night = plan(Mode::Sun, beijing, DEFAULT_LIGHT, DEFAULT_DARK, at(2, 0)).unwrap();
        assert!(night.dark && night.source == Source::Sun);
        near(&hhmm(night.since, &tz(8)), "19:46");
        let (next, next_dark) = night.next.unwrap();
        assert!(!next_dark);
        near(&hhmm(next, &tz(8)), "04:46");
        let noon = plan(Mode::Sun, beijing, DEFAULT_LIGHT, DEFAULT_DARK, at(12, 0)).unwrap();
        assert!(!noon.dark && noon.since == next);
        let evening = plan(Mode::Sun, beijing, DEFAULT_LIGHT, DEFAULT_DARK, at(21, 0)).unwrap();
        assert!(evening.dark && evening.since > noon.since);
        // 同一时刻换一个时区表示，结果完全相同。
        let utc = plan(Mode::Sun, beijing, DEFAULT_LIGHT, DEFAULT_DARK, at(21, 0).with_timezone(&chrono::Utc)).unwrap();
        assert_eq!(utc, evening);
    }

    #[test]
    fn custom_times_and_fallbacks() {
        let at = |h, m| tz(8).with_ymd_and_hms(2026, 11, 13, h, m, 0).unwrap();
        let morning = plan(Mode::Custom, Some((39.9, 116.4)), 7 * 60, 19 * 60, at(6, 59)).unwrap();
        assert!(morning.dark && morning.source == Source::Custom);
        assert_eq!(hhmm(morning.since, &tz(8)), "19:00");
        assert_eq!(morning.next.map(|(t, d)| (hhmm(t, &tz(8)), d)), Some(("07:00".into(), false)));
        let day = plan(Mode::Custom, None, 7 * 60, 19 * 60, at(7, 0)).unwrap();
        assert!(!day.dark && hhmm(day.since, &tz(8)) == "07:00");
        // 深色时间早于浅色时间（夜班）同样成立。
        let swapped = plan(Mode::Custom, None, 20 * 60, 8 * 60, at(12, 0)).unwrap();
        assert!(swapped.dark && hhmm(swapped.since, &tz(8)) == "08:00");
        // 日出日落缺坐标或极夜时回退到自定义时间。
        assert_eq!(plan(Mode::Sun, None, 7 * 60, 19 * 60, at(12, 0)).unwrap().source, Source::Custom);
        let polar = tz(1).with_ymd_and_hms(2026, 12, 21, 12, 0, 0).unwrap();
        assert_eq!(plan(Mode::Sun, Some((69.65, 18.96)), 7 * 60, 19 * 60, polar).unwrap().source, Source::Custom);
        assert!(plan(Mode::Off, Some((39.9, 116.4)), 7 * 60, 19 * 60, at(12, 0)).is_none());
        assert!(plan(Mode::Custom, None, 600, 600, at(12, 0)).is_none(), "两个时间相同时不切换");
    }

    #[test]
    fn mode_keys_round_trip() {
        for mode in [Mode::Off, Mode::Sun, Mode::Custom] { assert_eq!(Mode::parse(mode.key()), mode); }
        assert_eq!(Mode::parse("unknown"), Mode::Off);
    }
}
