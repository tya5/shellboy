//! Shared human labels only. Numeric/source time facts are never rewritten.
pub const TIME_TEXT: [(&str,&str);4] = [
    ("ui.time.seconds","秒"),("ui.time.minutes","分"),
    ("ui.time.hours","時間"),("ui.time.ended_unknown","終了時刻未取得"),
];

pub fn elapsed_text(ms:u64,tenths:bool)->String {
    let seconds=ms/1000;
    let [(_,sec),(_,min),(_,hour),_]=TIME_TEXT;
    if seconds<60 {
        if tenths {format!("{seconds}.{}{sec}",(ms%1000)/100)} else {format!("{seconds}{sec}")}
    } else if seconds<3600 {
        format!("{}{min}{}{sec}",seconds/60,seconds%60)
    } else {
        format!("{}{hour}{}{min}",seconds/3600,(seconds/60)%60)
    }
}

pub fn ended_text(at:Option<&chrono::DateTime<chrono::FixedOffset>>,now:chrono::DateTime<chrono::Utc>,offset:chrono::FixedOffset)->String {
    let Some(at)=at else {return TIME_TEXT[3].1.into();};
    let local=at.with_timezone(&offset);
    let today=now.with_timezone(&offset).date_naive();
    local.format(if local.date_naive()==today {"%H:%M"} else {"%Y-%m-%d %H:%M"}).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn elapsed_units_boundaries_fraction_and_large_values_are_shared() {
        for (ms,want) in [(0,"0秒"),(45000,"45秒"),(59999,"59秒"),(60000,"1分0秒"),(312000,"5分12秒"),(3599999,"59分59秒"),(3600000,"1時間0分"),(3780000,"1時間3分")] {assert_eq!(elapsed_text(ms,false),want);}
        assert_eq!(elapsed_text(400,true),"0.4秒");
        assert_eq!(elapsed_text(12400,true),"12.4秒");
        assert_eq!(elapsed_text(316000,true),"5分16秒");
        assert!(!elapsed_text(u64::MAX,false).is_empty());
        let keys=TIME_TEXT.iter().map(|x|x.0).collect::<std::collections::BTreeSet<_>>();assert_eq!(keys.len(),4);
    }
    #[test]
    fn saved_end_uses_viewer_date_not_observation_time() {
        let utc=chrono::DateTime::parse_from_rfc3339("2026-10-31T12:16:00Z").unwrap();
        let now=chrono::DateTime::parse_from_rfc3339("2026-10-31T14:00:00Z").unwrap().with_timezone(&chrono::Utc);
        let offset=chrono::FixedOffset::east_opt(9*3600).unwrap();
        assert_eq!(ended_text(Some(&utc),now,offset),"21:16");
        assert_eq!(ended_text(Some(&utc),now+chrono::Duration::hours(2),offset),"2026-10-31 21:16");
        assert_eq!(ended_text(None,now,offset),"終了時刻未取得");
        let previous=chrono::DateTime::parse_from_rfc3339("2025-12-31T23:30:00-04:00").unwrap();
        assert_eq!(ended_text(Some(&previous),now,offset),"2026-01-01 12:30");
    }
}
