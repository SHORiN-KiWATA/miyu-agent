//! /reset 一族和 /stop 的回执过几秒自己撤回（用户 09-24）。

use crate::platforms::onebot::*;
use std::time::Instant;

/// 只管撤回：记下撤了哪条、什么时候撤的；`refuse` 里的消息号撤回失败。
#[derive(Default)]
struct RecallRecorder {
    refuse: Vec<String>,
    attempts: Mutex<Vec<(String, Instant)>>,
}

impl RecallRecorder {
    fn attempted(&self) -> Vec<String> {
        let attempts = self.attempts.lock().unwrap();
        attempts.iter().map(|(id, _)| id.clone()).collect()
    }
}

impl PlatformAdapter for RecallRecorder {
    fn send<'a>(&'a self, _message: OutboundMessage) -> BoxFuture<'a, Result<SendReceipt>> {
        Box::pin(async { Ok(SendReceipt::default()) })
    }

    fn bot_display_name<'a>(&'a self) -> BoxFuture<'a, Result<String>> {
        Box::pin(async { Ok("Miyu".to_string()) })
    }

    fn delete_message<'a>(&'a self, message_id: &'a str) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            self.attempts
                .lock()
                .unwrap()
                .push((message_id.to_string(), Instant::now()));
            if self.refuse.iter().any(|refused| refused == message_id) {
                anyhow::bail!("injected recall failure");
            }
            Ok(())
        })
    }
}

fn ids(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| value.to_string()).collect()
}

#[tokio::test]
async fn a_command_receipt_is_recalled_only_after_the_delay() {
    let recorder = Arc::new(RecallRecorder::default());
    let delay = Duration::from_millis(200);
    let sent_at = Instant::now();
    recall_receipt_later(
        recorder.clone(),
        "qq:1:group:2".to_string(),
        ids(&["11", "12"]),
        delay,
    )
    .expect("a delivered receipt schedules its recall")
    .await
    .unwrap();
    assert_eq!(recorder.attempted(), ids(&["11", "12"]));
    // 只断言下界：机器再忙也不会早于延时撤，晚多少不管。
    let attempts = recorder.attempts.lock().unwrap();
    assert!(attempts
        .iter()
        .all(|(_, at)| at.duration_since(sent_at) >= delay));
}

#[tokio::test]
async fn one_failed_recall_does_not_leave_the_rest_of_the_receipt() {
    let recorder = Arc::new(RecallRecorder {
        refuse: ids(&["11"]),
        ..RecallRecorder::default()
    });
    recall_receipt_later(
        recorder.clone(),
        "qq:1:private:3".to_string(),
        ids(&["11", "12"]),
        Duration::ZERO,
    )
    .expect("a delivered receipt schedules its recall")
    .await
    .unwrap();
    assert_eq!(recorder.attempted(), ids(&["11", "12"]));
}

#[test]
fn a_receipt_without_message_ids_schedules_nothing() {
    // 什么都不起，所以这里连运行时都不需要。
    let scheduled = recall_receipt_later(
        Arc::new(RecallRecorder::default()),
        "qq:1:group:2".to_string(),
        Vec::new(),
        Duration::ZERO,
    );
    assert!(scheduled.is_none());
}
