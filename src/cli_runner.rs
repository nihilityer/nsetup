//! CLI 子命令执行与输出。

use crate::cli::{Cli, Commands, RpcCmd, ServiceCmd};
use crate::config::Config;
use crate::installer;
use crate::orchestrator::StackAction;
use crate::rpc::proto::{ContainerHealth, ContainerState, HealthResponse, PullProgress, Stack};
use crate::rpc::{self, RpcClient};
use crate::system_service::{self, ServiceAction};
use dialoguer::console::{Term, strip_ansi_codes};
use std::collections::BTreeMap;
use std::path::Path;

/// 根据 CLI 参数分发到对应子命令。
pub async fn run(cli: Cli) -> anyhow::Result<()> {
    match cli.command {
        Commands::Init { force } => installer::init(force),
        Commands::Daemon => rpc::serve(Config::load_or_default()?).await,
        Commands::Service { action } => run_service_action(action),
        Commands::Infra { action } => generated::run_infra(action).await,
        Commands::App { action } => generated::run_app(action).await,
        Commands::Rpc {
            endpoint,
            token_file,
            action,
        } => run_rpc(endpoint.as_deref(), token_file.as_deref(), action).await,
        Commands::Deploy {
            name,
            compose,
            env_file,
            start,
        } => {
            let (compose_yaml, env_content) = read_deploy_files(&compose, env_file.as_deref())?;
            let mut client = RpcClient::connect(None, None).await?;
            log_message(
                &client
                    .deploy(name, compose_yaml, env_content, start)
                    .await?
                    .message,
            );
            Ok(())
        }
        Commands::Logs { app, tail, follow } => {
            let mut client = RpcClient::connect(None, None).await?;
            run_logs(&mut client, app, tail, follow).await
        }
        Commands::List => {
            let mut client = RpcClient::connect(None, None).await?;
            for stack in client.list_stacks().await?.stacks {
                tracing::info!("{}  容器: {}", stack.name, stack.containers.len());
            }
            Ok(())
        }
        Commands::Start { app } => run_remote_action(app, StackAction::Start).await,
        Commands::Stop { app } => run_remote_action(app, StackAction::Stop).await,
        Commands::Restart { app } => run_remote_action(app, StackAction::Restart).await,
        Commands::Upgrade {
            app,
            service,
            version,
        } => run_upgrade(app, service, version).await,
        Commands::Pull { app } => run_default_pull(app).await,
        Commands::Build { app } => run_remote_action(app, StackAction::Build).await,
        Commands::Show { app } => {
            let mut client = RpcClient::connect(None, None).await?;
            show_stack(client.get_stack(app).await?, true);
            Ok(())
        }
        Commands::Remove { app, force, purge } => remove(app, force, purge).await,
        Commands::Status => {
            let mut client = RpcClient::connect(None, None).await?;
            show_health(&client.health().await?);
            Ok(())
        }
    }
}

mod generated;

/// 使用显式端点执行底层 RPC 命令。
async fn run_rpc(
    endpoint: Option<&str>,
    token_file: Option<&Path>,
    action: RpcCmd,
) -> anyhow::Result<()> {
    let mut client = RpcClient::connect(endpoint, token_file).await?;
    match action {
        RpcCmd::Health => show_health(&client.health().await?),
        RpcCmd::List => {
            for stack in client.list_stacks().await?.stacks {
                tracing::info!(
                    "{}  容器: {}  目录: {}",
                    stack.name,
                    stack.containers.len(),
                    stack.project_directory
                );
            }
        }
        RpcCmd::Deploy {
            name,
            compose,
            env_file,
            start,
        } => {
            let (compose_yaml, env_content) = read_deploy_files(&compose, env_file.as_deref())?;
            log_message(
                &client
                    .deploy(name, compose_yaml, env_content, start)
                    .await?
                    .message,
            );
        }
        RpcCmd::Remove { name, volumes } => {
            log_message(&client.remove(name, volumes).await?.message);
        }
        RpcCmd::Start { name } => run_action(&mut client, name, StackAction::Start).await?,
        RpcCmd::Stop { name } => run_action(&mut client, name, StackAction::Stop).await?,
        RpcCmd::Restart { name } => run_action(&mut client, name, StackAction::Restart).await?,
        RpcCmd::Upgrade {
            name,
            service,
            version,
        } => log_message(
            &client
                .upgrade_application(name, service, version)
                .await?
                .message,
        ),
        RpcCmd::Pull { name } => run_pull(&mut client, name).await?,
        RpcCmd::Build { name } => run_action(&mut client, name, StackAction::Build).await?,
        RpcCmd::Show { name } => show_stack(client.get_stack(name).await?, false),
        RpcCmd::Logs { name, tail, follow } => run_logs(&mut client, name, tail, follow).await?,
    }
    Ok(())
}

/// 将 CLI 服务枚举映射为 systemd 服务操作。
fn run_service_action(action: ServiceCmd) -> anyhow::Result<()> {
    let action = match action {
        ServiceCmd::Start => ServiceAction::Start,
        ServiceCmd::Stop => ServiceAction::Stop,
        ServiceCmd::Restart => ServiceAction::Restart,
        ServiceCmd::Status => ServiceAction::Status,
    };
    system_service::control(action)
}

/// 通过默认本机端点执行应用生命周期操作。
async fn run_remote_action(app: String, action: StackAction) -> anyhow::Result<()> {
    let mut client = RpcClient::connect(None, None).await?;
    run_action(&mut client, app, action).await
}

/// 通过默认本机端点拉取项目镜像。
async fn run_default_pull(app: String) -> anyhow::Result<()> {
    let mut client = RpcClient::connect(None, None).await?;
    run_pull(&mut client, app).await
}

/// 设置应用服务的镜像版本并完成拉取与重新创建。
async fn run_upgrade(app: String, service: Option<String>, version: String) -> anyhow::Result<()> {
    let mut client = RpcClient::connect(None, None).await?;
    log_message(
        &client
            .upgrade_application(app, service, version)
            .await?
            .message,
    );
    Ok(())
}

/// 使用已有 RPC 客户端执行应用生命周期操作。
async fn run_action(
    client: &mut RpcClient,
    name: String,
    action: StackAction,
) -> anyhow::Result<()> {
    log_message(&client.action(name, action).await?.message);
    Ok(())
}

/// 拉取镜像并持续刷新 Docker 报告的字节进度。
async fn run_pull(client: &mut RpcClient, name: String) -> anyhow::Result<()> {
    let mut stream = client.pull(name.clone()).await?;
    let mut display = PullDisplay::new(name);
    display.render()?;
    loop {
        match stream.message().await {
            Ok(Some(progress)) => display.update(progress)?,
            Ok(None) => {
                display.finish()?;
                return Ok(());
            }
            Err(error) => {
                display.clear()?;
                return Err(error.into());
            }
        }
    }
}

/// 逐行输出 Compose 日志，不添加 nsetup 自身的日志前缀。
async fn run_logs(
    client: &mut RpcClient,
    name: String,
    tail: u32,
    follow: bool,
) -> anyhow::Result<()> {
    let mut stream = client.logs(name, tail, follow).await?;
    let terminal = Term::stdout();
    while let Some(line) = stream.message().await? {
        let content = strip_ansi_codes(line.content.trim_end_matches('\r'));
        terminal.write_line(&content)?;
    }
    Ok(())
}

/// 单个 Docker 拉取进度节点的最新计数。
#[derive(Debug, Clone, Copy, Default)]
struct PullNode {
    /// 当前已处理字节数。
    current: u64,
    /// 总字节数。
    total: u64,
}

/// 在终端中聚合并渲染服务端拉取事件。
#[derive(Debug)]
struct PullDisplay {
    /// 终端输出句柄。
    terminal: Term,
    /// 正在拉取的项目名。
    name: String,
    /// 按 Docker 节点标识保存的字节计数。
    nodes: BTreeMap<String, PullNode>,
    /// 最近一次 Docker 操作说明。
    activity: String,
}

impl PullDisplay {
    /// 创建项目拉取进度显示器。
    fn new(name: String) -> Self {
        Self {
            terminal: Term::stderr(),
            name,
            nodes: BTreeMap::new(),
            activity: String::from("等待 Docker 返回进度"),
        }
    }

    /// 合并一个 Docker 事件并刷新进度行。
    fn update(&mut self, progress: PullProgress) -> anyhow::Result<()> {
        let key = if progress.id.is_empty() {
            format!("{}:{}", progress.text, progress.status)
        } else {
            progress.id.clone()
        };
        self.nodes.insert(
            key,
            PullNode {
                current: progress.current,
                total: progress.total,
            },
        );
        self.activity = [progress.text, progress.id, progress.status]
            .into_iter()
            .find(|value| !value.is_empty())
            .unwrap_or_else(|| String::from("拉取中"));
        self.render()
    }

    /// 清除当前进度行。
    fn clear(&self) -> anyhow::Result<()> {
        if self.terminal.is_term() {
            self.terminal.clear_line()?;
        }
        Ok(())
    }

    /// 标记整个拉取流成功完成。
    fn finish(&self) -> anyhow::Result<()> {
        if self.terminal.is_term() {
            self.terminal.clear_line()?;
            self.terminal.write_line(&format!(
                "[{}] 100%  项目 {} 镜像拉取完成",
                "=".repeat(30),
                self.name
            ))?;
        } else {
            tracing::info!("项目 {} 镜像拉取完成", self.name);
        }
        Ok(())
    }

    /// 按已知总量计算并输出聚合进度。
    fn render(&self) -> anyhow::Result<()> {
        if !self.terminal.is_term() {
            return Ok(());
        }
        let (current, total, percent) = aggregate_pull_progress(&self.nodes);
        let line = if total > 0 {
            let filled = usize::from(percent) * 30 / 100;
            format!(
                "[{}{}] {percent:>3}%  {}/{}  {}",
                "=".repeat(filled),
                " ".repeat(30 - filled),
                format_bytes(current),
                format_bytes(total),
                self.activity
            )
        } else {
            format!("[{}]  --  {}", " ".repeat(30), self.activity)
        };
        self.terminal.clear_line()?;
        self.terminal.write_str(&line)?;
        self.terminal.flush()?;
        Ok(())
    }
}

/// 聚合 Docker 已明确提供总量的进度节点。
fn aggregate_pull_progress(nodes: &BTreeMap<String, PullNode>) -> (u64, u64, u8) {
    let (current, total) = nodes.values().filter(|node| node.total > 0).fold(
        (0_u64, 0_u64),
        |(current, total), node| {
            (
                current.saturating_add(node.current.min(node.total)),
                total.saturating_add(node.total),
            )
        },
    );
    let percent = if total == 0 {
        0
    } else {
        u8::try_from((u128::from(current) * 100) / u128::from(total)).unwrap_or(100)
    };
    (current, total, percent)
}

/// 将字节数格式化为紧凑的二进制单位。
fn format_bytes(bytes: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = KIB * 1024;
    const GIB: u64 = MIB * 1024;
    if bytes >= GIB {
        format!("{:.1} GiB", bytes as f64 / GIB as f64)
    } else if bytes >= MIB {
        format!("{:.1} MiB", bytes as f64 / MIB as f64)
    } else if bytes >= KIB {
        format!("{:.1} KiB", bytes as f64 / KIB as f64)
    } else {
        format!("{bytes} B")
    }
}

/// 确认并删除应用。
async fn remove(app: String, force: bool, purge: bool) -> anyhow::Result<()> {
    if !force
        && !dialoguer::Confirm::new()
            .with_prompt(format!("确认删除项目 {app}？"))
            .default(false)
            .interact()?
    {
        tracing::info!("已取消");
        return Ok(());
    }
    let mut client = RpcClient::connect(None, None).await?;
    log_message(&client.remove(app, purge).await?.message);
    Ok(())
}

/// 读取待部署的 Compose 和环境变量文件。
fn read_deploy_files(compose: &Path, env_file: Option<&Path>) -> anyhow::Result<(String, String)> {
    let compose_yaml = std::fs::read_to_string(compose)
        .map_err(|error| anyhow::anyhow!("无法读取 Compose 文件 {}: {error}", compose.display()))?;
    let env_content = env_file
        .map(std::fs::read_to_string)
        .transpose()
        .map_err(|error| anyhow::anyhow!("无法读取环境变量文件: {error}"))?
        .unwrap_or_default();
    Ok((compose_yaml, env_content))
}

/// 输出守护进程健康状态。
fn show_health(health: &HealthResponse) {
    tracing::info!("守护进程版本: {}", health.version);
    tracing::info!("Docker 可用: {}", health.docker_available);
    tracing::info!("Compose 项目目录: {}", health.compose_root);
}

/// 输出 Compose 项目信息。
fn show_stack(stack: Stack, detailed: bool) {
    tracing::info!("项目: {}", stack.name);
    tracing::info!("目录: {}", stack.project_directory);
    if detailed {
        tracing::info!("Compose: {}", stack.compose_file);
        tracing::info!("环境变量: {}", stack.env_file);
    }
    for container in stack.containers {
        tracing::info!(
            "{}  {}  状态: {}  健康: {}",
            container.service,
            container.name,
            container_state_label(container.state),
            container_health_label(container.health)
        );
    }
}

/// 输出服务端操作结果。
fn log_message(message: &str) {
    tracing::info!("{}", message);
}

/// 将协议容器状态转换为中文标签。
fn container_state_label(value: i32) -> &'static str {
    match ContainerState::try_from(value).unwrap_or(ContainerState::Unspecified) {
        ContainerState::Unspecified => "未知",
        ContainerState::Created => "已创建",
        ContainerState::Running => "运行中",
        ContainerState::Paused => "已暂停",
        ContainerState::Restarting => "重启中",
        ContainerState::Removing => "删除中",
        ContainerState::Exited => "已退出",
        ContainerState::Dead => "不可恢复",
    }
}

/// 将协议健康状态转换为中文标签。
fn container_health_label(value: i32) -> &'static str {
    match ContainerHealth::try_from(value).unwrap_or(ContainerHealth::Unknown) {
        ContainerHealth::Unspecified => "未配置",
        ContainerHealth::Starting => "检查中",
        ContainerHealth::Healthy => "健康",
        ContainerHealth::Unhealthy => "不健康",
        ContainerHealth::Unknown => "未知",
    }
}
