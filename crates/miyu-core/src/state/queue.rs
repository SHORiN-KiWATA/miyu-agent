//! 排队消息。
//!
//! `*_for_target` 那一组是跨进程用的：终端里排的消息要能进到 daemon 正在跑的回
//! 合里，所以队列的目标是「某个正在跑的回合」而不是「本进程的会话」。

use crate::state::*;

impl StateStore {
    pub fn enqueue_prompt(
        &self,
        prompt_id: &str,
        content: &str,
        display_content: &str,
        attachments: &[QueuedPromptAttachment],
    ) -> Result<QueuedPrompt> {
        self.enqueue_prompt_with_uploads(prompt_id, content, display_content, attachments, &[])
    }

    pub fn enqueue_prompt_with_uploads(
        &self,
        prompt_id: &str,
        content: &str,
        display_content: &str,
        attachments: &[QueuedPromptAttachment],
        uploaded_attachment_ids: &[String],
    ) -> Result<QueuedPrompt> {
        self.conv_db.enqueue_prompt(
            &self.session(),
            None,
            prompt_id,
            content,
            display_content,
            attachments,
            uploaded_attachment_ids,
            &self.queue_session_id,
            self.queue_owner_pid,
        )
    }

    pub fn running_turn_queue_target(&self) -> Result<Option<RunningTurnQueueTarget>> {
        Ok(self
            .conv_db
            .running_turn_queue_target(&self.session())?
            .map(
                |(turn_id, queue_session_id, owner_pid)| RunningTurnQueueTarget {
                    turn_id,
                    queue_session_id,
                    owner_pid,
                },
            ))
    }

    pub fn enqueue_prompt_for_target_with_uploads(
        &self,
        target: &RunningTurnQueueTarget,
        prompt_id: &str,
        content: &str,
        display_content: &str,
        attachments: &[QueuedPromptAttachment],
        uploaded_attachment_ids: &[String],
    ) -> Result<QueuedPrompt> {
        let queue_session_id = target
            .queue_session_id
            .as_deref()
            .context("running turn does not expose a queue session")?;
        let owner_pid = target
            .owner_pid
            .context("running turn does not expose an owner process")?;
        self.conv_db.enqueue_prompt(
            &self.session(),
            Some(&target.turn_id),
            prompt_id,
            content,
            display_content,
            attachments,
            uploaded_attachment_ids,
            queue_session_id,
            owner_pid,
        )
    }

    pub fn load_queued_prompts_for_target(
        &self,
        target: &RunningTurnQueueTarget,
    ) -> Result<Vec<QueuedPrompt>> {
        let Some(queue_session_id) = target.queue_session_id.as_deref() else {
            return Ok(Vec::new());
        };
        self.conv_db
            .load_queued_prompts(&self.session(), queue_session_id)
    }

    pub fn remove_queued_prompt_for_target(
        &self,
        target: &RunningTurnQueueTarget,
        prompt_id: &str,
    ) -> Result<bool> {
        let Some(queue_session_id) = target.queue_session_id.as_deref() else {
            return Ok(false);
        };
        self.conv_db
            .remove_queued_prompt(&self.session(), prompt_id, queue_session_id)
    }

    /// 见 `ConversationDb::queued_job_report`。
    pub fn queued_job_report(&self, prompt_id: &str) -> Result<Option<JobReportResult>> {
        self.conv_db.queued_job_report(prompt_id)
    }

    pub fn load_queued_prompts(&self) -> Result<Vec<QueuedPrompt>> {
        self.conv_db
            .load_queued_prompts(&self.session(), &self.queue_session_id)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn consume_queued_prompts_with_checkpoint(
        &self,
        turn_id: &str,
        prompts: &[(String, String, String)],
        preceding_assistant_content: Option<&str>,
        preceding_assistant_reasoning: Option<&str>,
        preceding_assistant_provider_id: Option<&str>,
        preceding_assistant_model: Option<&str>,
        checkpoint: TurnRedoCheckpointPayload,
    ) -> Result<()> {
        self.conv_db.consume_queued_prompts_with_checkpoint(
            &self.session(),
            turn_id,
            prompts,
            preceding_assistant_content,
            preceding_assistant_reasoning,
            preceding_assistant_provider_id,
            preceding_assistant_model,
            &self.queue_session_id,
            Some(checkpoint),
        )
    }

    /// Explicit-cancel variant of queue cleanup: drop the prompts the user queued
    /// outright (no fold into context) and return the dropped ids. Synthetic
    /// messages (background-job reports, cross-session messages) stay queued for
    /// the end-of-turn redelivery: they arrive only once (09-26).
    pub fn delete_queued_prompts(&self) -> Result<Vec<String>> {
        self.conv_db
            .delete_queued_prompts(&self.session(), &self.queue_session_id)
    }

    /// 见 [`ConversationDb::take_queued_synthetic_prompts`]。
    pub fn take_queued_synthetic_prompts(&self) -> Result<Vec<QueuedSyntheticPrompt>> {
        self.conv_db
            .take_queued_synthetic_prompts(&self.session(), &self.queue_session_id)
    }

    pub fn discard_queued_prompts(&self) -> Result<usize> {
        self.conv_db
            .discard_queued_prompts(&self.session(), &self.queue_session_id)
    }
}

#[cfg(any(test, feature = "testkit"))]
mod test_support;
