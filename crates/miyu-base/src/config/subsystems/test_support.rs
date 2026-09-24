//! 测试夹具:只在 cfg(test) 编译,生产二进制零字节。从 `src/config/subsystems.rs` 搬来(09-16 夹具搬家)。
#![allow(dead_code)]
use super::*;

impl EnabledSubsystems {
    /// 一件都不构造(dev 的 core_only 清单必须落在这里)。
    pub fn is_empty(&self) -> bool {
        !(self.memory || self.persona_reminder || self.voice || self.emotion)
    }

    /// 开着的子系统 id,按表顺序。
    pub fn ids(&self) -> Vec<&'static str> {
        SUBSYSTEMS
            .iter()
            .map(|descriptor| descriptor.id)
            .filter(|id| self.get(id))
            .collect()
    }

    pub fn get(&self, id: &str) -> bool {
        match id {
            "memory" => self.memory,
            "persona_reminder" => self.persona_reminder,
            "voice" => self.voice,
            "emotion" => self.emotion,
            _ => false,
        }
    }
}
