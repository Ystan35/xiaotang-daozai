//! 岛上「Claude 账号」轮换视图与设置卡片。
use crate::{app::App,claude_tracker::{self,Status},extras::{setting_row,settings_card},paint::{Card,H},theme::{self,W}};
use eframe::egui::{self,Color32};

pub(crate) const ROWS:usize=4;
const ORANGE:Color32=Color32::from_rgb(255,166,77);

/// 恢复时间：今天只显示时刻，其余带「明天」或日期。
pub(crate) fn reset_label(at:i64,now:i64)->String{
    use chrono::TimeZone;
    let (Some(at),Some(now))=(chrono::Local.timestamp_opt(at,0).single(),chrono::Local.timestamp_opt(now,0).single()) else{return String::new();};
    let days=(at.date_naive()-now.date_naive()).num_days();
    match days{0=>at.format("%H:%M").to_string(),1=>format!("Tomorrow {}",at.format("%H:%M")),_=>at.format("%m/%d %H:%M").to_string()}
}

impl App {
    pub(crate) fn claude_tracker_view(&self)->bool{
        self.claude_tracker.enabled && (self.claude_tracker.view || self.quota.config.accounts.is_empty())
    }

    /// AI 额度页右下角的「Claude 账号」切换（顶栏中间是日期时间，放不下）；返回是否显示轮换视图。
    pub(crate) fn claude_tracker_chip(&mut self,c:&Card)->bool{
        if !self.claude_tracker.enabled{return false;}
        let on=self.claude_tracker_view();
        let (x,y,w,h)=(CHIP_X,147.0,357.0-CHIP_X,18.0);
        let (r,b)=crate::paint::capsule(c,x,y,w,h,"claude-tracker-chip",if on{Color32::from_rgb(70,48,38)}else{theme::BTN},theme::BTN_HOVER);
        b.text(x+w/2.0,y+12.5,if on{"Back to quota"}else{"Claude accounts"},9.5,W::Semibold,if on{Color32::from_rgb(240,170,130)}else{theme::GRAY},H::Center);
        if r.on_hover_text(if on{"Back to AI quota"}else{"Claude account rotation"}).clicked(){
            self.claude_tracker.view=!on;self.claude_tracker.editing=None;
        }
        self.claude_tracker_view()
    }

    pub(crate) fn page_claude_tracker(&mut self,c:&Card){
        let now=chrono::Utc::now().timestamp();
        if self.claude_tracker.accounts.is_empty(){
            c.text(43.0,61.0,"No Claude accounts yet",13.0,W::Bold,theme::WHITE,H::Left);
            c.text(43.0,80.0,"Add accounts and pick their browser profiles",10.5,W::Regular,theme::GRAY,H::Left);
            let (r,b)=crate::paint::capsule(c,43.0,92.0,94.0,24.0,"add-claude-account",theme::BTN,theme::BTN_HOVER);
            b.text(90.0,108.0,"Add account",10.0,W::Semibold,theme::WHITE,H::Center);
            if r.clicked(){self.settings_tab=11;self.open_settings();}
            return;
        }
        let count=self.claude_tracker.accounts.len();
        // 滚轮逐行滚动，‹ › 一次翻一屏。
        let first=self.claude_tracker.first.min(count.saturating_sub(ROWS));self.claude_tracker.first=first;
        let mut action:Option<(String,&'static str,i64)>=None;
        for (row,a) in self.claude_tracker.accounts.iter().enumerate().skip(first).take(ROWS){
            let y=36.0+(row-first) as f32*29.0;let cy=y+12.0;
            let status=a.status(now);
            let editing=self.claude_tracker.editing.as_deref()==Some(a.id.as_str());
            c.circle(58.0,cy,3.5,if status==Status::Available{theme::GREEN}else{ORANGE});
            let tip=a.profile.as_ref().map(|p|p.title()).unwrap_or_else(||"Default browser".into());
            let name_w=if editing{66.0}else{98.0};
            c.text(68.0,cy+4.0,&c.fit(&a.name,11.0,W::Semibold,name_w),11.0,W::Semibold,theme::WHITE,H::Left);
            c.hit(68.0,y,name_w,24.0,("claude-name",&a.id)).on_hover_text(tip);
            let button=|x:f32,w:f32,label:&str,key:&str|{
                let (r,b)=crate::paint::capsule(c,x,y+2.0,w,20.0,&format!("claude-{key}-{}",a.id),theme::BTN,theme::BTN_HOVER);
                b.text(x+w/2.0,cy+3.5,label,9.5,W::Semibold,theme::WHITE,H::Center);r.clicked()
            };
            if editing{
                let at=a.reset_at.unwrap_or(now);
                c.text(140.0,cy+4.0,&reset_label(at,now),10.5,W::Semibold,ORANGE,H::Left);
                for (x,w,label,key,delta) in [(184.0,26.0,"-1h","minus-hour",-3600),(212.0,20.0,"−","minus",-900),(234.0,20.0,"+","plus",900),(256.0,26.0,"+1h","plus-hour",3600)]{
                    if button(x,w,label,key){action=Some((a.id.clone(),"adjust",delta));}
                }
                if button(286.0,30.0,"Ready","clear"){action=Some((a.id.clone(),"clear",0));}
                if button(320.0,30.0,"Done","done"){action=Some((a.id.clone(),"done",0));}
            }else{
                let (text,color)=match status{Status::Available=>("Ready".to_string(),theme::GREEN),Status::Limited(at)=>(format!("back {}",reset_label(at,now)),ORANGE)};
                c.text(172.0,cy+4.0,&c.fit(&text,10.5,W::Regular,86.0),10.5,W::Regular,color,H::Left);
                let (label,key)=if status==Status::Available{("Used up","limit")}else{("Adjust","edit")};
                if button(266.0,40.0,label,key){action=Some((a.id.clone(),key_action(key),0));}
                if button(310.0,40.0,"Open","open"){action=Some((a.id.clone(),"open",0));}
            }
        }
        if count>ROWS{
            let last=count-ROWS;
            c.text(200.0,161.0,&format!("{}–{} / {count} · scroll",first+1,first+ROWS),10.0,W::Regular,theme::DIM,H::Center);
            for (x,label,step) in [(132.0,"‹",-1isize),(268.0,"›",1)]{
                if c.hit(x-8.0,150.0,18.0,16.0,("claude-page",step)).clicked(){
                    self.claude_tracker.first=if step<0{if first==0{last}else{first.saturating_sub(ROWS)}}else if first>=last{0}else{(first+ROWS).min(last)};
                }
                c.text(x+1.0,162.0,label,13.0,W::Regular,theme::GRAY,H::Center);
            }
        }
        let Some((id,kind,delta))=action else{return;};
        let tracker=&mut self.claude_tracker;
        match kind{
            "limit"=>tracker.mark_limited(&id,now),
            "edit"=>tracker.editing=Some(id.clone()),
            "adjust"=>tracker.adjust(&id,delta,now),
            "clear"=>tracker.clear(&id),
            "done"=>tracker.editing=None,
            "open"=>{
                let profile=tracker.find(&id).and_then(|a|a.profile.clone());
                if let Err(e)=claude_tracker::open(profile.as_ref()){self.say(e);}
                return;
            }
            _=>{}
        }
        self.claude_tracker.save();
    }

    pub(crate) fn claude_tracker_settings(&mut self,ui:&mut egui::Ui){
        ui.add_space(10.0);
        let enabled=self.claude_tracker.enabled;
        settings_card(ui,|ui|{
            setting_row(ui,"Claude account rotation","When you hit a limit, note when it resets on the island and get reminded; Open launches claude.ai in that account's browser profile",&mut self.claude_tracker.enabled);
            if !self.claude_tracker.enabled{return;}
            if self.claude_tracker.profiles.is_empty(){self.claude_tracker.refresh_profiles();}
            ui.separator();
            let now=chrono::Utc::now().timestamp();
            let profiles=self.claude_tracker.profiles.clone();
            let mut remove=None;
            for a in &mut self.claude_tracker.accounts{
                let id=a.id.clone();
                ui.push_id(id,|ui|{
                    ui.horizontal(|ui|{
                        ui.add_sized([110.0,24.0],egui::TextEdit::singleline(&mut a.name).hint_text("Account name").char_limit(24));
                        let selected=a.profile.as_ref().map(|p|p.title()).unwrap_or_else(||"Default browser".into());
                        egui::ComboBox::from_id_salt("profile").width(210.0).selected_text(selected).show_ui(ui,|ui|{
                            ui.selectable_value(&mut a.profile,None,"Default browser");
                            for p in &profiles{ui.selectable_value(&mut a.profile,Some(p.clone()),p.title());}
                        });
                        let status=match a.status(now){Status::Available=>"Ready".to_string(),Status::Limited(at)=>format!("back {}",reset_label(at,now))};
                        ui.label(egui::RichText::new(status).size(11.0).color(ui.visuals().weak_text_color()));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center),|ui|{
                            if ui.button("Remove").clicked(){remove=Some(a.id.clone());}
                            if a.reset_at.is_some() && ui.button("Mark ready").clicked(){a.reset_at=None;}
                        });
                    });
                });
            }
            if let Some(id)=remove{self.claude_tracker.remove(&id);}
            ui.horizontal_wrapped(|ui|{
                let full=self.claude_tracker.accounts.len()>=claude_tracker::MAX_ACCOUNTS;
                if ui.add_enabled(!full,egui::Button::new("＋ Add Claude account")).clicked(){self.claude_tracker.add("",now);}
                if ui.button("Re-detect browser profiles").clicked(){self.claude_tracker.refresh_profiles();}
            });
            let hint=if profiles.is_empty(){"No Chrome or Edge profiles found; accounts open in the default browser."}else{"Pick the browser profile signed in to each Claude account. Daozai only reads profile names, never chats, cookies or sign-ins, and doesn't go online."};
            ui.add(egui::Label::new(egui::RichText::new(hint).size(11.0).color(ui.visuals().weak_text_color())).wrap());
        });
        if enabled!=self.claude_tracker.enabled{self.claude_tracker.view=self.claude_tracker.enabled;self.claude_tracker.editing=None;}
    }

    /// 到点的账号恢复可用并提醒；每 5 秒检查一次。
    pub(crate) fn claude_tracker_tick(&mut self){
        if !self.claude_tracker.enabled||self.t-self.claude_tracker_checked<5.0{return;}
        self.claude_tracker_checked=self.t;
        let names=self.claude_tracker.take_due(chrono::Utc::now().timestamp());
        if names.is_empty(){return;}
        self.claude_tracker.save();
        self.alert("Claude account ready",format!("{} can be used again",names.join(", ")),crate::app::QUOTA_PAGE,true);
    }
}

/// 右下角切换按钮的左边界；额度页底部的更新时间不能越过它。
pub(crate) const CHIP_X:f32=287.0;

fn key_action(key:&str)->&'static str{if key=="limit"{"limit"}else{"edit"}}

#[cfg(test)]
mod tests {
    use super::reset_label;
    #[test]
    fn reset_labels_name_today_tomorrow_and_later_days(){
        use chrono::TimeZone;
        let now=chrono::Local.with_ymd_and_hms(2026,11,13,10,0,0).single().unwrap().timestamp();
        assert_eq!(reset_label(now+5*3600,now),"15:00");
        assert_eq!(reset_label(now+20*3600,now),"明天 06:00");
        assert_eq!(reset_label(now+3*86400,now),"11/16 10:00");
    }
}
