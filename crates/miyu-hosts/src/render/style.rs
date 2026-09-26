//! 终端配色与 ANSI 样式常量。
//!
//! 集中一处是为了改主题时不用全文件找。命名按用途而非颜色（`HEADER_STYLE` 而
//! 不是 `BOLD_CYAN`），换配色时才不用改调用点。

pub(crate) const RESET: &str = "\x1b[0m";

pub const PRIMARY_STYLE: &str = "\x1b[38;5;189m";

pub(crate) const SECONDARY_STYLE: &str = "\x1b[36m";

pub(crate) const TERTIARY_STYLE: &str = "\x1b[35m";

pub(crate) const HEADER_STYLE: &str = "\x1b[1m\x1b[35m";

pub(crate) const INLINE_CODE_STYLE: &str = SECONDARY_STYLE;

pub(crate) const LINK_LABEL_STYLE: &str = "\x1b[38;5;117m";

pub(crate) const URL_STYLE: &str = "\x1b[2m\x1b[38;5;75m";

pub(crate) const IMAGE_STYLE: &str = "\x1b[38;5;183m";

pub(crate) const BOLD_STYLE: &str = "\x1b[1m\x1b[34m";

pub(crate) const ITALIC_STYLE: &str = "\x1b[3m\x1b[38;5;250m";

pub(crate) const STRIKE_STYLE: &str = "\x1b[9m";

pub(crate) const CODE_BLOCK_BG: &str = "";

pub(crate) const CODE_BLOCK_FRAME_STYLE: &str = SECONDARY_STYLE;

pub(crate) const CODE_TOKEN_RESET: &str = "\x1b[0m";

pub(crate) const CODE_KEYWORD_STYLE: &str = "\x1b[38;2;196;167;231m";

pub(crate) const CODE_FUNCTION_STYLE: &str = "\x1b[38;2;156;207;216m";

pub(crate) const CODE_STRING_STYLE: &str = "\x1b[38;2;166;214;160m";

pub(crate) const CODE_NUMBER_STYLE: &str = "\x1b[38;2;246;193;119m";

pub(crate) const CODE_COMMENT_STYLE: &str = "\x1b[32m";

pub(crate) const PATCH_DELETE_STYLE: &str = "\x1b[48;2;60;41;53m\x1b[38;5;210m";

pub(crate) const PATCH_INSERT_STYLE: &str = "\x1b[48;2;32;52;67m\x1b[38;5;157m";

/// 思考正文的配色:暗 + 绿。四处在用,抽出来省得改一处忘三处。
pub(crate) const THOUGHT_BODY_STYLE: &str = "\x1b[2m\x1b[38;5;10m";
