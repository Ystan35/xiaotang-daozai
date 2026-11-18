//! Ask: questions answered by Google Gemini (AI Studio API key).
//! The key lives in Windows Credential Manager; history is a small JSON file
//! that prunes itself after the configured number of days.
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver};
use std::time::{SystemTime, UNIX_EPOCH};

pub const MODELS: [(&str, &str); 2] = [("gemini-flash-latest", "Flash"), ("gemini-flash-lite-latest", "Flash-Lite")];
pub const KEEP_DAYS: [u32; 4] = [0, 1, 3, 7];
pub const KEY_URL: &str = "https://aistudio.google.com/apikey";
const SYSTEM: &str = "You are a helpful assistant inside a desktop widget. Give complete, accurate answers: as long as the question needs, but no padding. Use short paragraphs or bullet lists where they help. Plain text only: no markdown headings, tables or bold.";
const CONTEXT_TURNS: usize = 8;
/// Thinking models spend part of this budget before they write, so keep it generous.
const MAX_OUTPUT_TOKENS: u32 = 8192;
const MAX_HISTORY: usize = 200;

pub fn now_secs() -> u64 { SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0) }

pub fn model_label(model: &str) -> &'static str {
    MODELS.iter().find(|(id, _)| *id == model).map(|(_, l)| *l).unwrap_or("Flash")
}

// ---------- key storage ----------
#[cfg(windows)]
fn credential() -> Result<keyring::Entry, String> { keyring::Entry::new("Daozai.Ask", "gemini").map_err(|_| "Can't open Windows Credential Manager".into()) }
#[cfg(not(windows))]
static MEMORY_KEY: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

pub fn save_key(key: &str) -> Result<(), String> {
    let key = key.trim();
    if key.is_empty() { return Err("Paste a key first".into()); }
    if key.chars().any(|c| c.is_whitespace() || c.is_control()) { return Err("That doesn't look like an API key".into()); }
    #[cfg(windows)] { credential()?.set_password(key).map_err(|_| "Can't store the key securely".into()) }
    #[cfg(not(windows))] { *MEMORY_KEY.lock().unwrap() = Some(key.to_string()); Ok(()) }
}
pub fn load_key() -> Option<String> {
    #[cfg(windows)] { credential().ok()?.get_password().ok().filter(|k| !k.is_empty()) }
    #[cfg(not(windows))] { MEMORY_KEY.lock().unwrap().clone() }
}
pub fn delete_key() -> Result<(), String> {
    #[cfg(windows)] { match credential()?.delete_credential() { Ok(()) | Err(keyring::Error::NoEntry) => Ok(()), Err(_) => Err("Can't remove the key".into()) } }
    #[cfg(not(windows))] { *MEMORY_KEY.lock().unwrap() = None; Ok(()) }
}

// ---------- history ----------
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Entry {
    pub q: String,
    pub a: String,
    pub at: u64,
    pub thread: u64,
    #[serde(default)]
    pub error: bool,
    /// The question came with a picture (the picture itself is never saved).
    #[serde(default)]
    pub image: bool,
}

#[derive(Default, Debug, Clone)]
pub struct History {
    pub entries: Vec<Entry>,
    pub path: Option<PathBuf>,
}

impl History {
    pub fn path() -> PathBuf { crate::model::config_dir().join("ask-history.json") }
    pub fn load(keep_days: u32) -> Self {
        let path = Self::path();
        let entries = std::fs::read(&path).ok().and_then(|b| serde_json::from_slice::<Vec<Entry>>(&b).ok()).unwrap_or_default();
        let mut h = History { entries, path: Some(path) };
        if h.prune(keep_days, now_secs()) { h.save(keep_days); }
        h
    }
    /// Drops entries older than `keep_days`. Returns true if anything changed.
    pub fn prune(&mut self, keep_days: u32, now: u64) -> bool {
        let before = self.entries.len();
        if keep_days > 0 {
            let cutoff = now.saturating_sub(keep_days as u64 * 86_400);
            self.entries.retain(|e| e.at >= cutoff);
        }
        if self.entries.len() > MAX_HISTORY { let extra = self.entries.len() - MAX_HISTORY; self.entries.drain(..extra); }
        self.entries.len() != before
    }
    pub fn save(&self, keep_days: u32) {
        let Some(path) = &self.path else { return };
        if keep_days == 0 { let _ = std::fs::remove_file(path); return; }
        if let Some(dir) = path.parent() { let _ = std::fs::create_dir_all(dir); }
        let saved: Vec<&Entry> = self.entries.iter().filter(|e| !e.error).collect();
        if let Ok(text) = serde_json::to_vec_pretty(&saved) {
            let tmp = path.with_extension("json.tmp");
            if std::fs::write(&tmp, text).is_ok() { let _ = std::fs::rename(&tmp, path); }
        }
    }
    pub fn clear(&mut self) {
        self.entries.clear();
        if let Some(path) = &self.path { let _ = std::fs::remove_file(path); }
    }
    pub fn thread(&self, thread: u64) -> Vec<&Entry> { self.entries.iter().filter(|e| e.thread == thread).collect() }
    pub fn latest_thread(&self) -> Option<u64> { self.entries.last().map(|e| e.thread) }
}

// ---------- images ----------
/// A picture ready to send: re-encoded as JPEG and no larger than `MAX_EDGE`.
#[derive(Clone, Debug)]
pub struct Image { pub mime: &'static str, pub data: Vec<u8>, pub w: u32, pub h: u32 }
const MAX_EDGE: u32 = 1600;
const MAX_FILE: u64 = 25 << 20;

pub fn prepare_image(img: &crate::clipboard_image::Rgba) -> Option<Image> {
    if img.w == 0 || img.h == 0 || img.px.len() != img.w as usize * img.h as usize * 4 { return None; }
    let rgba = image::RgbaImage::from_raw(img.w, img.h, img.px.clone())?;
    let mut dynamic = image::DynamicImage::ImageRgba8(rgba);
    if img.w.max(img.h) > MAX_EDGE { dynamic = dynamic.resize(MAX_EDGE, MAX_EDGE, image::imageops::FilterType::Triangle); }
    // Flatten any transparency onto white so JPEG doesn't turn it black.
    let mut rgb = image::RgbImage::new(dynamic.width(), dynamic.height());
    for (x, y, p) in dynamic.to_rgba8().enumerate_pixels() {
        let a = p[3] as u32;
        let mix = |c: u8| ((c as u32 * a + 255 * (255 - a)) / 255) as u8;
        rgb.put_pixel(x, y, image::Rgb([mix(p[0]), mix(p[1]), mix(p[2])]));
    }
    let mut data = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut data, 85).encode_image(&rgb).ok()?;
    Some(Image { mime: "image/jpeg", data, w: rgb.width(), h: rgb.height() })
}

pub fn load_image_file(path: &std::path::Path) -> Result<crate::clipboard_image::Rgba, String> {
    let size = std::fs::metadata(path).map_err(|_| "Can't open that file".to_string())?.len();
    if size > MAX_FILE { return Err("That image is too large (25 MB max)".into()); }
    let img = image::open(path).map_err(|_| "That file isn't a PNG, JPEG or WebP image".to_string())?.to_rgba8();
    Ok(crate::clipboard_image::Rgba { w: img.width(), h: img.height(), px: img.into_raw() })
}

pub const IMAGE_ONLY_QUESTION: &str = "What's in this image?";

// ---------- request ----------
pub fn request_body(context: &[(String, String)], question: &str, image: Option<&Image>) -> serde_json::Value {
    let mut contents = Vec::new();
    let skip = context.len().saturating_sub(CONTEXT_TURNS);
    for (q, a) in &context[skip..] {
        contents.push(serde_json::json!({"role":"user","parts":[{"text":q}]}));
        contents.push(serde_json::json!({"role":"model","parts":[{"text":a}]}));
    }
    let mut parts = Vec::new();
    if let Some(img) = image {
        use base64::Engine;
        parts.push(serde_json::json!({"inline_data":{"mime_type":img.mime,"data":base64::engine::general_purpose::STANDARD.encode(&img.data)}}));
    }
    parts.push(serde_json::json!({"text":question}));
    contents.push(serde_json::json!({"role":"user","parts":parts}));
    serde_json::json!({
        "systemInstruction": {"parts":[{"text":SYSTEM}]},
        "contents": contents,
        "generationConfig": {"maxOutputTokens": MAX_OUTPUT_TOKENS, "temperature": 0.6}
    })
}

pub fn parse_answer(body: &serde_json::Value) -> Result<String, String> {
    if let Some(reason) = body.pointer("/promptFeedback/blockReason").and_then(|v| v.as_str()) {
        return Err(format!("Gemini declined to answer ({})", reason.to_lowercase().replace('_', " ")));
    }
    let parts = body.pointer("/candidates/0/content/parts").and_then(|v| v.as_array());
    let text: String = parts.map(|p| p.iter().filter(|p| !p.get("thought").and_then(|t| t.as_bool()).unwrap_or(false)).filter_map(|p| p.get("text").and_then(|t| t.as_str())).collect::<Vec<_>>().join("")).unwrap_or_default();
    let text = clean(&text);
    if text.is_empty() {
        let finish = body.pointer("/candidates/0/finishReason").and_then(|v| v.as_str()).unwrap_or("");
        return Err(if finish == "SAFETY" { "Gemini declined to answer (safety)".into() } else { "Gemini sent an empty answer".into() });
    }
    if body.pointer("/candidates/0/finishReason").and_then(|v| v.as_str()) == Some("MAX_TOKENS") { return Ok(format!("{text}…\n\n(Answer cut off at the length limit. Ask \"continue\" for the rest.)")); }
    Ok(text)
}

/// Strips the heaviest markdown so answers read cleanly in the small card.
pub fn clean(text: &str) -> String {
    text.lines()
        .map(|l| { let t = l.trim_end(); let t = t.trim_start_matches('#').trim_start_matches(' '); t.replace("**", "").replace("__", "") })
        .map(|l| if let Some(rest) = l.strip_prefix("* ") { format!("• {rest}") } else if let Some(rest) = l.strip_prefix("- ") { format!("• {rest}") } else { l })
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

pub fn error_message(status: u16, body: &str) -> String {
    let detail = serde_json::from_str::<serde_json::Value>(body).ok().and_then(|v| v.pointer("/error/message").and_then(|m| m.as_str()).map(str::to_string)).unwrap_or_default();
    match status {
        400 if detail.contains("API key") || detail.contains("API_KEY") => "That API key isn't valid. Check Settings > Ask".into(),
        400 if detail.to_lowercase().contains("location") => "Gemini isn't available in your region".into(),
        401 | 403 => "That API key isn't allowed to use Gemini. Check Settings > Ask".into(),
        404 => "That Gemini model isn't available. Pick another in Settings > Ask".into(),
        429 => "Gemini's free limit was reached. Try again in a minute".into(),
        500..=599 => "Gemini is busy right now. Try again shortly".into(),
        _ if !detail.is_empty() => detail.chars().take(120).collect(),
        _ => format!("Gemini request failed ({status})"),
    }
}

pub fn ask(model: &str, key: &str, context: &[(String, String)], question: &str, image: Option<&Image>) -> Result<String, String> {
    if cfg!(test) { return Err("Network is disabled in tests".into()); }
    let url = format!("https://generativelanguage.googleapis.com/v1beta/models/{model}:generateContent");
    let agent = crate::quota::http_agent_long(std::time::Duration::from_secs(if image.is_some() { 120 } else { 90 }));
    let body = request_body(context, question, image);
    match agent.post(&url).set("x-goog-api-key", key).set("Content-Type", "application/json").send_string(&body.to_string()) {
        Ok(resp) => {
            let text = resp.into_string().map_err(|_| "Couldn't read Gemini's answer".to_string())?;
            let json: serde_json::Value = serde_json::from_str(&text).map_err(|_| "Gemini sent an unreadable answer".to_string())?;
            parse_answer(&json)
        }
        Err(ureq::Error::Status(code, resp)) => Err(error_message(code, &resp.into_string().unwrap_or_default())),
        Err(_) => Err("Can't reach Gemini. Check your connection".into()),
    }
}

/// Runs a question on a background thread; the receiver yields once.
pub fn spawn(model: String, context: Vec<(String, String)>, question: String, image: Option<Image>, ctx: Option<eframe::egui::Context>) -> Receiver<Result<String, String>> {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let result = match load_key() {
            Some(key) => ask(&model, &key, &context, &question, image.as_ref()),
            None => Err("Add your Google AI Studio key in Settings > Ask".into()),
        };
        let _ = tx.send(result);
        if let Some(ctx) = ctx { ctx.request_repaint(); }
    });
    rx
}

#[cfg(test)]
mod tests {
    use super::*;
    fn e(q: &str, at: u64, thread: u64) -> Entry { Entry { q: q.into(), a: "a".into(), at, thread, error: false, image: false } }

    #[test]
    fn prune_drops_entries_older_than_the_window() {
        let now = 10 * 86_400;
        let mut h = History { entries: vec![e("old", now - 4 * 86_400, 1), e("new", now - 3600, 2)], path: None };
        assert!(h.prune(3, now));
        assert_eq!(h.entries.iter().map(|e| e.q.as_str()).collect::<Vec<_>>(), ["new"]);
        assert!(!h.prune(3, now));
    }

    #[test]
    fn history_round_trips_and_skips_errors_and_zero_days_deletes_file() {
        let dir = std::env::temp_dir().join(format!("daozai-ask-{}", std::process::id()));
        let path = dir.join("ask-history.json");
        let mut h = History { entries: vec![e("q", now_secs(), 1), Entry { error: true, ..e("bad", now_secs(), 1) }], path: Some(path.clone()) };
        h.save(3);
        let loaded: Vec<Entry> = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(loaded.len(), 1);
        h.save(0);
        assert!(!path.exists());
        h.save(3);
        h.clear();
        assert!(!path.exists() && h.entries.is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn long_answers_are_allowed_and_a_cut_off_answer_says_so() {
        let body = request_body(&[], "explain", None);
        assert!(body["generationConfig"]["maxOutputTokens"].as_u64().unwrap() >= 4096);
        let cut = serde_json::json!({"candidates":[{"content":{"parts":[{"text":"Part one"}]},"finishReason":"MAX_TOKENS"}]});
        let text = parse_answer(&cut).unwrap();
        assert!(text.starts_with("Part one") && text.contains("continue"));
    }

    #[test]
    fn request_body_keeps_recent_context_and_system_prompt() {
        let ctx: Vec<(String, String)> = (0..CONTEXT_TURNS + 2).map(|i| (format!("q{i}"), format!("a{i}"))).collect();
        let body = request_body(&ctx, "now?", None);
        let contents = body["contents"].as_array().unwrap();
        assert_eq!(contents.len(), CONTEXT_TURNS * 2 + 1);
        assert_eq!(contents[0]["parts"][0]["text"], "q2");
        assert_eq!(contents.last().unwrap()["parts"][0]["text"], "now?");
        assert!(body["systemInstruction"]["parts"][0]["text"].as_str().unwrap().contains("complete"));
    }

    #[test]
    fn parse_answer_joins_parts_skips_thoughts_and_reports_blocks() {
        let ok = serde_json::json!({"candidates":[{"content":{"parts":[{"text":"hmm","thought":true},{"text":"**Paris** is the "},{"text":"capital."}]}}]});
        assert_eq!(parse_answer(&ok).unwrap(), "Paris is the capital.");
        let blocked = serde_json::json!({"promptFeedback":{"blockReason":"SAFETY"}});
        assert!(parse_answer(&blocked).unwrap_err().contains("safety"));
        assert!(parse_answer(&serde_json::json!({"candidates":[]})).is_err());
    }

    #[test]
    fn images_are_downscaled_to_jpeg_and_sent_before_the_question() {
        let big = crate::clipboard_image::Rgba { w: 3200, h: 1000, px: vec![0u8; 3200 * 1000 * 4] };
        let img = prepare_image(&big).unwrap();
        assert_eq!((img.w, img.h, img.mime), (1600, 500, "image/jpeg"));
        assert_eq!(&img.data[..2], &[0xFF, 0xD8]);
        let decoded = image::load_from_memory(&img.data).unwrap().to_rgb8();
        assert!(decoded.get_pixel(10, 10)[0] > 240, "transparent pixels become white, not black");
        let body = request_body(&[("q".into(), "a".into())], "what is it?", Some(&img));
        let parts = body["contents"].as_array().unwrap().last().unwrap()["parts"].as_array().unwrap().clone();
        assert_eq!(parts[0]["inline_data"]["mime_type"], "image/jpeg");
        assert!(parts[0]["inline_data"]["data"].as_str().unwrap().len() > 100);
        assert_eq!(parts[1]["text"], "what is it?");
        assert!(prepare_image(&crate::clipboard_image::Rgba { w: 2, h: 2, px: vec![0; 3] }).is_none());
    }

    #[test]
    fn clean_turns_markdown_bullets_into_plain_dots() {
        assert_eq!(clean("## Title\n* one\n- **two**"), "Title\n• one\n• two");
    }

    #[test]
    fn error_messages_are_friendly() {
        assert!(error_message(400, r#"{"error":{"message":"API key not valid. Please pass a valid API key."}}"#).contains("isn't valid"));
        assert!(error_message(429, "").contains("limit"));
        assert!(error_message(503, "").contains("busy"));
        assert!(error_message(403, "").contains("allowed"));
    }

    #[test]
    fn key_validation_rejects_blank_and_spaced_keys() {
        assert!(save_key("  ").is_err());
        assert!(save_key("abc def").is_err());
    }
}
