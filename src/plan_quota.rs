//! Read-only subscription meters. Protocol references (no real-account verification):
//! OpenCode: anomalyco/opencode d21b0790a6c146815f28fea604e41e922f009590,
//! packages/console/app/src/routes/zen/go/v1/usage.ts.
//! MiniMax: MiniMax-AI/cli 38aba5f26173dd1065b115b221d79c9ddd87c6bb,
//! src/{client/endpoints,config/schema,types/api,utils/quota,output/quota-table}.ts.
//! GLM: zai-org/zai-coding-plugins, plugins/glm-plan-usage/skills/
//! usage-query-skill/scripts/query-usage.mjs (official usage plugin).
use super::{Snapshot, Window};
use serde_json::Value;
use std::io::Read;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlanProvider { OpenCodeGo, MiniMaxChina, MiniMaxGlobal, GlmChina, GlmGlobal }
impl PlanProvider {
    pub fn endpoint(self) -> &'static str {
        match self {
            Self::OpenCodeGo => "https://opencode.ai/zen/go/v1/usage",
            Self::MiniMaxChina => "https://api.minimax.cn/v1/token_plan/remains",
            Self::MiniMaxGlobal => "https://api.minimax.io/v1/token_plan/remains",
            Self::GlmChina => "https://open.bigmodel.cn/api/monitor/usage/quota/limit",
            Self::GlmGlobal => "https://api.z.ai/api/monitor/usage/quota/limit",
        }
    }
    fn authorization(self, key: &str) -> String {
        match self { Self::GlmChina | Self::GlmGlobal => key.into(), _ => format!("Bearer {key}") }
    }
}

const MAX_BODY: u64 = 1024 * 1024;
/// Caller must supply quota::http_agent(): bounded timeout, redirects disabled,
/// user's configured proxy preserved. Never follow a region redirect with a key.
pub fn request(agent: &ureq::Agent, provider: PlanProvider, key: &str) -> Result<Snapshot, (String, u64)> {
    let key = key.trim();
    if key.is_empty() || key.len() > 8192 || key.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(("Enter a valid plan API key".into(), 0));
    }
    let glm=matches!(provider,PlanProvider::GlmChina|PlanProvider::GlmGlobal);
    let response = agent.get(provider.endpoint())
        .set("Authorization", &provider.authorization(key)).set("Accept", "application/json")
        .call().or_else(|e| match e {
            ureq::Error::Status(_,response) if glm=>Ok(response),
            ureq::Error::Status(s, _) => Err(status_error(s)),
            _ => Err((if glm{"GLM quota network error; check network, proxy and region, then retry"}else{"Plan quota network request failed; try again later"}.into(), 30)),
        })?;
    let status=response.status();
    if !(200..300).contains(&status)&&(!glm||(300..400).contains(&status)){return Err(status_error(status));}
    let mut body = Vec::new();
    response.into_reader().take(MAX_BODY + 1).read_to_end(&mut body)
        .map_err(|_| ("Can't read the plan quota response".into(), 30))?;
    if body.len() as u64 > MAX_BODY { return Err(("Plan quota response too large".into(), 30)); }
    let value = serde_json::from_slice(&body);
    if glm&&!(200..300).contains(&status){return Err(glm_error(value.as_ref().unwrap_or(&Value::Null),Some(status)));}
    let value=value.map_err(|_| ("Plan quota response isn't valid JSON".into(), 30))?;
    parse(provider, &value).map_err(|e| (e, if glm{glm_error(&value,None).1}else{30}))
}
fn glm_error(v:&Value,http:Option<u16>)->(String,u64){
    let numeric=|v:&Value|v.as_u64().or_else(||v.as_str()?.parse::<u64>().ok());
    let code=v.pointer("/error/code").and_then(numeric).or_else(||v.get("code").and_then(numeric));
    let reason=v["msg"].as_str().or_else(||v["message"].as_str()).or_else(||v.pointer("/error/message").and_then(Value::as_str)).unwrap_or_default().to_ascii_lowercase();
    // HTTP 429 也可能表示套餐到期，先按业务码区分；服务端原文不进入界面或缓存。
    let (message,retry)=match code{
        Some(1003)=>("GLM key expired; get a new plan key from the console for your region",0),
        Some(401|1000|1001)=>("GLM key authentication failed; check the key, its validity and region",0),
        Some(1005)=>("GLM account needs extra verification; finish it in the console for your region and retry",60),
        Some(403|1220)=>("No permission for GLM quota; check region and Coding Plan key",60),
        Some(1309|1314)=>("GLM plan expired or inactive; check its status in the console",60),
        Some(1315)=>("Wrong GLM key type; use this region's Coding Plan key",60),
        Some(1311)=>("GLM plan lacks permission; check plan benefits in the console",60),
        Some(1113)=>("GLM account is in arrears; check account status in the console",60),
        Some(429|1302)=>("Too many GLM quota requests; try again later",180),
        Some(1305)=>("GLM service is busy; try again later",180),
        Some(1308|1310|1316..=1321)=>("GLM plan quota used up; see the console for reset time",180),
        _ if ["不存在coding plan","不存在 coding plan","no coding plan","coding plan not found","未开通coding plan","套餐不存在"].iter().any(|s|reason.contains(s))=>("No active GLM Coding Plan found; make sure it's active and use the plan key",60),
        _ if reason.contains("token expired")||reason.contains("invalid api key")=>("GLM key invalid or expired; get a new plan key and check the region",0),
        _=>return match http{
            Some(401)=>("GLM key authentication failed; check the key, its validity and region".into(),0),
            Some(403)=>("GLM quota query refused (403); check account permissions, region and network access".into(),60),
            Some(429)=>("Too many GLM quota requests; try again later".into(),180),
            Some(status)=>(format!("GLM quota service unavailable (HTTP {status}); try again later"),30),
            None=>match code.filter(|c|*c<10000){Some(code)=>(format!("GLM quota query failed (code {code}); check plan status in the console"),30),None=>("GLM quota query failed; check the plan key and status in the console".into(),30)},
        },
    };(message.into(),retry)
}
fn status_error(status: u16) -> (String, u64) {
    match status {
        401 => ("Plan key authentication failed (401); check key and region".into(), 0),
        403 => ("Plan quota query refused (403); check plan permissions and region".into(), 60),
        429 => ("Too many plan quota requests; try again later".into(), 180),
        300..=399 => ("The plan service redirected; stopped to protect the key. Check the region".into(), 0),
        _ => (format!("Plan quota service unavailable ({status})"), 30),
    }
}
fn number(v: &Value) -> Option<f64> {
    v.as_f64().or_else(|| v.as_str()?.parse().ok()).filter(|n| n.is_finite() && *n >= 0.0)
}
fn percent(v: &Value) -> Option<f32> { number(v).filter(|n| *n <= 100.0).map(|n| n as f32) }
fn millis(v: &Value) -> Option<i64> {
    number(v).filter(|n| *n >= 1_000_000_000_000.0 && *n <= 253_402_300_799_000.0).map(|n| (n / 1000.0) as i64)
}
fn iso(v: &Value) -> Option<i64> { chrono::DateTime::parse_from_rfc3339(v.as_str()?).ok().map(|d| d.timestamp()) }
fn window(label: &str, seconds: Option<i64>, remaining: f32, reset: Option<i64>) -> Window {
    Window { label: label.into(), seconds, remaining, reset ,amount:None}
}
pub fn parse(provider: PlanProvider, v: &Value) -> Result<Snapshot, String> {
    let mut snapshot = Snapshot { at: chrono::Utc::now().timestamp(), ..Default::default() };
    match provider {
        PlanProvider::OpenCodeGo => parse_opencode(v, &mut snapshot)?,
        PlanProvider::MiniMaxChina | PlanProvider::MiniMaxGlobal => parse_minimax(v, &mut snapshot)?,
        PlanProvider::GlmChina | PlanProvider::GlmGlobal => parse_glm(v, &mut snapshot)?,
    }
    if snapshot.windows.is_empty() && snapshot.extra.is_empty() {
        return Err("No recognizable limited plan quota returned; unknown or unlimited quota isn't shown as zero".into());
    }
    Ok(snapshot)
}
fn parse_opencode(v: &Value, s: &mut Snapshot) -> Result<(), String> {
    if !v["error"].is_null() { return Err("OpenCode Go returned no plan quota".into()); }
    s.plan = "Subscription quota".into();
    // The API does not publish rolling duration. Keep provider ordering stable
    // in Snapshot::rings rather than sorting a known week ahead of rolling.
    for (key, label, duration) in [("rolling", "Rolling", None), ("weekly", "This week", None), ("monthly", "This month", None)] {
        let row = &v["usage"][key];
        if let Some(p) = percent(&row["percent"]) {
            s.windows.push(window(label, duration, 100.0 - p, iso(&row["resetsAt"])));
        }
    }
    Ok(())
}
fn minimax_percent(row: &Value, prefix: &str) -> Option<f32> {
    let status = &row[format!("{prefix}_status")];
    // Explicit unlimited/unknown states cannot be represented by a finite ring.
    if !status.is_null() && !matches!(status.as_u64(), Some(1 | 2)) { return None; }
    let explicit = &row[format!("{prefix}_remaining_percent")];
    if !explicit.is_null() { return percent(explicit); }
    // Legacy protocol: usage_count is remaining, NOT used. Zero total is unknown.
    let total = number(&row[format!("{prefix}_total_count")]).filter(|n| *n > 0.0)?;
    let remaining = number(&row[format!("{prefix}_usage_count")]).filter(|n| *n <= total)?;
    Some((remaining / total * 100.0) as f32)
}
fn parse_minimax(v: &Value, s: &mut Snapshot) -> Result<(), String> {
    if let Some(code) = v.pointer("/base_resp/status_code") {
        if number(code) != Some(0.0) { return Err("MiniMax returned no plan quota; check plan key and region".into()); }
    }
    s.plan = "Coding plan".into();
    let rows = v["model_remains"].as_array().ok_or("MiniMax quota response has no model list")?;
    // General is the authoritative shared pool in current plans. Do not sum
    // overlapping model pools or substitute unrelated video/image quotas.
    let row = rows.iter().find(|r| r["model_name"].as_str() == Some("general"))
        .or_else(|| rows.iter().find(|r| r["model_name"].as_str() == Some("MiniMax-M*")))
        .ok_or("MiniMax returned no recognizable coding plan quota")?;
    for (prefix, start, end, label) in [
        ("current_interval", "start_time", "end_time", "Current window"),
        ("current_weekly", "weekly_start_time", "weekly_end_time", "This week"),
    ] {
        if let Some(mut p) = minimax_percent(row, prefix) {
            if prefix == "current_weekly" && !row["weekly_boost_permille"].is_null() {
                let boost = number(&row["weekly_boost_permille"]).filter(|n| *n > 0.0 && *n <= 10000.0)
                    .ok_or("MiniMax weekly quota multiplier invalid")?;
                p *= (boost / 1000.0) as f32;
            }
            let reset = millis(&row[end]);
            let duration = millis(&row[start]).zip(reset).and_then(|(a,b)| b.checked_sub(a)).filter(|d| *d > 0);
            s.windows.push(window(label, duration, p, reset));
        }
    }
    Ok(())
}
fn parse_glm(v: &Value, s: &mut Snapshot) -> Result<(), String> {
    if !v["error"].is_null() || v["success"].as_bool() == Some(false) || (!v["code"].is_null() && number(&v["code"]) != Some(200.0) && number(&v["code"]) != Some(0.0)) {
        return Err(glm_error(v,None).0);
    }
    let data = if v["data"].is_object() { &v["data"] } else { v };
    let rows = data["limits"].as_array().ok_or("GLM quota response has no limit list")?;
    if rows.len()>16{return Err("GLM returned too many quota windows; check response version".into());}
    if !rows.iter().any(|row|matches!(row["type"].as_str(),Some("TOKENS_LIMIT"|"CREDIT_LIMIT")) && percent(&row["percentage"]).is_some()){
        return Err("GLM returned no recognizable coding quota (tool quota doesn't count)".into());
    }
    // 复用原 ZCode 已支持的周期和重置时间解析，保留两种登录来源的相同口径。
    *s=super::zcode::coding(&serde_json::json!({"data":data}))?;
    if let Some(level)=data["level"].as_str().map(str::trim).filter(|v|!v.is_empty()&&v.len()<=64){s.plan=level.into();}
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test] fn glm_business_errors_are_specific_without_echoing_private_messages(){
        for provider in [PlanProvider::GlmChina,PlanProvider::GlmGlobal]{
            for (code,message,expected) in [
                (401,"token expired or incorrect synthetic-secret","密钥"),
                (1003,"synthetic-secret","已过期"),
                (1220,"synthetic-secret","权限"),
                (1309,"synthetic-secret","套餐已到期"),
                (1315,"synthetic-secret","套餐 Key"),
                (1302,"synthetic-secret","频繁"),
                (500,"用户不存在coding plan synthetic-secret","未查询到有效"),
                (5555,"synthetic-secret https://private.invalid","5555"),
            ]{
                let error=parse(provider,&json!({"success":false,"code":code,"msg":message})).unwrap_err();
                assert!(error.contains(expected),"{code}: {error}");
                assert!(!error.contains("synthetic-secret")&&!error.contains("private.invalid"));
            }
        }
    }
    #[test] fn glm_http_and_business_errors_preserve_their_meaning_and_retry_delay(){
        for (status,body,expected,delay) in [
            (429,r#"{"error":{"code":"1309","message":"synthetic-secret"}}"#,"套餐已到期",60),
            (401,r#"{"error":{"code":"1003","message":"synthetic-secret"}}"#,"已过期",0),
            (200,r#"{"success":false,"code":1302,"msg":"synthetic-secret"}"#,"频繁",180),
            (429,"<html>synthetic-secret</html>","频繁",180),
            (503,"<html>synthetic-secret</html>","503",30),
        ]{
            let count=std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));let calls=count.clone();
            let agent=ureq::AgentBuilder::new().try_proxy_from_env(false).redirects(0).middleware(move|request:ureq::Request,_:ureq::MiddlewareNext|{
                calls.fetch_add(1,std::sync::atomic::Ordering::Relaxed);
                assert_eq!(request.url(),PlanProvider::GlmChina.endpoint());assert_eq!(request.header("Authorization"),Some("synthetic-glm-key"));
                ureq::Response::new(status,"Test",body)
            }).build();
            let (error,retry)=request(&agent,PlanProvider::GlmChina,"synthetic-glm-key").unwrap_err();
            assert!(error.contains(expected),"{status}: {error}");assert_eq!(retry,delay);
            assert!(!error.contains("synthetic-secret")&&!error.contains("synthetic-glm-key"));assert_eq!(count.load(std::sync::atomic::Ordering::Relaxed),1);
        }
    }
    #[test] fn glm_pr46_response_keeps_plan_periods_and_rejects_failed_payloads(){
        let v=json!({"code":200,"success":true,"data":{"level":"lite","limits":[
            {"type":"CREDIT_LIMIT","unit":3,"number":5,"percentage":46,"nextResetTime":1793892721234i64},
            {"type":"CREDIT_LIMIT","unit":6,"number":1,"percentage":10,"nextResetTime":1794366124964i64}]}});
        let s=parse(PlanProvider::GlmChina,&v).unwrap();assert_eq!(s.plan,"lite");assert_eq!(s.windows.len(),2);
        assert_eq!(s.windows[0].remaining,54.0);assert_eq!(s.windows[0].reset,Some(1793892721));assert_eq!(s.windows[1].remaining,90.0);
        let mut rejected=v.clone();rejected["success"]=json!(false);assert!(parse(PlanProvider::GlmChina,&rejected).is_err());
        rejected=v;rejected["error"]=json!({"code":"1003","message":"synthetic-secret"});assert!(parse(PlanProvider::GlmChina,&rejected).is_err());
    }
    #[test] fn opencode_percentage_and_iso_reset() {
        let s = parse(PlanProvider::OpenCodeGo, &json!({"usage":{"rolling":{"percent":25,"resetsAt":"2030-01-01T00:00:00Z"},"weekly":{"percent":100},"monthly":{"percent":0}}})).unwrap();
        assert_eq!(s.windows.len(),3); assert_eq!(s.windows[0].remaining,75.0); assert_eq!(s.windows[0].reset,Some(1893456000)); assert_eq!(s.windows[1].remaining,0.0);
    }
    #[test] fn absent_invalid_or_error_data_never_becomes_zero() {
        for provider in [PlanProvider::OpenCodeGo, PlanProvider::MiniMaxChina, PlanProvider::GlmGlobal] {
            for v in [json!({}),json!({"error":{"message":"SECRET"}}),json!({"usage":{"rolling":{"percent":"NaN"}}})] { assert!(parse(provider,&v).is_err()); }
        }
        assert!(parse(PlanProvider::OpenCodeGo,&json!({"usage":{"rolling":{"percent":101}}})).is_err());
    }
    #[test] fn minimax_legacy_remaining_and_epoch_milliseconds() {
        let s = parse(PlanProvider::MiniMaxChina,&json!({"base_resp":{"status_code":0},"model_remains":[{"model_name":"MiniMax-M*","current_interval_total_count":100,"current_interval_usage_count":75,"start_time":1900000000000i64,"end_time":1900018000000i64}]})).unwrap();
        assert_eq!(s.windows[0].remaining,75.0); assert_eq!(s.windows[0].seconds,Some(18000)); assert_eq!(s.windows[0].reset,Some(1900018000));
    }
    #[test] fn minimax_prefers_explicit_percent_and_general_pool() {
        let s = parse(PlanProvider::MiniMaxGlobal,&json!({"model_remains":[{"model_name":"MiniMax-M*","current_interval_total_count":100,"current_interval_usage_count":90},{"model_name":"general","current_interval_total_count":0,"current_interval_usage_count":0,"current_interval_remaining_percent":40,"current_weekly_remaining_percent":80,"weekly_boost_permille":1500}]})).unwrap();
        assert_eq!(s.windows[0].remaining,40.0); assert_eq!(s.windows[1].remaining,120.0);
    }
    #[test] fn minimax_unknown_unlimited_and_zero_total_not_finite_quota() {
        for row in [json!({"model_name":"general","current_interval_total_count":0,"current_interval_usage_count":0}),json!({"model_name":"general","current_interval_status":3,"current_interval_remaining_percent":100}),json!({"model_name":"video","current_interval_remaining_percent":100})] {
            assert!(parse(PlanProvider::MiniMaxChina,&json!({"model_remains":[row]})).is_err());
        }
    }
    #[test] fn glm_used_percentage_windows_and_tools_are_separate() {
        let s = parse(PlanProvider::GlmChina,&json!({"success":true,"code":200,"data":{"limits":[{"type":"TOKENS_LIMIT","unit":3,"number":5,"percentage":20,"nextResetTime":1900000000000i64},{"type":"CREDIT_LIMIT","unit":6,"number":1,"percentage":75},{"type":"TIME_LIMIT","unit":5,"number":1,"percentage":50},{"type":"FUTURE_LIMIT","percentage":0}]}})).unwrap();
        assert_eq!(s.windows.len(),2); assert_eq!(s.extra.len(),1); assert_eq!(s.windows[0].remaining,80.0); assert_eq!(s.windows[0].reset,Some(1900000000)); assert_eq!(s.windows[0].seconds,Some(18000)); assert_eq!(s.windows[1].remaining,25.0);
        assert!(parse(PlanProvider::GlmChina,&json!({"data":{"limits":[{"type":"TOKENS_LIMIT"}]}})).is_err());
    }
    #[test] fn opencode_unknown_rolling_cadence_preserves_primary_order() {
        let s = parse(PlanProvider::OpenCodeGo, &json!({"usage":{"rolling":{"percent":10},"weekly":{"percent":20},"monthly":{"percent":30}}})).unwrap();
        assert_eq!(s.rings().iter().map(|w| w.label.as_str()).collect::<Vec<_>>(), vec!["滚动", "本周"]);
        assert!(s.windows.iter().all(|w| w.seconds.is_none()));
    }
    #[test] fn glm_tools_only_cannot_connect_as_blank_coding_account() {
        assert!(parse(PlanProvider::GlmGlobal, &json!({"data":{"limits":[{"type":"TIME_LIMIT","percentage":10}]}})).is_err());
    }
    #[test] fn authorization_regions_and_errors_are_fixed() {
        assert_eq!(PlanProvider::GlmChina.authorization("synthetic"),"synthetic");
        assert_eq!(PlanProvider::OpenCodeGo.authorization("synthetic"),"Bearer synthetic");
        assert!(PlanProvider::MiniMaxChina.endpoint().starts_with("https://api.minimax.cn/"));
        assert!(PlanProvider::MiniMaxGlobal.endpoint().starts_with("https://api.minimax.io/"));
        assert_eq!(status_error(302).1,0); assert_eq!(status_error(429).1,180);
        assert!(!parse(PlanProvider::MiniMaxChina,&json!({"base_resp":{"status_code":1004,"status_msg":"SECRET"}})).unwrap_err().contains("SECRET"));
    }
}
