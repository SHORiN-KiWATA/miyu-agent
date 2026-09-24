//! 回合的计量：输出速度样本（词元 / 毫秒）和上下文占用。后者是压缩触发线与
//! footer 上下文条的锚点。

use crate::state::conversation_db::*;

impl ConversationDb {
    /// 记下该回合的输出速度样本(tokens / 毫秒),都是 0 = 没测到。
    pub fn set_turn_generation(&self, turn_id: &str, tokens: u64, millis: u64) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE turns SET generation_tokens = ?1, generation_ms = ?2 WHERE turn_id = ?3",
            params![tokens as i64, millis as i64, turn_id],
        )?;
        Ok(())
    }

    /// 一个会话里每条回合的输出速度样本(turn_id → (tokens, ms)),只带非零的。
    pub fn load_turn_generation(
        &self,
        session_id: &str,
    ) -> Result<std::collections::HashMap<String, (u64, u64)>> {
        let conn = self.conn.lock().unwrap();
        let mut statement = conn.prepare(
            "SELECT turn_id, generation_tokens, generation_ms FROM turns
              WHERE session_id = ?1 AND generation_tokens > 0 AND generation_ms > 0",
        )?;
        let rows = statement.query_map(params![session_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?.max(0) as u64,
                row.get::<_, i64>(2)?.max(0) as u64,
            ))
        })?;
        let mut map = std::collections::HashMap::new();
        for row in rows {
            let (turn_id, tokens, millis) = row?;
            map.insert(turn_id, (tokens, millis));
        }
        Ok(map)
    }

    /// 记下该回合最后一次请求的上下文占用(供应商真实计数)。None = 未知,
    /// 此时上下文表继续用本地估算。
    pub fn set_turn_context_end(&self, turn_id: &str, tokens: Option<u64>) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE turns SET token_context_end = ?1 WHERE turn_id = ?2",
            params![tokens.map(|value| value as i64), turn_id],
        )?;
        Ok(())
    }

    /// 最新一条可见回合的上下文锚点,且仅当它是「已完成的普通回合 + 真实
    /// (非估算)用量」时才算数。摘要行在尾(刚压完)、被打断的回合、估算用量
    /// 一律返回 None —— 那些位置的数字不代表下一次请求的前缀大小。
    /// 供应商最近一次报回来的上下文占用——**包括正在跑的这一轮**。
    ///
    /// `token_context_end` 是每**一次请求**结束就写一次的（见
    /// `turn_loop::stream`），所以一轮里调了五次工具，它就被刷新了五次。取它
    /// 的最新值，拿到的就是「上一次请求结束时」的实测数，这是发下一次请求
    /// 之前能知道的最新事实（用户 09-22：「这个信息不是最新的」）。
    ///
    /// 和 `load_context_anchor` 的差别：那个取「最新一条」再判状态，于是回合
    /// 跑着的时候最新一条就是它自己（running），一律返回 None——那是压缩触发线
    /// 要的语义（宁可退回估算），别动。这里要的恰恰相反：running 那一条的数
    /// 才是最新的。
    pub fn latest_context_end_tokens(&self, session_id: &str) -> Result<Option<u64>> {
        let conn = self.conn.lock().unwrap();
        let tokens: Option<i64> = conn
            .query_row(
                "SELECT token_context_end FROM turns
                  WHERE session_id = ?1 AND hidden = 0 AND is_summary = 0
                    AND token_usage_estimated = 0 AND token_context_end > 0
                  ORDER BY seq DESC LIMIT 1",
                params![session_id],
                |row| row.get(0),
            )
            .optional()?;
        Ok(tokens.map(|value| value as u64))
    }

    /// 最近一条带供应商实测占用的已完成普通回合，连同它的 `seq`——**不要求它是
    /// 最新一条**。它之后压缩过（出现摘要行）就不算：摘要改写了前缀，这个数作废。
    ///
    /// 和 `load_context_anchor` 的差别：那个只看最新一条，最新一条被打断了就返回
    /// None，调用方只好把整段历史重拼重数（09-23：打断一次要数两遍，80 万词元的
    /// 会话 debug 下每遍一两秒）。这里把锚点往前找，调用方只估锚点之后那几轮。
    pub fn load_last_measured_anchor(
        &self,
        session_id: &str,
    ) -> Result<Option<(ContextAnchor, i64)>> {
        let conn = self.conn.lock().unwrap();
        let row = conn
            .query_row(
                "SELECT seq, turn_id, assistant_provider_id, assistant_model, token_context_end
                   FROM turns
                  WHERE session_id = ?1 AND hidden = 0 AND is_summary = 0
                    AND status = 'completed' AND token_usage_estimated = 0
                    AND token_context_end > 0
                  ORDER BY seq DESC LIMIT 1",
                params![session_id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, i64>(4)?,
                    ))
                },
            )
            .optional()?;
        let Some((seq, turn_id, provider_id, model, tokens)) = row else {
            return Ok(None);
        };
        let compacted_since: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM turns
                            WHERE session_id = ?1 AND hidden = 0 AND is_summary = 1 AND seq > ?2)",
            params![session_id, seq],
            |row| row.get(0),
        )?;
        if compacted_since {
            return Ok(None);
        }
        Ok(Some((
            ContextAnchor {
                turn_id,
                provider_id,
                model,
                tokens: tokens as u64,
            },
            seq,
        )))
    }

    pub fn load_context_anchor(&self, session_id: &str) -> Result<Option<ContextAnchor>> {
        let conn = self.conn.lock().unwrap();
        let row = conn
            .query_row(
                "SELECT turn_id, assistant_provider_id, assistant_model, token_context_end,
                        token_usage_estimated, is_summary, status
                   FROM turns
                  WHERE session_id = ?1 AND hidden = 0
                  ORDER BY seq DESC LIMIT 1",
                params![session_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, Option<i64>>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, i64>(5)?,
                        row.get::<_, String>(6)?,
                    ))
                },
            )
            .optional()?;
        let Some((turn_id, provider_id, model, tokens, estimated, is_summary, status)) = row else {
            return Ok(None);
        };
        if estimated != 0 || is_summary != 0 || status != "completed" {
            return Ok(None);
        }
        let tokens = match tokens {
            Some(value) if value > 0 => value as u64,
            _ => return Ok(None),
        };
        Ok(Some(ContextAnchor {
            turn_id,
            provider_id,
            model,
            tokens,
        }))
    }
}
