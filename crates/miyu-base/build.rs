use std::collections::{BTreeMap, HashSet};
use std::env;
use std::fs;
use std::path::Path;
use std::path::PathBuf;

use base64::{engine::general_purpose, Engine as _};

const PROMPT_MASK: &[u8] = b"MiyuPromptMask";

fn main() {
    println!("cargo:rerun-if-changed=../../src/prompts/miyu.md");
    println!("cargo:rerun-if-changed=../../src/prompts/miyu.hint.md");
    println!("cargo:rerun-if-changed=../../src/prompts/miyu-dialogs.md");
    println!("cargo:rerun-if-changed=../../assets/o200k_base.tiktoken");
    println!("cargo:rerun-if-changed=../../assets/jieba/dict.txt");
    // 构建 id 不在这里算:它对整棵源码树 rerun,住在根包 `build.rs`,运行时经
    // `install_build_id` 装进来。这份脚本只对上面列出的资源文件 rerun,
    // 这样改上层 crate 一行不会从 base 起全量重编。
    let out_dir = env::var("OUT_DIR").expect("OUT_DIR is set by cargo");
    // 开发态资源根(src/memes、src/personas、assets/…)是 workspace 根,不是本 crate 的目录。
    let workspace_root = Path::new(&env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root");
    println!(
        "cargo:rustc-env=MIYU_WORKSPACE_ROOT={}",
        workspace_root.display()
    );

    let obfuscate = |path: &str| {
        let content = fs::read(path).unwrap_or_else(|_| panic!("read {path}"));
        let encoded = content
            .into_iter()
            .enumerate()
            .map(|(index, byte)| byte ^ PROMPT_MASK[index % PROMPT_MASK.len()])
            .collect::<Vec<_>>();
        base64_encode(&encoded)
    };
    let prompt = obfuscate("../../src/prompts/miyu.md");
    let hint = obfuscate("../../src/prompts/miyu.hint.md");
    let dialogs = obfuscate("../../src/prompts/miyu-dialogs.md");
    let dest = Path::new(&out_dir).join("default_miyu_prompt.rs");
    fs::write(
        dest,
        format!(
            "const PROMPT_MASK: &[u8] = b\"MiyuPromptMask\";\n\
             const OBFUSCATED_DEFAULT_SYSTEM_PROMPT: &str = \"{prompt}\";\n\
             const OBFUSCATED_DEFAULT_MIYU_HINT: &str = \"{hint}\";\n\
             const OBFUSCATED_DEFAULT_MIYU_DIALOGS: &str = \"{dialogs}\";\n"
        ),
    )
    .expect("write generated prompt asset");

    build_o200k_vocab();
    build_jieba_index();
}

fn build_jieba_index() {
    let source = fs::read_to_string("../../assets/jieba/dict.txt").expect("read Jieba dictionary");
    let mut entries = BTreeMap::<String, u64>::new();
    for (line_number, line) in source.lines().enumerate() {
        let mut fields = line.split_whitespace();
        let word = fields.next().expect("Jieba dictionary word");
        let frequency = fields
            .next()
            .expect("Jieba dictionary frequency")
            .parse::<u64>()
            .unwrap_or_else(|_| panic!("invalid Jieba frequency on line {}", line_number + 1));
        entries.insert(word.to_string(), frequency);
    }
    let total = entries.values().copied().sum::<u64>();
    let max_word_chars = entries
        .keys()
        .map(|word| word.chars().count())
        .max()
        .expect("Jieba dictionary is not empty");
    let destination = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR")).join("jieba.fst");
    let mut file = fs::File::create(destination).expect("create compact Jieba index");
    use std::io::Write as _;
    file.write_all(&total.to_le_bytes())
        .expect("write Jieba frequency total");
    file.write_all(
        &u32::try_from(max_word_chars)
            .expect("maximum Jieba word length fits in u32")
            .to_le_bytes(),
    )
    .expect("write maximum Jieba word length");
    let mut builder = fst::MapBuilder::new(file).expect("create Jieba FST builder");
    for (word, frequency) in entries {
        builder
            .insert(word, frequency)
            .expect("insert sorted Jieba entry");
    }
    builder.finish().expect("finish compact Jieba index");
}

fn build_o200k_vocab() {
    let source =
        fs::read_to_string("../../assets/o200k_base.tiktoken").expect("read o200k_base vocabulary");
    let mut output = Vec::with_capacity(source.len() / 2);
    let mut tokens = HashSet::with_capacity(199_998);
    let mut count = 0usize;
    for (expected_rank, line) in source.lines().enumerate() {
        let mut parts = line.split(' ');
        let token = general_purpose::STANDARD
            .decode(parts.next().expect("vocabulary token"))
            .expect("decode vocabulary token");
        assert!(tokens.insert(token.clone()), "duplicate o200k token");
        let rank = parts
            .next()
            .expect("vocabulary rank")
            .parse::<usize>()
            .expect("parse vocabulary rank");
        assert_eq!(rank, expected_rank, "o200k ranks must be sequential");
        let len = u16::try_from(token.len()).expect("token length fits in u16");
        output.extend_from_slice(&len.to_le_bytes());
        output.extend_from_slice(&token);
        count += 1;
    }
    assert_eq!(count, 199_998, "unexpected o200k vocabulary size");
    assert_eq!(tokens.len(), count, "o200k tokens must be unique");

    let destination =
        PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR")).join("o200k_base.bin");
    fs::write(destination, output).expect("write compact o200k vocabulary");
}

fn base64_encode(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut output = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let first = chunk[0];
        let second = chunk.get(1).copied().unwrap_or(0);
        let third = chunk.get(2).copied().unwrap_or(0);
        output.push(TABLE[(first >> 2) as usize] as char);
        output.push(TABLE[(((first & 0b0000_0011) << 4) | (second >> 4)) as usize] as char);
        if chunk.len() > 1 {
            output.push(TABLE[(((second & 0b0000_1111) << 2) | (third >> 6)) as usize] as char);
        } else {
            output.push('=');
        }
        if chunk.len() > 2 {
            output.push(TABLE[(third & 0b0011_1111) as usize] as char);
        } else {
            output.push('=');
        }
    }
    output
}
