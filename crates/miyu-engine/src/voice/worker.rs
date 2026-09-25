//! `miyu-voice` 进程主体:麦克风、唤醒、识别、提示音全部住在这里。
//!
//! 它不懂对话——识别出什么就以 IPC 信令告诉 daemon,daemon 侧的
//! `voice_bridge` 负责起回合、发通知、取消、听写中继。因此这里只有
//! 一条持久 `VoiceAttach` 连接,双向裸交换 Event 帧:
//!
//! | 方向 | kind | data |
//! |---|---|---|
//! | w→d | voice.ready | {device} |
//! | w→d | voice.wake / voice.speech_start / voice.timeout / voice.window_closed | {} |
//! | w→d | voice.command / voice.dictation | {text} |
//! | w→d | voice.transcribed | {request_id, text} |
//! | w→d | voice.error | {message} |
//! | d→w | voice.hold | {on} |
//! | d→w | voice.close_window | {} |
//! | d→w | voice.listen | {}(快捷键呼叫:不用唤醒词直接进入等待指令) |
//! | d→w | voice.dictation | {on, source: "mic" \| "stream"} |
//! | d→w | voice.audio | {pcm16: base64 的 16kHz 单声道 PCM16 LE}(stream 听写期间) |
//! | d→w | voice.transcribe | {request_id, wav_path} |
//! | d→w | voice.cue | {name} |
//! | d→w | voice.play | {wav_path}(daemon 已合成好的音频,播完删文件) |
//! | d→w | voice.stop_speaking | {} |
//! | w→d | voice.speaking | {on}(播报开始/结束;播报期间麦克风帧丢弃) |
//!
//! daemon 消失(attach 断开)即退出,由 daemon 侧负责重启。

use super::cues::{Cue, Player};
use super::speaker::Speaker;
use super::{models, Control, SttChoice, VoiceEvent, VoiceRuntimeConfig, VoiceService};
use anyhow::{Context, Result};
use miyu_base::config::{AppConfig, VoiceConfig};
use miyu_base::i18n::text as t;
use miyu_base::paths::MiyuPaths;
use miyu_core::ipc;
use serde_json::{json, Value};
use std::sync::mpsc;
use std::time::Duration;

/// 控制循环消费的归并事件。
enum WorkerEvent {
    Voice(VoiceEvent),
    Signal(String, Value),
    /// 播报开始/结束(来自播报线程)。
    Speaking(bool),
    /// 信令连接断开:daemon 没了,worker 退出。
    AttachLost,
}

/// 从配置拼出管线参数。云端 STT 的供应商按 id 在 providers 里找。
pub fn runtime_config(config: &AppConfig, paths: &MiyuPaths) -> Result<VoiceRuntimeConfig> {
    let voice = &config.voice;
    let stt = SttChoice::Local {
        threads: voice.stt_threads.max(1),
        language: voice.stt_language.clone(),
    };
    Ok(VoiceRuntimeConfig {
        models_dir: models_dir(paths),
        wake_keywords: voice.wake_keywords.clone(),
        wake_threshold: voice.wake_threshold,
        wake_boost: voice.wake_boost,
        // WebUI 的文本框留空写回的是 "",与 null 同义。
        microphone: voice
            .microphone
            .clone()
            .filter(|name| !name.trim().is_empty()),
        stt,
        stt_unload_after: Duration::from_secs(voice.stt_unload_seconds),
        follow_up: Duration::from_secs(voice.follow_up_seconds),
        min_utterance_chars: voice.min_utterance_chars,
    })
}

/// 语音模型是**机器级**资产:只读、版本钉死在目录名里、188MB。按家存就意味着
/// 每开一个家(沙箱、测具、第二个用户、换 `MIYU_HOME`)重下一遍——09-21 用户
/// 在流量统计里逮到五个 miyu-voice 进程各收 188MB,就是这么来的。
///
/// 所以读走候选链、写走机器级共享位置,与仓库里别的资源(`ResourceKind`)同一个
/// 套路。存量的 `<家>/state/models` 排在共享缓存前面:已经下过的人不用再下,
/// 老二进制也照样找得到。
///
/// 环境变量在 [`model_layout`] 一处读完,下面全是纯函数——测具要摆布局就直接
/// 构造 `ModelLayout`,不去改进程级 env(改了会污染并发跑的同伴,实测挂过)。
struct ModelLayout {
    /// `MIYU_VOICE_MODELS_DIR`:指了就只认它。
    explicit: Option<std::path::PathBuf>,
    per_home: std::path::PathBuf,
    shared: std::path::PathBuf,
    /// 安装前缀与 /usr/share/miyu/models 那几段复用既有的资源候选链
    /// (`ResourceKind::Models` 就是机器级模型目录,嵌入模型已经住在里面)。
    installed: Vec<std::path::PathBuf>,
}

fn model_layout(paths: &MiyuPaths) -> ModelLayout {
    ModelLayout {
        explicit: std::env::var_os("MIYU_VOICE_MODELS_DIR").map(std::path::PathBuf::from),
        per_home: paths.state_dir.join("models"),
        shared: shared_models_dir(),
        installed: miyu_base::paths::resources::candidates(
            miyu_base::paths::resources::ResourceKind::Models,
        ),
    }
}

/// 机器级共享缓存:跟 `MIYU_HOME` 无关,所有家共用一份。
fn shared_models_dir() -> std::path::PathBuf {
    directories::BaseDirs::new()
        .map(|dirs| dirs.cache_dir().join("miyu/voice-models"))
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp/miyu-voice-models"))
}

impl ModelLayout {
    fn search_paths(&self) -> Vec<std::path::PathBuf> {
        if let Some(explicit) = &self.explicit {
            return vec![explicit.clone()];
        }
        let mut candidates = vec![self.per_home.clone(), self.shared.clone()];
        candidates.extend(self.installed.iter().cloned());
        let mut seen: Vec<std::path::PathBuf> = Vec::new();
        candidates.retain(|path| {
            if seen.contains(path) {
                false
            } else {
                seen.push(path.clone());
                true
            }
        });
        candidates
    }

    /// 本次要用哪个目录:候选链里第一个三件齐全的;一个都没有就用下载落点。
    fn resolve(&self) -> std::path::PathBuf {
        self.search_paths()
            .into_iter()
            .find(|dir| models::models_ready(dir))
            .unwrap_or_else(|| self.download_dir())
    }

    /// 缺模型时下到哪儿:显式指定的听显式的;`<家>/state/models` 已经有半份
    /// (上次下到一半断了)就在原地补全,别把残件晾着再下一份;否则进共享缓存。
    fn download_dir(&self) -> std::path::PathBuf {
        if let Some(explicit) = &self.explicit {
            return explicit.clone();
        }
        let partial =
            std::fs::read_dir(&self.per_home).is_ok_and(|mut entries| entries.next().is_some());
        if partial {
            self.per_home.clone()
        } else {
            self.shared.clone()
        }
    }
}

pub fn models_dir(paths: &MiyuPaths) -> std::path::PathBuf {
    model_layout(paths).resolve()
}

/// 模型缺失时现场下载;下载前后各发一条桌面通知,让人知道在忙什么。
fn ensure_models_with_notice(dir: &std::path::Path) -> Result<()> {
    if models::models_ready(dir) {
        return Ok(());
    }
    miyu_base::notify::notify(
        t("Miyu voice", "Miyu 语音"),
        &format!(
            "{} ({})",
            t("downloading speech models…", "正在下载语音模型…"),
            models::DOWNLOAD_SIZE_HINT
        ),
    );
    models::ensure_models(dir, &mut |stage| tracing::info!("{stage}"))?;
    miyu_base::notify::notify(
        t("Miyu voice", "Miyu 语音"),
        t("speech models ready", "语音模型已就位"),
    );
    Ok(())
}

/// worker 形态:连回 daemon,常驻监听。
pub fn run_worker() -> Result<()> {
    let paths = MiyuPaths::new()?;
    let config = AppConfig::load_or_default(&paths)?;
    // 语音唤醒关着(只开了播报)就不碰麦克风和识别模型:进程只管播放。
    let wake_enabled = config.voice.enabled;
    let runtime = runtime_config(&config, &paths)?;
    if wake_enabled {
        ensure_models_with_notice(&runtime.models_dir)?;
    }
    let voice_config = config.voice.clone();

    let (event_tx, event_rx) = mpsc::channel::<WorkerEvent>();
    let (outbound_tx, outbound_rx) = tokio::sync::mpsc::unbounded_channel::<(String, Value)>();

    // 信令连接线程:读 daemon 信令,写识别事件。
    {
        let event_tx = event_tx.clone();
        let socket = paths.ipc_socket();
        std::thread::Builder::new()
            .name("miyu-voice-attach".into())
            .spawn(move || {
                if let Err(error) = attach_loop(&socket, event_tx.clone(), outbound_rx) {
                    tracing::error!("信令连接失败: {error:#}");
                }
                let _ = event_tx.send(WorkerEvent::AttachLost);
            })?;
    }

    let service = if wake_enabled {
        let (service, voice_events) = VoiceService::start(runtime)?;
        // 语音事件桥接线程。
        let event_tx = event_tx.clone();
        std::thread::Builder::new()
            .name("miyu-voice-events".into())
            .spawn(move || {
                for event in voice_events {
                    if event_tx.send(WorkerEvent::Voice(event)).is_err() {
                        return;
                    }
                }
            })?;
        Some(service)
    } else {
        None
    };
    let control = |command: Control| {
        if let Some(service) = &service {
            service.control(command);
        }
    };
    let player = Player::start()?;
    let cue = |name: Cue| {
        if voice_config.sounds {
            player.play(name, voice_config.sound_volume);
        }
    };
    let send = |kind: &str, data: Value| {
        let _ = outbound_tx.send((kind.to_string(), data));
    };
    let speaker = {
        let event_tx = event_tx.clone();
        Speaker::start(Box::new(move |on| {
            let _ = event_tx.send(WorkerEvent::Speaking(on));
        }))?
    };
    // 正在播的 daemon 合成文件,播完删。
    let mut playing_file: Option<String> = None;

    let device = service
        .as_ref()
        .map(|service| service.device_description.clone())
        .unwrap_or_else(|| t("(wake off, playback only)", "(唤醒关闭,仅播报)").to_string());
    tracing::info!("语音前端就绪:采集 {device}");
    send("voice.ready", json!({ "device": device }));

    for event in event_rx {
        match event {
            WorkerEvent::Voice(event) => match event {
                VoiceEvent::Wake => {
                    cue(Cue::Wake);
                    send("voice.wake", json!({}));
                }
                VoiceEvent::Command(text) => {
                    cue(Cue::Heard);
                    send("voice.command", json!({ "text": text }));
                }
                VoiceEvent::Dictation(text) => {
                    send("voice.dictation", json!({ "text": text }));
                }
                VoiceEvent::SpeechStart => send("voice.speech_start", json!({})),
                VoiceEvent::ListeningTimeout => send("voice.timeout", json!({})),
                VoiceEvent::WindowClosed => send("voice.window_closed", json!({})),
                VoiceEvent::ListenOff => {
                    cue(Cue::Off);
                    send("voice.window_closed", json!({ "reason": "listen" }))
                }
                VoiceEvent::Transcribed { request_id, text } => send(
                    "voice.transcribed",
                    json!({ "request_id": request_id, "text": text }),
                ),
                VoiceEvent::HeardSpeech(seconds) => {
                    tracing::debug!("听到 {seconds:.1}s 语音,未命中唤醒词")
                }
                VoiceEvent::Timing {
                    stage,
                    audio_secs,
                    millis,
                } => tracing::debug!("[timing] {stage} audio={audio_secs:.2}s took={millis}ms"),
                VoiceEvent::Fatal(message) => {
                    send("voice.error", json!({ "message": message }));
                    // 给信令线程一点时间把错误送出去。
                    std::thread::sleep(Duration::from_millis(200));
                    anyhow::bail!("语音服务致命错误: {message}");
                }
            },
            WorkerEvent::Signal(kind, data) => match kind.as_str() {
                "voice.hold" => control(Control::Hold(
                    data.get("on").and_then(Value::as_bool).unwrap_or(false),
                )),
                "voice.close_window" => control(Control::CloseWindow),
                "voice.listen" => {
                    // 快捷键呼叫要立刻听得见:先掐掉正在播的。
                    speaker.stop();
                    control(Control::Listen)
                }
                "voice.play" => {
                    let path = data
                        .get("wav_path")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    match std::fs::read(&path) {
                        Ok(bytes) => {
                            playing_file = Some(path);
                            speaker.play_wav(bytes);
                        }
                        Err(error) => tracing::warn!("读取播报音频失败 {path}: {error}"),
                    }
                }
                "voice.stop_speaking" => speaker.stop(),
                "voice.dictation" => {
                    if data.get("on").and_then(Value::as_bool).unwrap_or(false) {
                        let external = data
                            .get("source")
                            .and_then(Value::as_str)
                            .is_some_and(|source| source == "stream");
                        control(Control::StartDictation { external });
                    } else {
                        control(Control::StopDictation);
                    }
                }
                "voice.audio" => {
                    let encoded = data
                        .get("pcm16")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    match decode_pcm16(encoded) {
                        Ok(samples) if !samples.is_empty() => control(Control::Audio(samples)),
                        Ok(_) => {}
                        Err(error) => tracing::warn!("外部音频帧解码失败: {error:#}"),
                    }
                }
                "voice.transcribe" => {
                    let request_id = data
                        .get("request_id")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    let path = data
                        .get("wav_path")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    match load_wav_16k(path) {
                        Ok(samples) => control(Control::Transcribe {
                            request_id,
                            samples,
                        }),
                        Err(error) => {
                            tracing::warn!("读取待转写音频失败: {error:#}");
                            send(
                                "voice.transcribed",
                                json!({ "request_id": request_id, "text": "", "error": format!("{error:#}") }),
                            );
                        }
                    }
                }
                "voice.cue" => {
                    if let Some(name) = data
                        .get("name")
                        .and_then(Value::as_str)
                        .and_then(Cue::parse)
                    {
                        cue(name);
                    }
                }
                other => tracing::debug!("忽略未知信令 {other}"),
            },
            WorkerEvent::Speaking(on) => {
                control(Control::Playback(on));
                send("voice.speaking", json!({ "on": on }));
                if !on {
                    if let Some(path) = playing_file.take() {
                        let _ = std::fs::remove_file(path);
                    }
                }
            }
            WorkerEvent::AttachLost => {
                tracing::info!("daemon 信令连接断开,worker 退出");
                break;
            }
        }
    }
    Ok(())
}

/// base64 的 PCM16 LE(16kHz 单声道)→ f32 采样。
fn decode_pcm16(encoded: &str) -> Result<Vec<f32>> {
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded.trim())
        .context("base64")?;
    Ok(bytes
        .chunks_exact(2)
        .map(|pair| i16::from_le_bytes([pair[0], pair[1]]) as f32 / 32768.0)
        .collect())
}

/// 读 WAV 并重采样到 16kHz 单声道。
fn load_wav_16k(path: &str) -> Result<Vec<f32>> {
    let bytes = std::fs::read(path).with_context(|| format!("读取 {path}"))?;
    let (rate, samples) = super::stt::decode_wav(&bytes)?;
    if rate == super::pipeline::SAMPLE_RATE {
        return Ok(samples);
    }
    let mut resampler = super::mic::LinearResampler::new(rate, super::pipeline::SAMPLE_RATE);
    Ok(resampler.process(&samples))
}

/// 信令连接:注册 VoiceAttach,读信令写事件,直到断开。
fn attach_loop(
    socket: &std::path::Path,
    event_tx: mpsc::Sender<WorkerEvent>,
    mut outbound: tokio::sync::mpsc::UnboundedReceiver<(String, Value)>,
) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async move {
        let mut stream = ipc::connect(socket).await?;
        ipc::send(&mut stream, &ipc::Request::new(ipc::Command::VoiceAttach)).await?;
        let first = ipc::receive::<ipc::Frame>(&mut stream)
            .await?
            .context("daemon 关闭了信令连接")?;
        anyhow::ensure!(
            matches!(first, ipc::Frame::Ack),
            "VoiceAttach 被拒绝: {first:?}"
        );
        loop {
            tokio::select! {
                outgoing = outbound.recv() => {
                    let Some((kind, data)) = outgoing else { break };
                    ipc::send(&mut stream, &ipc::Frame::Event { id: 0, kind, data, at_ms: None }).await?;
                }
                frame = ipc::receive::<ipc::Frame>(&mut stream) => {
                    let Some(frame) = frame? else { break };
                    if let ipc::Frame::Event { kind, data, .. } = frame {
                        if event_tx.send(WorkerEvent::Signal(kind, data)).is_err() {
                            break;
                        }
                    }
                }
            }
        }
        Ok(())
    })
}

/// 测试形态:不连 daemon,打开麦克风把事件逐行打印。排查"没反应"先跑它:
/// 一行"听到语音"都没有 = 音频没进来;有但不命中 = 唤醒词层。
pub fn run_test(keyword: Option<String>, device: Option<String>, timings: bool) -> Result<()> {
    let paths = MiyuPaths::new()?;
    let config = AppConfig::load_or_default(&paths)?;
    let mut runtime = runtime_config(&config, &paths)?;
    ensure_models_with_notice(&runtime.models_dir)?;
    if let Some(keyword) = keyword {
        runtime.wake_keywords = miyu_base::config::split_wake_keywords(&keyword);
    }
    if device.is_some() {
        runtime.microphone = device;
    }
    println!(
        "{}",
        if miyu_base::i18n::is_zh() {
            format!(
                "语音测试:唤醒词「{}」,对着麦克风说话,Ctrl+C 退出",
                runtime.wake_keywords.join(" / ")
            )
        } else {
            format!(
                "voice test: keywords \"{}\", speak into the mic, Ctrl+C to quit",
                runtime.wake_keywords.join(" / ")
            )
        }
    );
    let sources = super::mic::list_input_sources();
    if !sources.is_empty() {
        println!("{}:", t("available input sources", "可用输入源"));
        for source in &sources {
            println!("  - {}  ({})", source.label, source.name);
        }
    }
    let player = Player::start()?;
    let voice_config: VoiceConfig = config.voice.clone();
    let (service, events) = VoiceService::start(runtime)?;
    println!(
        "{}: {}",
        t("capturing from", "正在采集"),
        service.device_description
    );
    for event in events {
        match event {
            VoiceEvent::SpeechStart => {
                println!("· {}", t("speech onset in window", "窗口内检测到开口"))
            }
            VoiceEvent::HeardSpeech(seconds) => println!(
                "· {}",
                if miyu_base::i18n::is_zh() {
                    format!("听到 {seconds:.1}s 语音,未命中唤醒词")
                } else {
                    format!("heard {seconds:.1}s of speech, wake word not matched")
                }
            ),
            VoiceEvent::Wake => {
                if voice_config.sounds {
                    player.play(Cue::Wake, voice_config.sound_volume);
                }
                println!("✔ {}", t("wake word hit, listening…", "唤醒命中,等待指令…"))
            }
            VoiceEvent::Command(text) => {
                if voice_config.sounds {
                    player.play(Cue::Heard, voice_config.sound_volume);
                }
                println!("» {text}")
            }
            VoiceEvent::Dictation(text) => println!("» [{}] {text}", t("dictation", "听写")),
            VoiceEvent::ListeningTimeout => {
                println!("… {}", t("timed out, back to wake word", "超时,回到待唤醒"))
            }
            VoiceEvent::ListenOff => {
                if voice_config.sounds {
                    player.play(Cue::Off, voice_config.sound_volume);
                }
                println!("… {}", t("stopped listening", "不听了"))
            }
            VoiceEvent::WindowClosed => println!(
                "… {}",
                t(
                    "window closed, wake word required again",
                    "窗口关闭,重新需要唤醒词"
                )
            ),
            VoiceEvent::Transcribed { text, .. } => println!("» {text}"),
            VoiceEvent::Timing {
                stage,
                audio_secs,
                millis,
            } => {
                if timings {
                    println!("  [timing] {stage} audio={audio_secs:.2}s took={millis}ms");
                }
            }
            VoiceEvent::Fatal(message) => {
                anyhow::bail!("{message}");
            }
        }
    }
    Ok(())
}

/// 试听提示音。
pub fn play_cue(name: &str, volume: f32) -> Result<()> {
    let cue = Cue::parse(name).with_context(|| format!("未知提示音「{name}」"))?;
    let player = Player::start()?;
    player.play(cue, volume);
    // Player 线程是异步排队的,等它播完。
    std::thread::sleep(Duration::from_millis(900));
    Ok(())
}

/// 模型解析的候选链与下载落点(09-21)。
///
/// 不碰网络、也不碰进程级环境变量:用空文件摆出「三件齐全」的形状,只钉解析
/// 顺序——这正是之前出事的那一层(单路径 `state/models`,换个家就重下 188MB)。
#[cfg(test)]
mod model_path_tests {
    use super::*;
    use std::path::{Path, PathBuf};

    /// 摆出 `models_ready` 认账的七个文件。
    fn seed(dir: &Path) {
        let kws = models::kws_paths(dir);
        let sense = models::sense_voice_paths(dir);
        for path in [
            &kws.encoder,
            &kws.decoder,
            &kws.joiner,
            &kws.tokens,
            &sense.model,
            &sense.tokens,
            &models::vad_path(dir),
        ] {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, b"").unwrap();
        }
        assert!(models::models_ready(dir));
    }

    fn layout(root: &Path) -> ModelLayout {
        ModelLayout {
            explicit: None,
            per_home: root.join("state/models"),
            shared: root.join("shared"),
            installed: vec![root.join("usr-share/models")],
        }
    }

    /// 存量优先:已经下过的人升级二进制后照样命中原地那份,不重下。
    #[test]
    fn an_existing_per_home_copy_still_wins() {
        let temp = tempfile::tempdir().unwrap();
        let layout = layout(temp.path());
        seed(&layout.per_home);
        seed(&layout.shared);
        assert_eq!(layout.resolve(), layout.per_home);
    }

    /// 家里没有、共享缓存有:零下载。这就是「换个家就重下」被堵掉的那一刀。
    #[test]
    fn a_fresh_home_reuses_the_shared_cache() {
        let temp = tempfile::tempdir().unwrap();
        let layout = layout(temp.path());
        seed(&layout.shared);
        assert_eq!(layout.resolve(), layout.shared);
    }

    /// 装机自带的那份也认(将来包里带语音模型时不用再下)。
    #[test]
    fn an_installed_copy_is_used_when_nothing_else_has_it() {
        let temp = tempfile::tempdir().unwrap();
        let layout = layout(temp.path());
        seed(&layout.installed[0]);
        assert_eq!(layout.resolve(), layout.installed[0]);
    }

    /// 谁都没有:落到共享缓存,而不是各回各家。
    #[test]
    fn nothing_anywhere_downloads_into_the_shared_cache() {
        let temp = tempfile::tempdir().unwrap();
        let layout = layout(temp.path());
        assert_eq!(layout.resolve(), layout.shared);
    }

    /// 上次下到一半:在原地补全,别把残件晾着再下一份。
    #[test]
    fn a_half_finished_per_home_download_is_finished_in_place() {
        let temp = tempfile::tempdir().unwrap();
        let layout = layout(temp.path());
        std::fs::create_dir_all(&layout.per_home).unwrap();
        std::fs::write(models::vad_path(&layout.per_home), b"").unwrap();
        assert!(!models::models_ready(&layout.per_home));
        assert_eq!(layout.resolve(), layout.per_home);
    }

    /// 显式指定就只认它,读写都不再看别处——测具与多机部署靠这条。
    #[test]
    fn an_explicit_override_wins_over_everything() {
        let temp = tempfile::tempdir().unwrap();
        let mut layout = layout(temp.path());
        seed(&layout.per_home);
        let pinned = temp.path().join("pinned");
        layout.explicit = Some(pinned.clone());
        assert_eq!(layout.search_paths(), vec![pinned.clone()]);
        assert_eq!(layout.resolve(), pinned.clone());
        assert_eq!(layout.download_dir(), pinned);
    }

    /// 候选链自己不许有重复项——重复会让「第一个就位的胜出」看起来对、
    /// 实则把后面的候选挤掉。
    #[test]
    fn the_candidate_chain_has_no_duplicates() {
        let temp = tempfile::tempdir().unwrap();
        let mut layout = layout(temp.path());
        // 装机候选里混进一个与 per_home 相同的路径:去重必须把它吃掉。
        layout.installed.push(layout.per_home.clone());
        let candidates = layout.search_paths();
        let mut seen: Vec<PathBuf> = Vec::new();
        for path in &candidates {
            assert!(!seen.contains(path), "duplicate candidate: {path:?}");
            seen.push(path.clone());
        }
        assert_eq!(candidates.len(), 3, "{candidates:?}");
    }

    /// 真实布局(读 env、读 XDG)至少给得出共享缓存与家目录两条,顺序不变。
    #[test]
    fn the_real_layout_keeps_per_home_ahead_of_the_shared_cache() {
        let paths = MiyuPaths::new().unwrap();
        let layout = model_layout(&paths);
        if layout.explicit.is_some() {
            return;
        }
        let candidates = layout.search_paths();
        let home = candidates.iter().position(|p| *p == layout.per_home);
        let shared = candidates.iter().position(|p| *p == layout.shared);
        assert!(home < shared, "{candidates:?}");
    }
}
