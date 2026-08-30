//! Clap 命令界面与客户端请求构造。

use crate::constants::{MAX_CONFIG_SIZE, MAX_RPC_MESSAGE_SIZE};
use crate::install::{InstallOptions, install};
use crate::rpc::proto;
use crate::rpc::{Action, RpcClient};
use crate::spec::{BindMount, PortProtocol, PublishedPort};
use crate::template::{TemplateKind, skeleton};
use anyhow::Context;
use clap::{Arg, ArgAction, Args, CommandFactory, FromArgMatches, Parser, Subcommand, ValueEnum};
use dialoguer::Confirm;
use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Component, Path, PathBuf};
use tokio_stream::StreamExt;

/// Linux 主机与 Docker Compose 项目管理工具。
#[derive(Debug, Parser)]
#[command(name = "nsetup", version, about)]
pub struct Cli {
    /// 远程 daemon 端点；默认连接本机 Unix socket。
    #[arg(long, global = true)]
    pub endpoint: Option<String>,
    /// TCP 端点要求的 Bearer 令牌文件。
    #[arg(long, global = true)]
    pub token_file: Option<PathBuf>,
    /// 要执行的操作。
    #[command(subcommand)]
    pub command: Command,
}

impl Cli {
    /// 使用完整中文帮助信息解析命令行。
    #[must_use]
    pub fn parse_localized() -> Self {
        let matches = localized_command(Self::command()).get_matches();
        match Self::from_arg_matches(&matches) {
            Ok(cli) => cli,
            Err(error) => error.exit(),
        }
    }
}

/// 将 Clap 命令树中的帮助章节、帮助参数与版本参数本地化为中文。
fn localized_command(mut command: clap::Command) -> clap::Command {
    let has_version = command.get_version().is_some();
    command = command
        .disable_help_subcommand(true)
        .disable_help_flag(true)
        .disable_version_flag(true)
        .subcommand_help_heading("命令")
        .subcommand_value_name("命令")
        .help_template("{before-help}{about-with-newline}用法：{usage}\n\n{all-args}{after-help}")
        .mut_args(|argument| {
            if argument.is_positional() {
                argument.help_heading("参数")
            } else {
                argument.help_heading("选项")
            }
        })
        .arg(
            Arg::new("help")
                .short('h')
                .long("help")
                .action(ArgAction::Help)
                .help_heading("选项")
                .help("显示帮助"),
        );
    if has_version {
        command = command.arg(
            Arg::new("version")
                .short('V')
                .long("version")
                .action(ArgAction::Version)
                .help_heading("选项")
                .help("显示版本"),
        );
    }
    for subcommand in command.get_subcommands_mut() {
        *subcommand = localized_command(subcommand.clone());
    }
    command
}

/// nsetup 顶层命令。
#[derive(Debug, Subcommand)]
pub enum Command {
    /// 安装或替换 daemon 与 systemd unit。
    Init(InitArgs),
    /// 显示 daemon 与 Docker 状态。
    Status,
    /// 输出带注释的 TOML 模板骨架。
    Template(TemplateArgs),
    /// 应用 TOML 模板声明。
    Up(UpArgs),
    /// 导入受支持的 Compose YAML 项目。
    Import(ImportArgs),
    /// 将当前状态导出为 TOML。
    Export(ExportArgs),
    /// 局部编辑单个服务。
    Edit(Box<EditArgs>),
    /// 列出受管项目。
    List,
    /// 显示单个受管项目。
    Show(NameArgs),
    /// 启动项目。
    Start(NameArgs),
    /// 停止项目。
    Stop(NameArgs),
    /// 重启项目。
    Restart(NameArgs),
    /// 拉取项目镜像并显示进度。
    Pull(NameArgs),
    /// 构建项目镜像。
    Build(NameArgs),
    /// 读取或跟随项目日志。
    Logs(LogsArgs),
    /// 停止并删除项目，同时保留 bind mount 数据。
    #[command(name = "rm")]
    Remove(RemoveArgs),
    /// 为 systemd 运行特权 daemon。
    #[command(hide = true)]
    Daemon,
}

/// `init` 接受的参数。
#[derive(Debug, Args)]
pub struct InitArgs {
    /// 短路由主机名拼接的基础域名。
    #[arg(long)]
    pub domain: Option<String>,
    /// Compose 项目根目录。
    #[arg(long)]
    pub stacks_root: Option<PathBuf>,
    /// 允许的 bind mount 根目录；可重复指定多个目录。
    #[arg(long = "data-root")]
    pub data_roots: Vec<PathBuf>,
    /// daemon 监听 URI 或 TCP 地址。
    #[arg(long)]
    pub listen: Option<String>,
    /// Docker daemon 的 Unix socket。
    #[arg(long)]
    pub docker_socket: Option<PathBuf>,
    /// 替换现有安装，同时保留未指定的配置值。
    #[arg(long)]
    pub force: bool,
}

/// `template` 接受的参数。
#[derive(Debug, Args)]
pub struct TemplateArgs {
    /// 模板名称；可选值为 `app`、`traefik`、`static`，默认为 `app`。
    #[arg(default_value = "app", hide_default_value = true)]
    pub name: String,
}

/// `up` 接受的参数。
#[derive(Debug, Args)]
pub struct UpArgs {
    /// TOML 配置文件。
    #[arg(short = 'f', long = "file")]
    pub file: PathBuf,
    /// 静态站点资源目录。
    #[arg(long)]
    pub assets: Option<PathBuf>,
    /// 应用成功后启动项目。
    #[arg(long)]
    pub start: bool,
    /// 替换现有项目。
    #[arg(long)]
    pub force: bool,
}

/// `import` 接受的参数。
#[derive(Debug, Args)]
pub struct ImportArgs {
    /// 目标项目名。
    pub name: String,
    /// Compose YAML 文件。
    #[arg(short = 'f', long = "file")]
    pub file: PathBuf,
    /// 可选的 Compose `.env` 文件。
    #[arg(long)]
    pub env_file: Option<PathBuf>,
    /// 导入后启动项目。
    #[arg(long)]
    pub start: bool,
}

/// `export` 接受的参数。
#[derive(Debug, Args)]
pub struct ExportArgs {
    /// 项目名。
    pub name: String,
    /// 输出文件；省略时写入标准输出。
    #[arg(short = 'o', long)]
    pub output: Option<PathBuf>,
}

/// 简单项目命令共用的参数。
#[derive(Debug, Args)]
pub struct NameArgs {
    /// 项目名。
    pub name: String,
}

/// `logs` 接受的参数。
#[derive(Debug, Args)]
pub struct LogsArgs {
    /// 项目名。
    pub name: String,
    /// 返回的历史日志行数，默认为 100。
    #[arg(long, default_value_t = 100, hide_default_value = true)]
    pub tail: u32,
    /// 持续跟随新增日志行。
    #[arg(short = 'f', long)]
    pub follow: bool,
}

/// `rm` 接受的参数。
#[derive(Debug, Args)]
pub struct RemoveArgs {
    /// 项目名。
    pub name: String,
    /// 跳过交互确认。
    #[arg(long)]
    pub force: bool,
}

/// `edit` 接受的参数。
#[derive(Debug, Args)]
pub struct EditArgs {
    /// 项目名。
    pub name: String,
    /// 服务名；多服务项目必须指定。
    #[arg(long)]
    pub service: Option<String>,
    /// 不含标签的镜像仓库。
    #[arg(long)]
    pub image: Option<String>,
    /// 明确的镜像版本标签。
    #[arg(long)]
    pub version: Option<String>,
    /// 完整替换命令参数。
    #[arg(long, num_args = 1..)]
    pub command: Vec<String>,
    /// 默认路由容器端口。
    #[arg(long = "port")]
    pub container_port: Option<u16>,
    /// 新路由主机名；可重复指定多个主机名。
    #[arg(long = "host")]
    pub hosts: Vec<String>,
    /// 应用于新路由的 URL 路径前缀。
    #[arg(long)]
    pub path_prefix: Option<String>,
    /// 新路由中间件；可重复指定，支持 `gzip`、`forwarded-headers`、`internal-only`、`tls`。
    #[arg(long = "middleware", hide_possible_values = true)]
    pub middlewares: Vec<MiddlewareArg>,
    /// 新路由使用的后端协议；支持 `http`、`https`、`h2c`，默认为 `http`。
    #[arg(
        long,
        default_value = "http",
        hide_default_value = true,
        hide_possible_values = true
    )]
    pub protocol: ProtocolArg,
    /// 为新路由启用粘性 Cookie。
    #[arg(long)]
    pub sticky_cookie: bool,
    /// 覆盖新路由的 `passHostHeader` 设置，取值为 `true` 或 `false`。
    #[arg(long, hide_possible_values = true)]
    pub pass_host_header: Option<bool>,
    /// 新路由的优先级。
    #[arg(long)]
    pub priority: Option<u32>,
    /// `HOST:CONTAINER[/tcp|udp]` 格式的发布端口。
    #[arg(long = "publish")]
    pub published_ports: Vec<String>,
    /// `HOST:CONTAINER[:ro]` 格式的 bind mount。
    #[arg(long = "volume")]
    pub volumes: Vec<String>,
    /// `KEY=VALUE` 格式的环境变量。
    #[arg(long = "env")]
    pub environment: Vec<String>,
    /// 新的逻辑网络模式；支持 `bridge`、`host`、`external`。
    #[arg(long, hide_possible_values = true)]
    pub network: Option<NetworkArg>,
    /// 与 `--network external` 一起使用的 Docker 网络名。
    #[arg(long)]
    pub external_network: Option<String>,
    /// `KEY=VALUE` 格式的自定义 Docker label。
    #[arg(long = "label")]
    pub labels: Vec<String>,
    /// `CMD-SHELL` 形式的健康检查命令。
    #[arg(long)]
    pub healthcheck_cmd: Option<String>,
    /// 健康检查间隔。
    #[arg(long)]
    pub healthcheck_interval: Option<String>,
    /// 健康检查超时。
    #[arg(long)]
    pub healthcheck_timeout: Option<String>,
    /// 健康检查启动宽限期。
    #[arg(long)]
    pub healthcheck_start_period: Option<String>,
    /// 健康检查重试次数。
    #[arg(long)]
    pub healthcheck_retries: Option<u32>,
    /// 移除当前健康检查。
    #[arg(long)]
    pub remove_healthcheck: bool,
    /// 启动编辑后的服务。
    #[arg(long)]
    pub start: bool,
}

/// CLI 网络选项。
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum NetworkArg {
    /// Compose bridge 网络。
    Bridge,
    /// 宿主机网络。
    Host,
    /// 指定名称的外部网络。
    External,
}

/// CLI 路由协议选项。
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum ProtocolArg {
    /// 普通 HTTP。
    Http,
    /// 连接后端的 HTTPS。
    Https,
    /// 明文 HTTP/2。
    H2c,
}

/// CLI 中间件选项。
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum MiddlewareArg {
    /// 响应压缩。
    Gzip,
    /// 转发请求头。
    ForwardedHeaders,
    /// 内网地址白名单。
    InternalOnly,
    /// TLS 安全响应头。
    Tls,
}

/// 执行已解析的命令。
///
/// # 错误
///
/// 输入无效、文件操作失败、RPC 失败或 daemon 操作失败时返回错误。
pub async fn run(cli: Cli) -> anyhow::Result<()> {
    match cli.command {
        Command::Init(args) => {
            install(InstallOptions {
                domain: args.domain,
                stacks_root: args.stacks_root,
                data_roots: (!args.data_roots.is_empty()).then_some(args.data_roots),
                listen: args.listen,
                docker_socket: args.docker_socket,
                force: args.force,
            })?;
            write_line("nsetup daemon 已安装并启动")?;
            return Ok(());
        }
        Command::Template(args) => {
            let kind = TemplateKind::parse(&args.name)?;
            write_text(skeleton(kind))?;
            return Ok(());
        }
        Command::Daemon => {
            let config = crate::config::Config::load()?;
            return crate::rpc::serve(config).await;
        }
        _ => {}
    }
    let mut client = RpcClient::connect(cli.endpoint.as_deref(), cli.token_file.as_deref()).await?;
    dispatch_remote(&mut client, cli.command).await
}

/// 建立 daemon 连接后分发单个命令。
///
/// # 错误
///
/// 文件、输入、流式传输或 RPC 操作失败时返回错误。
async fn dispatch_remote(client: &mut RpcClient, command: Command) -> anyhow::Result<()> {
    match command {
        Command::Status => {
            let status = client.status().await?;
            write_line(&format!(
                "版本: {}\nDocker: {}\n项目根目录: {}\n主域名: {}",
                status.version,
                if status.docker_available {
                    "可用"
                } else {
                    "不可用"
                },
                status.stacks_root,
                status.domain
            ))?;
        }
        Command::Up(args) => {
            let config_toml = read_limited(&args.file, MAX_CONFIG_SIZE)?;
            let assets = args
                .assets
                .as_deref()
                .map(read_assets)
                .transpose()?
                .unwrap_or_default();
            let response = client
                .apply(proto::ApplyRequest {
                    config_toml,
                    assets,
                    start: args.start,
                    force: args.force,
                })
                .await?;
            write_line(&response.message)?;
        }
        Command::Import(args) => {
            let compose_yaml = read_limited(&args.file, MAX_CONFIG_SIZE)?;
            let env_file = args
                .env_file
                .as_deref()
                .map(|path| read_limited(path, MAX_CONFIG_SIZE))
                .transpose()?;
            let response = client
                .import_compose(proto::ImportComposeRequest {
                    name: args.name,
                    compose_yaml,
                    env_file,
                    start: args.start,
                })
                .await?;
            write_line(&response.message)?;
        }
        Command::Export(args) => {
            let response = client.export(args.name).await?;
            if let Some(path) = args.output {
                write_new_file(&path, response.config_toml.as_bytes())?;
            } else {
                write_text(&response.config_toml)?;
            }
        }
        Command::Edit(args) => {
            let response = client.edit(edit_request(*args)?).await?;
            write_line(&response.message)?;
        }
        Command::List => {
            let response = client.list().await?;
            if response.stacks.is_empty() {
                write_line("没有受管项目")?;
            } else {
                for stack in response.stacks {
                    write_line(&format!(
                        "{}\t{}\t{}",
                        stack.name,
                        stack.services.join(","),
                        compact_status(&stack.status)
                    ))?;
                }
            }
        }
        Command::Show(args) => {
            let stack = client.get(args.name).await?;
            write_text(&format!(
                "项目名: {}\n服务: {}\n状态: {}\n\n{}",
                stack.name,
                stack.services.join(", "),
                stack.status,
                stack.compose_yaml
            ))?;
        }
        Command::Start(args) => write_operation(client.action(args.name, Action::Start).await?)?,
        Command::Stop(args) => write_operation(client.action(args.name, Action::Stop).await?)?,
        Command::Restart(args) => {
            write_operation(client.action(args.name, Action::Restart).await?)?;
        }
        Command::Build(args) => write_operation(client.action(args.name, Action::Build).await?)?,
        Command::Pull(args) => {
            let mut stream = client.pull(args.name).await?;
            while let Some(progress) = stream.next().await.transpose()? {
                write_line(&format!(
                    "{}\t{}\t{}\t{}/{}",
                    progress.id, progress.status, progress.text, progress.current, progress.total
                ))?;
            }
        }
        Command::Logs(args) => {
            let mut stream = client.logs(args.name, args.tail, args.follow).await?;
            while let Some(line) = stream.next().await.transpose()? {
                write_line(&line.line)?;
            }
        }
        Command::Remove(args) => {
            let confirmed = args.force
                || Confirm::new()
                    .with_prompt(format!(
                        "停止并删除项目 {}？bind mount 数据会保留",
                        args.name
                    ))
                    .default(false)
                    .interact()?;
            if confirmed {
                write_operation(client.remove(args.name).await?)?;
            } else {
                write_line("已取消")?;
            }
        }
        Command::Init(_) | Command::Template(_) | Command::Daemon => {
            anyhow::bail!("本地命令被错误地发送到 daemon 分发器");
        }
    }
    Ok(())
}

/// 将 CLI 编辑标志转换为类型化协议请求。
///
/// # 错误
///
/// 端口、挂载、环境变量或标志组合格式错误时返回错误。
fn edit_request(args: EditArgs) -> anyhow::Result<proto::EditRequest> {
    if args.hosts.is_empty()
        && (args.path_prefix.is_some()
            || args.sticky_cookie
            || args.pass_host_header.is_some()
            || args.priority.is_some())
    {
        anyhow::bail!("路由专用参数需要至少一个 --host");
    }
    let middleware_values: Vec<i32> = args
        .middlewares
        .iter()
        .copied()
        .map(middleware_value)
        .collect();
    let routes = if args.hosts.is_empty() {
        Vec::new()
    } else {
        vec![proto::Route {
            hosts: args.hosts,
            path_prefix: args.path_prefix,
            container_port: u32::from(
                args.container_port
                    .ok_or_else(|| anyhow::anyhow!("新增 --host 时必须提供 --port"))?,
            ),
            middlewares: middleware_values.clone(),
            protocol: protocol_value(args.protocol),
            sticky_cookie: args.sticky_cookie,
            pass_host_header: args.pass_host_header,
            priority: args.priority,
        }]
    };
    let published_ports = args
        .published_ports
        .iter()
        .map(|value| PublishedPort::parse(value).map(port_to_proto))
        .collect::<anyhow::Result<_>>()?;
    let volumes = args
        .volumes
        .iter()
        .map(|value| BindMount::parse(value).map(volume_to_proto))
        .collect::<anyhow::Result<_>>()?;
    let environment = parse_pairs(&args.environment, "环境变量")?;
    validate_pairs(&args.labels, "Docker label")?;
    let network_mode = args.network.map(network_value);
    if matches!(args.network, Some(NetworkArg::External)) && args.external_network.is_none() {
        anyhow::bail!("--network external 必须同时提供 --external-network");
    }
    if !matches!(args.network, Some(NetworkArg::External)) && args.external_network.is_some() {
        anyhow::bail!("--external-network 只能与 --network external 一起使用");
    }
    let health_fields_present = args.healthcheck_interval.is_some()
        || args.healthcheck_timeout.is_some()
        || args.healthcheck_start_period.is_some()
        || args.healthcheck_retries.is_some();
    if health_fields_present && args.healthcheck_cmd.is_none() {
        anyhow::bail!("健康检查参数需要 --healthcheck-cmd");
    }
    let healthcheck = args.healthcheck_cmd.map(|command| proto::Healthcheck {
        command,
        interval: args.healthcheck_interval,
        timeout: args.healthcheck_timeout,
        start_period: args.healthcheck_start_period,
        retries: args.healthcheck_retries,
    });
    Ok(proto::EditRequest {
        name: args.name,
        service: args.service,
        image: args.image,
        version: args.version,
        command: args.command,
        container_port: args.container_port.map(u32::from),
        routes,
        published_ports,
        volumes,
        environment,
        network_mode,
        external_network: args.external_network,
        middlewares: middleware_values,
        labels: args.labels,
        healthcheck,
        remove_healthcheck: args.remove_healthcheck,
        start: args.start,
    })
}

/// 将语义端口映射转换为 protobuf 表示。
fn port_to_proto(value: PublishedPort) -> proto::PublishedPort {
    proto::PublishedPort {
        host_ip: value.host_ip,
        host_port: u32::from(value.host_port),
        container_port: u32::from(value.container_port),
        protocol: match value.protocol {
            PortProtocol::Tcp => proto::PortProtocol::Tcp as i32,
            PortProtocol::Udp => proto::PortProtocol::Udp as i32,
        },
    }
}

/// 将语义 bind mount 转换为 protobuf 表示。
fn volume_to_proto(value: BindMount) -> proto::Volume {
    proto::Volume {
        host_path: value.host_path,
        container_path: value.container_path,
        read_only: value.read_only,
    }
}

/// 将 CLI 网络选项转换为 protobuf 数值。
const fn network_value(value: NetworkArg) -> i32 {
    match value {
        NetworkArg::Bridge => proto::NetworkMode::Bridge as i32,
        NetworkArg::Host => proto::NetworkMode::Host as i32,
        NetworkArg::External => proto::NetworkMode::External as i32,
    }
}

/// 将 CLI 协议选项转换为 protobuf 数值。
const fn protocol_value(value: ProtocolArg) -> i32 {
    match value {
        ProtocolArg::Http => proto::RouteProtocol::Http as i32,
        ProtocolArg::Https => proto::RouteProtocol::Https as i32,
        ProtocolArg::H2c => proto::RouteProtocol::H2c as i32,
    }
}

/// 将 CLI 中间件选项转换为 protobuf 数值。
const fn middleware_value(value: MiddlewareArg) -> i32 {
    match value {
        MiddlewareArg::Gzip => proto::Middleware::Gzip as i32,
        MiddlewareArg::ForwardedHeaders => proto::Middleware::ForwardedHeaders as i32,
        MiddlewareArg::InternalOnly => proto::Middleware::InternalOnly as i32,
        MiddlewareArg::Tls => proto::Middleware::Tls as i32,
    }
}

/// 将重复的 `KEY=VALUE` CLI 参数解析为 protobuf 映射。
///
/// # 错误
///
/// 缺少分隔符、键为空或键重复时返回错误。
fn parse_pairs(values: &[String], label: &str) -> anyhow::Result<HashMap<String, String>> {
    let mut output = HashMap::new();
    for value in values {
        let (key, content) = value
            .split_once('=')
            .ok_or_else(|| anyhow::anyhow!("{label}必须是 KEY=VALUE: {value}"))?;
        if key.is_empty() {
            anyhow::bail!("{label}键不能为空");
        }
        if output
            .insert(key.to_string(), content.to_string())
            .is_some()
        {
            anyhow::bail!("{label}重复: {key}");
        }
    }
    Ok(output)
}

/// 校验重复的 `KEY=VALUE` 参数并保留原始顺序。
///
/// # 错误
///
/// 键格式错误或重复时返回错误。
fn validate_pairs(values: &[String], label: &str) -> anyhow::Result<()> {
    let _pairs = parse_pairs(values, label)?;
    Ok(())
}

/// 在严格大小限制下读取 UTF-8 文本文件。
///
/// # 错误
///
/// 元数据读取、大小检查、文件读取或 UTF-8 解码失败时返回错误。
fn read_limited(path: &Path, limit: usize) -> anyhow::Result<String> {
    let metadata =
        std::fs::metadata(path).with_context(|| format!("无法读取文件信息: {}", path.display()))?;
    if metadata.len() > u64::try_from(limit)? {
        anyhow::bail!("文件超过 {} 字节限制: {}", limit, path.display());
    }
    std::fs::read_to_string(path).with_context(|| format!("无法读取文件: {}", path.display()))
}

/// 从静态站点目录递归加载普通文件。
///
/// # 错误
///
/// 遇到符号链接、特殊文件、不安全路径或超大上传时返回错误。
fn read_assets(root: &Path) -> anyhow::Result<Vec<proto::Asset>> {
    if !root.is_dir() {
        anyhow::bail!("assets 不是目录: {}", root.display());
    }
    let mut output = Vec::new();
    read_assets_at(root, root, &mut output)?;
    output.sort_by(|left, right| left.path.cmp(&right.path));
    let total: usize = output.iter().map(|asset| asset.content.len()).sum();
    if total > MAX_RPC_MESSAGE_SIZE - MAX_CONFIG_SIZE {
        anyhow::bail!("静态站点文件总大小超过 RPC 限制");
    }
    Ok(output)
}

/// 遍历一层静态资源目录且不跟随符号链接。
///
/// # 错误
///
/// 文件系统条目不安全或类型不受支持时返回错误。
fn read_assets_at(
    root: &Path,
    directory: &Path,
    output: &mut Vec<proto::Asset>,
) -> anyhow::Result<()> {
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            anyhow::bail!("assets 不允许符号链接: {}", entry.path().display());
        }
        if file_type.is_dir() {
            read_assets_at(root, &entry.path(), output)?;
        } else if file_type.is_file() {
            let relative = entry.path().strip_prefix(root)?.to_path_buf();
            if relative
                .components()
                .any(|component| !matches!(component, Component::Normal(_)))
            {
                anyhow::bail!("asset 路径不安全: {}", relative.display());
            }
            output.push(proto::Asset {
                path: relative.to_string_lossy().replace('\\', "/"),
                content: std::fs::read(entry.path())?,
            });
        } else {
            anyhow::bail!("assets 仅允许普通文件和目录: {}", entry.path().display());
        }
    }
    Ok(())
}

/// 写入导出文件且不覆盖现有路径。
///
/// # 错误
///
/// 目标已存在或无法写入时返回错误。
fn write_new_file(path: &Path, content: &[u8]) -> anyhow::Result<()> {
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o640)
        .open(path)
        .with_context(|| format!("无法创建输出文件（不会覆盖已有文件）: {}", path.display()))?;
    file.write_all(content)?;
    file.sync_all()?;
    Ok(())
}

/// 写入一行操作响应。
///
/// # 错误
///
/// 标准输出写入失败时返回错误。
fn write_operation(response: proto::OperationResponse) -> anyhow::Result<()> {
    let proto::OperationResponse { message } = response;
    write_line(&message)
}

/// 不使用 lint 禁止的打印宏，将文本写入标准输出。
///
/// # 错误
///
/// 标准输出写入失败时返回错误。
fn write_text(value: &str) -> anyhow::Result<()> {
    let mut stdout = io::stdout().lock();
    stdout.write_all(value.as_bytes())?;
    stdout.flush()?;
    Ok(())
}

/// 将一个值连同换行符写入标准输出。
///
/// # 错误
///
/// 标准输出写入失败时返回错误。
fn write_line(value: &str) -> anyhow::Result<()> {
    let mut stdout = io::stdout().lock();
    stdout.write_all(value.as_bytes())?;
    stdout.write_all(b"\n")?;
    stdout.flush()?;
    Ok(())
}

/// 为列表输出生成紧凑的单行状态。
fn compact_status(value: &str) -> String {
    value.lines().next().unwrap_or("未知").to_string()
}
