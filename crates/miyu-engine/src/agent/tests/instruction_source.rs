//! 指令源的投影规矩（`instruction_source::project`）：写一遍，各源共用。

use crate::agent::*;

struct Fake {
    current: Option<&'static str>,
    gone: Option<&'static str>,
}

impl InstructionSource for Fake {
    fn tag(&self) -> &'static str {
        "<fake "
    }

    fn current(&self) -> Option<String> {
        self.current.map(str::to_string)
    }

    fn gone(&self) -> Option<String> {
        self.gone.map(str::to_string)
    }
}

/// 请求：系统提示词、若干轮（每轮一份化石尾巴），最后是这一轮的用户消息。
fn request(fossils: &[&str]) -> Vec<ChatMessage> {
    let mut messages = vec![ChatMessage::system("s")];
    for fossil in fossils {
        messages.push(ChatMessage::plain("user", "问"));
        messages.push(ChatMessage::turn_context(fossil.to_string()));
        messages.push(ChatMessage::assistant("答".to_string(), None));
    }
    messages.push(ChatMessage::plain("user", "这一轮"));
    messages
}

#[test]
fn a_source_speaks_first_then_only_when_it_changes() {
    let a = Fake {
        current: Some("<fake v=\"a\"/>"),
        gone: None,
    };
    assert_eq!(
        project(&a, &request(&[])).as_deref(),
        Some("<fake v=\"a\"/>")
    );
    assert_eq!(project(&a, &request(&["<fake v=\"a\"/>"])), None);
    // 比的是最近一份：a → b → a 时历史里确实有 a，但模型最近看到的是 b。
    assert_eq!(
        project(&a, &request(&["<fake v=\"a\"/>", "<fake v=\"b\"/>"])).as_deref(),
        Some("<fake v=\"a\"/>")
    );
}

#[test]
fn a_source_that_went_away_says_so_once() {
    let off = Fake {
        current: None,
        gone: Some("<fake off/>"),
    };
    assert_eq!(project(&off, &request(&[])), None, "从没说过就不说没了");
    assert_eq!(
        project(&off, &request(&["<fake v=\"a\"/>"])).as_deref(),
        Some("<fake off/>")
    );
    assert_eq!(
        project(&off, &request(&["<fake v=\"a\"/>", "<fake off/>"])),
        None
    );
    let silent = Fake {
        current: None,
        gone: None,
    };
    assert_eq!(
        project(&silent, &request(&["<fake v=\"a\"/>"])),
        None,
        "默认不补"
    );
}

#[test]
fn projected_blocks_follow_the_source_order() {
    let sources: Vec<Box<dyn InstructionSource>> = vec![
        Box::new(RuntimeSource { platform: true }),
        Box::new(Fake {
            current: Some("<fake v=\"a\"/>"),
            gone: None,
        }),
    ];
    let mut messages = request(&[]);
    let before = messages.len();
    push_projected(&sources, &mut messages);
    let tail = messages[before..]
        .iter()
        .map(|message| match message.content.as_ref() {
            Some(ChatContent::Text(text)) => text.clone(),
            _ => String::new(),
        })
        .collect::<Vec<_>>();
    assert_eq!(tail.len(), 2, "{tail:?}");
    assert!(tail[0].starts_with("<runtime now="), "{tail:?}");
    assert_eq!(tail[1], "<fake v=\"a\"/>");
    assert!(messages[before..]
        .iter()
        .all(|message| message.transient_context && message.role == "user"));
}
