//! 测试夹具:只在 cfg(test) 编译,生产二进制零字节。从 `src/tools/sandbox/mod.rs` 搬来(09-16 夹具搬家)。
#![allow(dead_code)]
use super::*;

pub fn confine_std(command: &mut std::process::Command) {
    use std::os::unix::process::CommandExt;
    if let Some(policy) = current_sandbox() {
        for (key, value) in child_env(&policy, false) {
            command.env(key, value);
        }
        let rules = Rules::prepare(
            &policy,
            &(
                command.get_program().to_os_string(),
                command.get_args().map(|arg| arg.to_os_string()).collect(),
            ),
        );
        // SAFETY: 同上。
        unsafe {
            command.pre_exec(move || rules.apply());
        }
    }
}
