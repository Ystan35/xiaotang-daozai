//! Claude 多账号轮换：用户在额度用完时手动记下恢复时间，到点提醒；点账号用对应的浏览器资料打开 claude.ai。
//!
//! - 不读取聊天、登录信息或浏览器缓存，也不联网；恢复时间完全由用户在岛上标记。
//! - 浏览器资料只读取 Chrome / Edge 的 `Local State` 里的资料名称和目录名，用于显示与打开。
//! - 记录保存在本机 `claude-tracker.json`。

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Claude 免费版的会话窗口：首条消息起约 5 小时恢复。
pub const WINDOW_SECS: i64 = 5 * 3600;
/// 恢复时间最远可调到 7 天后（周额度）。
pub const MAX_AHEAD_SECS: i64 = 7 * 24 * 3600;
pub const MAX_ACCOUNTS: usize = 12;
pub const URL: &str = "https://claude.ai/new";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Browser { Chrome, Edge }

impl Browser {
    pub const ALL: [Self; 2] = [Self::Chrome, Self::Edge];
    pub fn name(self) -> &'static str { match self { Self::Chrome => "Chrome", Self::Edge => "Edge" } }
    pub fn exe(self) -> &'static str { match self { Self::Chrome => "chrome.exe", Self::Edge => "msedge.exe" } }
    /// `%LOCALAPPDATA%` 下的用户数据目录。
    pub fn data_dir(self) -> &'static str { match self { Self::Chrome => "Google/Chrome/User Data", Self::Edge => "Microsoft/Edge/User Data" } }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Profile {
    pub browser: Browser,
    /// 资料目录名，如 `Default`、`Profile 1`。
    pub dir: String,
    /// 浏览器里显示的资料名称（可能带邮箱）。
    pub label: String,
}

impl Profile {
    pub fn title(&self) -> String { format!("{} · {}", self.browser.name(), self.label) }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Account {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub profile: Option<Profile>,
    /// 用户标记的恢复时间（Unix 秒）；`None` 表示可用。
    #[serde(default)]
    pub reset_at: Option<i64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status { Available, Limited(i64) }

impl Account {
    pub fn status(&self, now: i64) -> Status {
        match self.reset_at { Some(at) if at > now => Status::Limited(at), _ => Status::Available }
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Tracker {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub accounts: Vec<Account>,
    /// 岛上 AI 额度页正在显示账号轮换。
    #[serde(skip)]
    pub view: bool,
    /// 岛上正在调整恢复时间的账号。
    #[serde(skip)]
    pub editing: Option<String>,
    /// 岛上列表的首行（超过一屏时翻页）。
    #[serde(skip)]
    pub first: usize,
    /// 本机识别到的浏览器资料。
    #[serde(skip)]
    pub profiles: Vec<Profile>,
    #[serde(skip)]
    path: Option<PathBuf>,
}

/// 把时刻向上取整到 5 分钟，便于和 Claude 显示的整点时间对齐。
fn round_up(at: i64) -> i64 { (at + 299).div_euclid(300) * 300 }

impl Tracker {
    fn path() -> PathBuf { crate::model::config_dir().join("claude-tracker.json") }

    pub fn load() -> Self {
        let path = Self::path();
        let mut tracker: Self = std::fs::read(&path).ok().and_then(|raw| serde_json::from_slice(&raw).ok()).unwrap_or_default();
        tracker.accounts.truncate(MAX_ACCOUNTS);
        tracker.path = Some(path);
        tracker
    }

    pub fn save(&self) {
        let Some(path) = &self.path else { return };
        if let Some(parent) = path.parent() { let _ = std::fs::create_dir_all(parent); }
        if let Ok(raw) = serde_json::to_vec_pretty(self) { let _ = crate::model::atomic_write(path, &raw); }
    }

    pub fn find(&self, id: &str) -> Option<&Account> { self.accounts.iter().find(|a| a.id == id) }
    fn find_mut(&mut self, id: &str) -> Option<&mut Account> { self.accounts.iter_mut().find(|a| a.id == id) }

    /// 新增账号；名称留空时按序号命名。达到上限返回 `None`。
    pub fn add(&mut self, name: &str, now: i64) -> Option<String> {
        if self.accounts.len() >= MAX_ACCOUNTS { return None; }
        let mut n = 0u32;
        let id = loop {
            let id = format!("claude-{now}-{n}");
            if self.find(&id).is_none() { break id; }
            n += 1;
        };
        let name = name.trim();
        let name = if name.is_empty() { format!("Claude {}", self.accounts.len() + 1) } else { name.chars().take(24).collect() };
        // 新账号默认绑定下一个还没被占用的浏览器资料。
        let profile = self.profiles.iter().find(|p| !self.accounts.iter().any(|a| a.profile.as_ref() == Some(*p))).cloned();
        self.accounts.push(Account { id: id.clone(), name, profile, reset_at: None });
        Some(id)
    }

    pub fn remove(&mut self, id: &str) {
        self.accounts.retain(|a| a.id != id);
        if self.editing.as_deref() == Some(id) { self.editing = None; }
        self.first = self.first.min(self.accounts.len().saturating_sub(1));
    }

    /// 额度用完：默认 5 小时后恢复，并进入调整状态。
    pub fn mark_limited(&mut self, id: &str, now: i64) {
        if let Some(a) = self.find_mut(id) { a.reset_at = Some(round_up(now + WINDOW_SECS)); }
        self.editing = Some(id.into());
    }

    /// 调整恢复时间；不早于 1 分钟后，不晚于 7 天后。
    pub fn adjust(&mut self, id: &str, delta: i64, now: i64) {
        if let Some(a) = self.find_mut(id) {
            let base = a.reset_at.filter(|at| *at > now).unwrap_or_else(|| round_up(now + WINDOW_SECS));
            a.reset_at = Some((base + delta).clamp(now + 60, now + MAX_AHEAD_SECS));
        }
    }

    pub fn clear(&mut self, id: &str) {
        if let Some(a) = self.find_mut(id) { a.reset_at = None; }
        if self.editing.as_deref() == Some(id) { self.editing = None; }
    }

    /// 已到恢复时间的账号：清除标记并返回名称，用于提醒。
    pub fn take_due(&mut self, now: i64) -> Vec<String> {
        let mut names = Vec::new();
        for a in &mut self.accounts {
            if a.reset_at.is_some_and(|at| at <= now) { a.reset_at = None; names.push(a.name.clone()); }
        }
        names
    }

    pub fn refresh_profiles(&mut self) { self.profiles = discover_profiles(); }
}

/// 从浏览器 `Local State` 中读取资料目录与名称；不读取 Cookie、历史或缓存。
pub fn parse_local_state(raw: &[u8], browser: Browser) -> Vec<Profile> {
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(raw) else { return Vec::new() };
    let Some(cache) = value["profile"]["info_cache"].as_object() else { return Vec::new() };
    let mut profiles: Vec<Profile> = cache.iter().filter(|(dir, _)| safe_dir(dir)).map(|(dir, info)| {
        let name = info["name"].as_str().unwrap_or_default().trim();
        let email = info["user_name"].as_str().unwrap_or_default().trim();
        let label = match (name.is_empty(), email.is_empty()) {
            (false, false) if name != email => format!("{name}（{email}）"),
            (false, _) => name.to_string(),
            (true, false) => email.to_string(),
            (true, true) => dir.clone(),
        };
        Profile { browser, dir: dir.clone(), label: label.chars().filter(|c| !c.is_control()).take(80).collect() }
    }).collect();
    // `Default` 在前，其余按资料编号排序（Profile 2 在 Profile 10 之前）。
    let key = |dir: &str| if dir == "Default" { (0, 0, String::new()) } else {
        dir.strip_prefix("Profile ").and_then(|n| n.parse::<u32>().ok()).map_or((2, 0, dir.to_string()), |n| (1, n, String::new()))
    };
    profiles.sort_by_key(|p| key(&p.dir));
    profiles
}

/// 资料目录只接受字母、数字、空格、`-` 与 `_`，避免拼进命令行时被解释。
pub fn safe_dir(dir: &str) -> bool {
    !dir.is_empty() && dir.len() <= 64 && dir.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '-' | '_'))
}

/// 打开 claude.ai 的程序与参数；未绑定资料时交给默认浏览器。
pub fn launch_command(profile: Option<&Profile>) -> Option<(String, String)> {
    match profile {
        None => Some((URL.to_string(), String::new())),
        Some(p) if safe_dir(&p.dir) => Some((p.browser.exe().to_string(), format!("--profile-directory=\"{}\" {URL}", p.dir))),
        Some(_) => None,
    }
}

#[cfg(windows)]
pub fn discover_profiles() -> Vec<Profile> {
    let Some(local) = std::env::var_os("LOCALAPPDATA") else { return Vec::new() };
    Browser::ALL.into_iter().flat_map(|browser| {
        let path = std::path::Path::new(&local).join(browser.data_dir()).join("Local State");
        std::fs::read(path).map(|raw| parse_local_state(&raw, browser)).unwrap_or_default()
    }).collect()
}

#[cfg(not(windows))]
pub fn discover_profiles() -> Vec<Profile> { Vec::new() }

#[cfg(windows)]
pub fn open(profile: Option<&Profile>) -> Result<(), String> {
    use windows::{core::{w, PCWSTR}, Win32::UI::{Shell::ShellExecuteW, WindowsAndMessaging::SW_SHOWNORMAL}};
    let (file, params) = launch_command(profile).ok_or("浏览器资料名称无效，请在设置中重新选择")?;
    let wide = |s: &str| s.encode_utf16().chain(Some(0)).collect::<Vec<_>>();
    let (file_w, params_w) = (wide(&file), wide(&params));
    let params = if params.is_empty() { PCWSTR::null() } else { PCWSTR(params_w.as_ptr()) };
    // ShellExecute 会按「应用路径」注册表找到 chrome.exe / msedge.exe，无需知道安装位置。
    let result = unsafe { ShellExecuteW(None, w!("open"), PCWSTR(file_w.as_ptr()), params, PCWSTR::null(), SW_SHOWNORMAL) };
    if result.0 as isize <= 32 {
        return Err(match profile { Some(p) => format!("无法启动 {}，请确认已安装", p.browser.name()), None => "无法打开默认浏览器".into() });
    }
    Ok(())
}

#[cfg(not(windows))]
pub fn open(_profile: Option<&Profile>) -> Result<(), String> { Err("打开浏览器暂仅支持 Windows".into()) }

#[cfg(test)]
mod tests {
    use super::*;

    fn tracker() -> Tracker { Tracker::default() }

    #[test]
    fn local_state_lists_profiles_in_browser_order_without_other_fields() {
        let raw = serde_json::json!({"profile":{"info_cache":{
            "Profile 10":{"name":"学校","user_name":"school@example.com","gaia_id":"x"},
            "Default":{"name":"个人","user_name":""},
            "Profile 2":{"name":"","user_name":"spare@example.com"},
            "Guest Profile":{"name":"访客"},
            "bad\"dir":{"name":"注入"},
        }},"os_crypt":{"encrypted_key":"secret"}}).to_string();
        let profiles = parse_local_state(raw.as_bytes(), Browser::Chrome);
        let dirs: Vec<_> = profiles.iter().map(|p| p.dir.as_str()).collect();
        assert_eq!(dirs, ["Default", "Profile 2", "Profile 10", "Guest Profile"], "非法目录名被跳过");
        assert_eq!(profiles[0].label, "个人");
        assert_eq!(profiles[1].label, "spare@example.com");
        assert_eq!(profiles[2].label, "学校（school@example.com）");
        assert_eq!(profiles[2].title(), "Chrome · 学校（school@example.com）");
        assert!(parse_local_state(b"not json", Browser::Edge).is_empty());
    }

    #[test]
    fn launch_command_quotes_profile_and_rejects_unsafe_names() {
        let profile = Profile { browser: Browser::Edge, dir: "Profile 1".into(), label: "x".into() };
        assert_eq!(launch_command(Some(&profile)), Some(("msedge.exe".into(), format!("--profile-directory=\"Profile 1\" {URL}"))));
        assert_eq!(launch_command(None), Some((URL.into(), String::new())));
        let bad = Profile { dir: "x\" --remote-debugging-port=9222".into(), ..profile };
        assert_eq!(launch_command(Some(&bad)), None);
    }

    #[test]
    fn marking_adjusting_and_due_accounts() {
        let mut t = tracker();
        t.profiles = vec![
            Profile { browser: Browser::Chrome, dir: "Profile 1".into(), label: "A".into() },
            Profile { browser: Browser::Chrome, dir: "Profile 10".into(), label: "B".into() },
        ];
        let now = 1_700_000_000;
        let a = t.add("主号", now).unwrap();
        let b = t.add("  ", now).unwrap();
        assert_eq!(t.find(&b).unwrap().name, "Claude 2");
        assert_eq!(t.find(&a).unwrap().profile.as_ref().unwrap().dir, "Profile 1");
        assert_eq!(t.find(&b).unwrap().profile.as_ref().unwrap().dir, "Profile 10", "新账号绑定未占用的资料");
        assert_eq!(t.find(&a).unwrap().status(now), Status::Available);

        t.mark_limited(&a, now);
        let reset = t.find(&a).unwrap().reset_at.unwrap();
        assert!(reset >= now + WINDOW_SECS && reset < now + WINDOW_SECS + 300 && reset % 300 == 0);
        assert_eq!(t.editing.as_deref(), Some(a.as_str()));
        t.adjust(&a, -3600, now);
        assert_eq!(t.find(&a).unwrap().reset_at, Some(reset - 3600));
        t.adjust(&a, -10 * 3600, now);
        assert_eq!(t.find(&a).unwrap().reset_at, Some(now + 60), "不早于 1 分钟后");
        t.adjust(&a, 30 * 24 * 3600, now);
        assert_eq!(t.find(&a).unwrap().reset_at, Some(now + MAX_AHEAD_SECS));
        t.adjust(&a, -(MAX_AHEAD_SECS - 3600), now);

        assert!(t.take_due(now + 3599).is_empty());
        assert_eq!(t.take_due(now + 3600), vec!["主号".to_string()]);
        assert_eq!(t.find(&a).unwrap().status(now + 3600), Status::Available);
        assert!(t.take_due(now + 7200).is_empty(), "只提醒一次");

        t.mark_limited(&b, now);
        t.clear(&b);
        assert_eq!(t.find(&b).unwrap().reset_at, None);
        assert_eq!(t.editing, None);
        t.remove(&a);
        assert!(t.find(&a).is_none() && t.accounts.len() == 1);
    }

    #[test]
    fn account_limit_and_json_round_trip_keeps_only_saved_fields() {
        let mut t = tracker();
        for i in 0..MAX_ACCOUNTS { assert!(t.add(&format!("号{i}"), 1).is_some()); }
        assert!(t.add("多余", 1).is_none());
        t.enabled = true;
        t.view = true;
        t.accounts[0].reset_at = Some(42);
        let raw = serde_json::to_vec(&t).unwrap();
        let back: Tracker = serde_json::from_slice(&raw).unwrap();
        assert!(back.enabled && !back.view);
        assert_eq!(back.accounts, t.accounts);
        let old: Tracker = serde_json::from_slice(br#"{"accounts":[{"id":"x","name":"y"}]}"#).unwrap();
        assert!(!old.enabled && old.accounts[0].reset_at.is_none() && old.accounts[0].profile.is_none());
    }
}
