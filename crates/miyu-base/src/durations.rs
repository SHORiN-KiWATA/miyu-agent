//! 时长的展示格式(09-16 从 render 下沉:子代理日志也要写「跑了多久」,工具层不能反向认识渲染层)。

use std::time::Duration;

/// 不到一秒的读数：`<1ms` / `340ms`。
///
/// 原来这一档一律打成 `0.0s`（没信息量），上层再靠「不到十分之一秒就什么都不报」
/// 绕开它——代价是同一段代码两次跑时有时无，写不出稳定的快照。
pub fn format_sub_second(elapsed: Duration) -> String {
    if elapsed < Duration::from_millis(1) {
        "<1ms".to_string()
    } else {
        format!("{}ms", elapsed.as_millis())
    }
}

/// 时分秒:`12s` / `1m 05s` / `1h 02m 05s`。
///
/// 超过一分钟的时长都走这里(用户 09-23):goal 提示原来写成 `1407s`,收段行写成
/// `125m 03s`,工具与后台任务用时过了一小时就把秒丢了(`2h 03m`),同一个界面
/// 三种说法。不到一分钟的由调用方按自己的精度写(`340ms` / `9.9s` / `12s`)。
pub fn format_hms(elapsed: Duration) -> String {
    let total = elapsed.as_secs();
    let (hours, minutes, seconds) = (total / 3_600, total % 3_600 / 60, total % 60);
    if hours > 0 {
        format!("{hours}h {minutes:02}m {seconds:02}s")
    } else if minutes > 0 {
        format!("{minutes}m {seconds:02}s")
    } else {
        format!("{seconds}s")
    }
}

/// `340ms` / `0.3s` / `12s` / `1m 05s` / `1h 02m 05s`。
pub fn format_seconds(elapsed: Duration) -> String {
    // 不到一秒报毫秒：见 `format_sub_second`。
    if elapsed < Duration::from_secs(1) {
        return format_sub_second(elapsed);
    }
    let secs = elapsed.as_secs_f64();
    if secs < 10.0 {
        format!("{secs:.1}s")
    } else if secs < 60.0 {
        format!("{:.0}s", secs)
    } else {
        format_hms(elapsed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 超过一分钟一律时分秒,过了一小时也带秒(用户 09-23)。
    #[test]
    fn hms_keeps_seconds_past_an_hour() {
        assert_eq!(format_hms(Duration::from_secs(12)), "12s");
        assert_eq!(format_hms(Duration::from_secs(65)), "1m 05s");
        assert_eq!(format_hms(Duration::from_secs(1_407)), "23m 27s");
        assert_eq!(format_hms(Duration::from_secs(3_725)), "1h 02m 05s");
        assert_eq!(format_hms(Duration::from_secs(90_061)), "25h 01m 01s");
        assert_eq!(format_seconds(Duration::from_secs(7_503)), "2h 05m 03s");
        assert_eq!(format_seconds(Duration::from_millis(9_900)), "9.9s");
    }
}
