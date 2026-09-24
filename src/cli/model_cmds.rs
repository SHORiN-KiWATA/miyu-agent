//! 模型与思考变体的命令。
//!
//! `miyu models` 管的是「这个会话用哪些模型」，`miyu variant` 管的是「思考多
//! 深」。两者都有全局池与会话覆盖两层：会话不设就继承全局，设了就只用自己
//! 那份。菜单渲染也在这里——终端里要在有限宽度内把 provider、模型名、变体三
//! 列排整齐。

use crate::cli::*;

pub(in crate::cli) fn short_model_name(model: &str, provider: &str) -> String {
    model
        .strip_prefix(&format!("{provider}/"))
        .unwrap_or(model)
        .rsplit('/')
        .next()
        .unwrap_or(model)
        .to_string()
}

pub(in crate::cli) fn print_mixed_model_endpoint(
    show: bool,
    result: &miyu_core::llm::ChatResult,
    variant: Option<&str>,
) {
    if !show {
        return;
    }
    let provider = result.provider_id.as_deref().unwrap_or("-");
    let model = result.model.as_deref().unwrap_or("-");
    println!(
        "\x1b[2m{}\x1b[0m\n",
        mixed_model_endpoint_label(provider, model, variant)
    );
}

pub(in crate::cli) fn mixed_model_endpoint_label(
    provider: &str,
    model: &str,
    variant: Option<&str>,
) -> String {
    let variant = variant
        .filter(|variant| !variant.is_empty())
        .map(|variant| format!(" · {variant}"))
        .unwrap_or_default();
    format!("{provider} / {model}{variant}")
}

/// 交互 REPL 里挂在回复末尾的那一行「本次供应商 / 模型」：暗色，和回复之间空一行
/// （渲染器收尾本来就留一个空行，这里不再多加，否则是两行）。全屏下按时间线正文
/// 的缩进对齐（和「已中断」那类提示一个位置）。
pub(in crate::cli) fn mixed_model_endpoint_frame(
    provider: &str,
    model: &str,
    variant: Option<&str>,
) -> String {
    // 光 SGR 2 在 kitty 里只淡一点点,用户看着「不是暗色」;配一个中灰(245)才
    // 是肉眼分得出的暗色,亮暗两种底色都看得清。尾巴留一个空行:正文块的规矩是
    // 每块自带尾空行(「已中断」那类提示也是),不留的话紧跟着的 `/compact` 提示
    // 会贴上来(用户 09-18 截图)。
    let line = format!(
        "\x1b[2m\x1b[38;5;245m{}\x1b[0m\n\n",
        mixed_model_endpoint_label(provider, model, variant)
    );
    if render::blocks::enabled() {
        render::timeline::indent_body(&line)
    } else {
        line
    }
}

/// 会话钉了模型池就按会话的算（`footer_config_for_session` 同一道守卫），这里
/// 拿的是已经钉到会话上的 store。「是不是混合」要看会话池，不是全局池：
/// 用户全局只挂一个模型、会话里钉两个的情形，按全局判永远不显示（BUG-05）。
pub(in crate::cli) fn session_scoped_config(store: &StateStore, config: &AppConfig) -> AppConfig {
    let mut config = config.clone();
    let session_id = store.session_id();
    if let Ok(Some(models)) = store.session_model_override(&session_id) {
        match config.usable_model_override(models) {
            Some(usable) => config.active_provider_models = Some(usable),
            None => drop_stale_model_override(store, &session_id),
        }
    }
    config
}

pub(in crate::cli) fn show_mixed_model_endpoint(config: &AppConfig, interactive: bool) -> bool {
    config.active_provider_model_choices().len() > 1
        && match config.display.mixed_model_endpoint_display.as_str() {
            "off" => false,
            "all" => true,
            _ => interactive,
        }
}

pub(in crate::cli) fn initialize_models_cache(paths: &MiyuPaths) {
    miyu_base::models_cache::try_load(paths);
    miyu_base::models_cache::spawn_background_refresh(paths.clone());
    if let Ok(config) = AppConfig::load_or_default(paths) {
        miyu_base::models_cache::spawn_provider_api_refresh(config.providers);
    }
}

pub(in crate::cli) async fn run_models(paths: &MiyuPaths, args: ModelsArgs) -> Result<()> {
    run_models_for_session(paths, args, None).await.map(|_| ())
}

/// REPL 的 `/models` 收的是一整串自由文本,这里把 `--global` / `-g` 从中
/// 摘出来,让两个入口的写法一致(`/models --global`、`/models -g gpt-5`)。
pub(in crate::cli) fn parse_models_argument(argument: &str) -> ModelsArgs {
    let mut global = false;
    let mut rest = argument.trim();
    loop {
        let stripped = rest
            .strip_prefix("--global")
            .or_else(|| rest.strip_prefix("-g"))
            .filter(|remainder| remainder.is_empty() || remainder.starts_with(char::is_whitespace));
        match stripped {
            Some(remainder) => {
                global = true;
                rest = remainder.trim_start();
            }
            None => break,
        }
    }
    ModelsArgs {
        target: (!rest.is_empty()).then(|| rest.to_string()),
        global,
    }
}

/// `miyu models --global`:直接编辑全局激活模型池。
///
/// 不带 --global 时这条命令改的只是终端集成会话的覆盖,全局池此前只能进
/// `miyu config` 的 TUI 里翻。全局池是所有没有单独覆盖的会话(WebUI、通讯
/// 平台、新开的终端会话)共同的默认来源,值得有一条一行就能改完的路。
///
/// 与会话覆盖的两点不同:池不能清空(至少留一个端点,`set_active_provider_models`
/// 自己会拦),以及 `default` 没有意义——全局池本身就是那个"默认"。
/// 返回真表示**真的改了**。假是"什么都没发生"：Esc 退出、勾选没变、或者
/// 不在终端里（只打了个清单）。调用方靠它决定要不要说"已更新"——不看这个
/// 的话，Esc 取消也会收到一句"会话模型已更新"（用户实测）。
pub(in crate::cli) async fn run_models_global(
    paths: &MiyuPaths,
    target: Option<&str>,
) -> Result<bool> {
    let mut config = AppConfig::load(paths)?;
    let choices = config.text_provider_model_choices();
    if choices.is_empty() {
        bail!(
            "{}",
            t(
                "no configured provider models; configure a model first",
                "没有已配置的 provider 模型；请先配置模型",
            )
        );
    }
    let selected = if let Some(target) = target.map(str::trim) {
        if target.eq_ignore_ascii_case("default") || target.eq_ignore_ascii_case("global") {
            bail!(
                "{}",
                t(
                    "the global pool is the default; pick a concrete model instead",
                    "全局池本身就是默认来源，请直接指定具体模型",
                )
            );
        }
        let choice = miyu_base::config::resolve_provider_model_argument(&choices, target)
            .map_err(anyhow::Error::msg)?;
        vec![ActiveProviderModelConfig {
            provider_id: choice.provider_id.clone(),
            model: choice.model.clone(),
        }]
    } else {
        if !(io::stdout().is_terminal() && io::stdin().is_terminal()) {
            print_model_choices(&config, &choices, None);
            return Ok(false);
        }
        let initial = choices
            .iter()
            .map(|choice| config.is_active_provider_model(&choice.provider_id, &choice.model))
            .collect::<Vec<_>>();
        let Some(active) = inline_fuzzy_select(
            &choices
                .iter()
                .map(|choice| choice.label())
                .collect::<Vec<_>>(),
            initial.clone(),
        )?
        else {
            return Ok(false);
        };
        if active == initial {
            println!(
                "{}",
                t(
                    "no changes (Enter picks the highlighted model; Tab multi-selects)",
                    "未做修改（回车=选定高亮模型,Tab=多选勾选）"
                )
            );
            return Ok(false);
        }
        choices
            .iter()
            .zip(active)
            .filter_map(|(choice, active)| {
                active.then(|| ActiveProviderModelConfig {
                    provider_id: choice.provider_id.clone(),
                    model: choice.model.clone(),
                })
            })
            .collect::<Vec<_>>()
    };
    if selected.is_empty() {
        bail!(
            "{}",
            t(
                "at least one model must stay active in the global pool",
                "全局池至少要保留一个激活模型",
            )
        );
    }
    config.set_active_provider_models(&selected)?;
    config.save(paths)?;
    let labels = selected
        .iter()
        .map(|model| format!("{}/{}", model.provider_id, model.model))
        .collect::<Vec<_>>()
        .join(", ");
    println!("{}: {labels}", t("global model pool", "全局激活模型池"));
    // daemon 在跑就让它立刻吃到新池,否则要等下次重启。已绑定覆盖的会话
    // 不受影响——它们本来就不看全局池。
    if ipc::daemon_info(paths).await.is_some() {
        retry_config_reload(RELOAD_MAX_ATTEMPTS, RELOAD_RETRY_INTERVAL, || {
            request_config_reload(paths)
        })
        .await?;
    }
    Ok(true)
}

/// Switches the model pool of one session (the current session when
/// `session_id` is None). The override persists on the session, so reopening
/// it restores the model; the global pool is managed in `miyu config`.
/// 返回真表示**真的改了**；见 [`run_models_global`]。
pub(in crate::cli) async fn run_models_for_session(
    paths: &MiyuPaths,
    args: ModelsArgs,
    session_id: Option<&str>,
) -> Result<bool> {
    if args.global {
        return run_models_global(paths, args.target.as_deref()).await;
    }
    let config = AppConfig::load(paths)?;
    let choices = config.text_provider_model_choices();
    if choices.is_empty() {
        bail!(
            "{}",
            t(
                "no configured provider models; configure a model first",
                "没有已配置的 provider 模型；请先配置模型",
            )
        );
    }
    if let Some(target) = args.target.as_deref() {
        let target = target.trim();
        if target.eq_ignore_ascii_case("default") || target.eq_ignore_ascii_case("global") {
            set_session_models(paths, session_id, Vec::new()).await?;
            println!(
                "{}",
                t(
                    "this session now follows the global active pool",
                    "当前会话已恢复跟随全局激活模型池"
                )
            );
            return Ok(true);
        }
        let choice = miyu_base::config::resolve_provider_model_argument(&choices, target)
            .map_err(anyhow::Error::msg)?;
        let label = choice.label();
        let models = vec![ActiveProviderModelConfig {
            provider_id: choice.provider_id.clone(),
            model: choice.model.clone(),
        }];
        set_session_models(paths, session_id, models).await?;
        println!("{}: {label}", t("session model", "当前会话模型"));
        return Ok(true);
    }
    if io::stdout().is_terminal() && io::stdin().is_terminal() {
        let menu = SessionModelMenu::new(&config, choices, paths, session_id)?;
        let Some(active) = inline_fuzzy_select_with(
            &menu.labels,
            menu.initial.clone(),
            Some(&menu.toggle_rule()),
        )?
        else {
            // 选择器里按了 Ctrl+C：什么都没发生，别让调用方去报"已更新"。
            return Ok(false);
        };
        let (changed, message) = menu.apply(paths, session_id, active).await?;
        println!("{message}");
        return Ok(changed);
    }
    print_model_choices(&config, &choices, None);
    Ok(false)
}

/// /models 交互菜单的数据。第一项是「继承全局模型池」，与 config TUI 的会话/QQ
/// 模型菜单同款：会话没有自己的覆盖时它就是当前状态。此前想恢复继承只能记住
/// `miyu models default` 这个隐藏写法，菜单里根本看不到这条路。
///
/// 行内（`inline_fuzzy_select`）与全屏面板（`pick_multi_with`）共用：菜单项、入场勾选
/// 在这里算，选完交回 `apply` 落盘——两条路一个规矩。
pub(in crate::cli) struct SessionModelMenu {
    choices: Vec<miyu_base::config::ProviderModelChoice>,
    pub(in crate::cli) labels: Vec<String>,
    pub(in crate::cli) initial: Vec<bool>,
    /// 各行「因继承而勾上」吗——全局激活池里的那几个（第 0 行恒为假）。Tab 的
    /// 连带规矩靠它：取消继承时把这些一并取消。
    pub(in crate::cli) derived: Vec<bool>,
}

/// `/models` 菜单里 Tab 的连带规矩（行内与全屏两个选择器共用）。第 0 行是「继承
/// 全局模型池」，`derived[i]` 标着因继承而勾上的模型行。
///
/// - 取消继承：派生勾选一并取消（用户 09-17：「取消激活继承全局模型的时候，那些
///   被激活的全局模型应该跟着一起被取消激活」），之后自己挑；
/// - 勾回继承：模型行回到派生态——全局池那几个勾上、别的清掉；
/// - 继承着时翻某个模型行：那就是要自己钉一批了，继承取消、派生勾选留作起点，
///   再翻转这一行。
///
/// 原来 Tab 是纯单行翻转：继承与各模型在存储层本是一对互斥形态（`None` = 继承、
/// `Some(list)` = 覆盖），菜单把它拍平成一排互不相干的布尔，派生关系只在入场算过
/// 一次、之后没人维护（初诊 BUG-10）。
pub(in crate::cli) fn toggle_model_row(active: &mut [bool], derived: &[bool], index: usize) {
    if active.is_empty() || index >= active.len() {
        return;
    }
    if index == 0 {
        let inherit = !active[0];
        active[0] = inherit;
        for (slot, is_derived) in active.iter_mut().zip(derived.iter()).skip(1) {
            *slot = inherit && *is_derived;
        }
        return;
    }
    if active[0] {
        active[0] = false;
    }
    active[index] = !active[index];
}

/// 菜单结果落盘前的裁决（纯函数，好测）。
#[derive(Debug, PartialEq, Eq)]
pub(in crate::cli) enum ModelMenuDecision {
    /// 什么都没改。
    NoChange,
    /// 回到继承全局池。
    Inherit,
    /// 取消了继承却一个模型都没勾：存储层没有这个状态（空 = 继承），得说清楚。
    NeedOne,
    /// 自己钉一批：勾上的模型行下标（不含第 0 行）。
    Override(Vec<usize>),
}

pub(in crate::cli) fn decide_model_menu(initial: &[bool], active: &[bool]) -> ModelMenuDecision {
    let was_inherit = initial.first().copied().unwrap_or(false);
    let inherit = active.first().copied().unwrap_or(false);
    if inherit {
        return if was_inherit {
            ModelMenuDecision::NoChange
        } else {
            ModelMenuDecision::Inherit
        };
    }
    let picked = active
        .iter()
        .enumerate()
        .skip(1)
        .filter_map(|(index, on)| on.then_some(index - 1))
        .collect::<Vec<_>>();
    if picked.is_empty() {
        // 本来就是覆盖、现在全清了 = 回到继承（老规矩）；本来在继承、取消继承后
        // 一个没勾 = 没法落盘，提示一句。
        return if was_inherit {
            ModelMenuDecision::NeedOne
        } else {
            ModelMenuDecision::Inherit
        };
    }
    if !was_inherit && initial.iter().skip(1).eq(active.iter().skip(1)) {
        return ModelMenuDecision::NoChange;
    }
    ModelMenuDecision::Override(picked)
}

impl SessionModelMenu {
    pub(in crate::cli) fn new(
        config: &AppConfig,
        choices: Vec<miyu_base::config::ProviderModelChoice>,
        paths: &MiyuPaths,
        session_id: Option<&str>,
    ) -> Result<Self> {
        let override_pool = session_model_override_snapshot(paths, session_id)?;
        let inherit_label = t("Inherit global model pool", "继承全局模型池").to_string();
        let mut labels = vec![inherit_label];
        labels.extend(choices.iter().map(|choice| choice.label()));
        let mut initial = vec![override_pool.is_none()];
        initial.extend(choices.iter().map(|choice| match override_pool.as_deref() {
            Some(pool) => pool.iter().any(|model| {
                model.provider_id == choice.provider_id && model.model == choice.model
            }),
            None => config.is_active_provider_model(&choice.provider_id, &choice.model),
        }));
        let mut derived = vec![false];
        derived.extend(
            choices
                .iter()
                .map(|choice| config.is_active_provider_model(&choice.provider_id, &choice.model)),
        );
        Ok(Self {
            choices,
            labels,
            initial,
            derived,
        })
    }

    /// 给选择器的 Tab 规矩。
    pub(in crate::cli) fn toggle_rule(&self) -> impl Fn(&mut [bool], usize) + '_ {
        move |active, index| toggle_model_row(active, &self.derived, index)
    }

    /// 把菜单结果落成会话覆盖。返回（真的改了没, 给用户的一句话）。
    pub(in crate::cli) async fn apply(
        &self,
        paths: &MiyuPaths,
        session_id: Option<&str>,
        active: Vec<bool>,
    ) -> Result<(bool, String)> {
        let follows_global = t(
            "this session now follows the global active pool",
            "当前会话已恢复跟随全局激活模型池",
        )
        .to_string();
        let picked = match decide_model_menu(&self.initial, &active) {
            ModelMenuDecision::NoChange => {
                return Ok((
                    false,
                    t(
                        "no changes (Enter picks the highlighted model; Tab multi-selects)",
                        "未做修改（回车=选定高亮模型,Tab=多选勾选）",
                    )
                    .to_string(),
                ));
            }
            ModelMenuDecision::NeedOne => {
                return Ok((
                    false,
                    t(
                        "inheritance was unticked but no model is ticked; still inheriting the global pool (tick at least one model to pin your own)",
                        "取消了继承但一个模型都没勾，仍继承全局模型池（要自己钉一批就至少勾一个）",
                    )
                    .to_string(),
                ));
            }
            ModelMenuDecision::Inherit => {
                set_session_models(paths, session_id, Vec::new()).await?;
                return Ok((true, follows_global));
            }
            ModelMenuDecision::Override(picked) => picked,
        };
        let models = picked
            .into_iter()
            .filter_map(|index| self.choices.get(index))
            .map(|choice| ActiveProviderModelConfig {
                provider_id: choice.provider_id.clone(),
                model: choice.model.clone(),
            })
            .collect::<Vec<_>>();
        set_session_models(paths, session_id, models).await?;
        Ok((
            true,
            t("session models updated", "已更新当前会话模型").to_string(),
        ))
    }
}

pub(in crate::cli) fn run_list_models(paths: &MiyuPaths) -> Result<()> {
    let config = AppConfig::load(paths)?;
    let choices = config.text_provider_model_choices();
    if choices.is_empty() {
        bail!(
            "{}",
            t(
                "no configured provider models; configure a model first",
                "没有已配置的 provider 模型；请先配置模型",
            )
        );
    }
    let override_pool = session_model_override_snapshot(paths, None)?;
    print_model_choices(&config, &choices, override_pool.as_deref());
    println!(
        "{}",
        t(
            "switch with: miyu models <index|provider/model>; 'miyu models default' follows the global pool",
            "切换：miyu models <序号|供应商/模型>；miyu models default 恢复跟随全局模型池"
        )
    );
    Ok(())
}

pub(in crate::cli) fn print_model_choices(
    config: &AppConfig,
    choices: &[miyu_base::config::ProviderModelChoice],
    override_pool: Option<&[ActiveProviderModelConfig]>,
) {
    for (index, choice) in choices.iter().enumerate() {
        let active = match override_pool {
            Some(pool) => pool.iter().any(|model| {
                model.provider_id == choice.provider_id && model.model == choice.model
            }),
            None => config.is_active_provider_model(&choice.provider_id, &choice.model),
        };
        let marker = if active { "[*]" } else { "[ ]" };
        println!("{marker} {}. {}", index + 1, choice.label());
    }
    match override_pool {
        Some(_) => println!(
            "{}",
            t(
                "[*] = models pinned to the current session",
                "[*] = 当前会话固定使用的模型"
            )
        ),
        None => println!(
            "{}",
            t(
                "[*] = global active pool (the current session follows it)",
                "[*] = 全局激活模型池（当前会话跟随全局）"
            )
        ),
    }
}

/// Reads a session's model override straight from the shared state database;
/// works whether or not the daemon is running.
pub(in crate::cli) fn session_model_override_snapshot(
    paths: &MiyuPaths,
    session_id: Option<&str>,
) -> Result<Option<Vec<ActiveProviderModelConfig>>> {
    let store = StateStore::new(paths)?;
    let session_id = match session_id {
        Some(session_id) => session_id.to_string(),
        None => store.session_id().to_string(),
    };
    store.session_model_override(&session_id)
}

pub(in crate::cli) async fn set_session_models(
    paths: &MiyuPaths,
    session_id: Option<&str>,
    models: Vec<ActiveProviderModelConfig>,
) -> Result<()> {
    if ipc::daemon_info(paths).await.is_some() {
        let target = match session_id {
            Some(id) => ipc::SessionRef::Id { id: id.to_string() },
            None => ipc::SessionRef::Current,
        };
        send_ipc_command(paths, IpcCommand::SetSessionModels { target, models }).await?;
        return Ok(());
    }
    let config = AppConfig::load(paths)?;
    let choices = config.text_provider_model_choices();
    for model in &models {
        if !choices
            .iter()
            .any(|choice| choice.provider_id == model.provider_id && choice.model == model.model)
        {
            bail!(
                "{}{}/{}",
                t("unknown model: ", "未知模型："),
                model.provider_id,
                model.model
            );
        }
    }
    let store = StateStore::new(paths)?;
    let session_id = match session_id {
        Some(session_id) => session_id.to_string(),
        None => store.session_id().to_string(),
    };
    store.set_session_model_override(
        &session_id,
        (!models.is_empty()).then_some(models.as_slice()),
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::cli) enum VariantOutcome {
    Updated,
    Cancelled,
    Rejected(String),
}

pub(in crate::cli) fn run_variant(paths: &MiyuPaths, args: VariantArgs) -> Result<()> {
    let selected = args
        .name
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if selected.is_none() && (!io::stdin().is_terminal() || !io::stdout().is_terminal()) {
        bail!(
            "{}",
            t(
                "interactive thinking-level selection requires a terminal; use `miyu effort <name>`",
                "交互选择思考档位需要终端；请使用 `miyu effort <名称>`",
            )
        );
    }
    if !miyu_base::models_cache::is_loaded() {
        miyu_base::models_cache::refresh_blocking(paths).map_err(|error| {
            anyhow::anyhow!(
                "{}: {error:#}",
                t("failed to load model metadata", "无法加载模型元数据")
            )
        })?;
    }

    let mut config = AppConfig::load_or_default(paths)?;
    // 与 `miyu models` 同一个作用域：终端集成会话钉了自己的模型时，档位列的
    // 是那个模型的，不是全局文本模型的。
    let store = StateStore::new(paths)?;
    apply_session_model_override(&store, &mut config);
    let mut client = OpenAiCompatibleClient::from_config(&config, paths)?;
    match execute_variant(
        paths,
        &mut client,
        selected,
        "miyu effort",
        VariantScope::Global,
        inline_variant_select,
    )? {
        VariantOutcome::Updated => print_variant_updated(),
        VariantOutcome::Cancelled => {}
        VariantOutcome::Rejected(message) => bail!("{message}"),
    }
    Ok(())
}

/// 档位改到哪一份（09-24：effort 做成会话级）。终端会话里的 `/effort` 只改这个会话；
/// `miyu effort` 与直连模式改全局默认档。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::cli) enum VariantScope<'a> {
    Global,
    Session(&'a str),
}

/// `pick`：不带参数时的交互菜单——行内 `inline_variant_select`，全屏 TUI 用
/// `pick_effort`（面板贴在大厅提示下方 / 会话正文底部）。
///
/// `client` 进来时带的是全局档位；改完它带的是生效的那一档（会话钉了的用钉的），
/// 调用方拿它刷 footer。
pub(in crate::cli) fn execute_variant(
    paths: &MiyuPaths,
    client: &mut OpenAiCompatibleClient,
    selected: Option<&str>,
    selector_command: &str,
    scope: VariantScope<'_>,
    pick: impl FnOnce(VariantMenu) -> Result<Option<VariantSelections>>,
) -> Result<VariantOutcome> {
    let options = client.thinking_variant_options();
    let selections = if let Some(selected) = selected {
        if options.len() != 1 {
            let message = if is_zh() {
                format!("当前激活了多个模型；请使用 {selector_command} 在 TUI 中分别设置")
            } else {
                format!(
                    "multiple models are active; use {selector_command} and configure them in the TUI"
                )
            };
            return Ok(VariantOutcome::Rejected(message));
        }
        let option = &options[0];
        let variant = match resolve_variant_name(selected, &option.variants) {
            Ok(variant) => variant,
            Err(message) => return Ok(VariantOutcome::Rejected(message)),
        };
        // 会话里 `/effort default` 是钉成模型默认档，不是回到跟随全局（用户 09-24）。
        let variant = match scope {
            VariantScope::Session(_) => {
                variant.or_else(|| Some(miyu_core::llm::MODEL_DEFAULT_PIN.to_string()))
            }
            VariantScope::Global => variant,
        };
        vec![(option.provider_id.clone(), option.model.clone(), variant)]
    } else {
        let Some(menu) = variant_menu_for(paths, &options, scope) else {
            return Ok(VariantOutcome::Cancelled);
        };
        let Some(selections) = pick(menu)? else {
            return Ok(VariantOutcome::Cancelled);
        };
        selections
    };
    match scope {
        VariantScope::Global => {
            client.set_thinking_variants(&selections)?;
            client.save_thinking_variants(paths)?;
        }
        VariantScope::Session(session_id) => {
            // 会话那份只记钉住的：选「跟随全局」（值为空）就是拔掉钉子，选「默认」钉的是
            // 模型默认档（`MODEL_DEFAULT_PIN`）。
            let scope = miyu_core::llm::ThinkingVariantScope::Session(session_id);
            let mut pinned = miyu_core::llm::ThinkingVariantPreferences::load_scoped(paths, scope);
            for (provider_id, model, variant) in &selections {
                pinned.set(provider_id, model, variant.clone());
            }
            pinned.save_scoped(paths, scope)?;
            client.reload_thinking_variants(paths);
            client.apply_session_thinking_variants(paths, session_id);
        }
    }
    Ok(VariantOutcome::Updated)
}

/// 菜单：全局档照常；会话档里每个模型先是「跟随全局（全局那一档）」，再是模型默认档和
/// 各档位。光标先落在这个会话钉住的那一项上，没钉就落在「跟随全局」上——原样回车不会
/// 把跟随变成钉住。
fn variant_menu_for(
    paths: &MiyuPaths,
    options: &[ThinkingVariantOptions],
    scope: VariantScope<'_>,
) -> Option<VariantMenu> {
    let VariantScope::Session(session_id) = scope else {
        return VariantMenu::new(options);
    };
    let pinned = miyu_core::llm::ThinkingVariantPreferences::load_scoped(
        paths,
        miyu_core::llm::ThinkingVariantScope::Session(session_id),
    );
    let items = options
        .iter()
        .map(|option| {
            VariantMenuItem::for_session(
                option,
                pinned.selected(&option.provider_id, &option.model),
            )
        })
        .collect();
    VariantMenu::from_items(items)
}

pub(in crate::cli) fn resolve_variant_name(
    selected: &str,
    available: &[String],
) -> std::result::Result<Option<String>, String> {
    let explicit_variant = selected.strip_prefix("variant:");
    if explicit_variant.is_none() && selected.eq_ignore_ascii_case("default") {
        return Ok(None);
    }
    let selected = explicit_variant.unwrap_or(selected);
    available
        .iter()
        .find(|candidate| candidate.eq_ignore_ascii_case(selected))
        .cloned()
        .map(Some)
        .ok_or_else(|| {
            format!(
                "{}: {selected}",
                t("unknown thinking variant", "未知思考档位")
            )
        })
}

pub(in crate::cli) fn print_variant_updated() {
    println!("{}\n", t("thinking variants updated", "已更新思考档位"));
}

/// Direct (daemon-less) sessions read their pinned model pool straight from
/// the state store; daemon-run turns get the same treatment in the turn task.
/// 覆盖指向的模型全都解析不动时:清掉这条覆盖,永久退回全局池。
///
/// 只在日志里留痕,不打到终端上(08-28 用户点名):这是自愈,不是需要用户当场
/// 处理的事故,每次进 REPL 糊一行灰字纯属噪音。
///
/// 清掉而不是只在运行时忽略,同样是用户要的("模型丢失的话就把模型换成回退就
/// 行了"):留着一条永远失效的覆盖,footer 与实际用的模型会一直对不上,而且每
/// 次进来都要重算一遍。
pub(in crate::cli) fn drop_stale_model_override(store: &StateStore, session_id: &str) {
    if let Err(error) = store.set_session_model_override(session_id, None) {
        tracing::debug!(
            error = %error,
            "{}",
            t(
                "clearing the stale session model override failed",
                "清除失效的会话模型覆盖失败"
            )
        );
    }
}

pub(in crate::cli) fn apply_session_model_override(state: &StateStore, config: &mut AppConfig) {
    match state.session_model_override(&state.session_id()) {
        Ok(Some(models)) => match config.usable_model_override(models) {
            Some(usable) => config.active_provider_models = Some(usable),
            None => drop_stale_model_override(state, &state.session_id()),
        },
        Ok(None) => {}
        Err(error) => tracing::warn!(
            error = %error,
            "{}",
            t(
                "loading the session model override failed",
                "读取会话模型覆盖失败"
            )
        ),
    }
}
