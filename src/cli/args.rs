//! 命令行参数、子命令与中文帮助定义。

use clap::{Arg, ArgAction, Args, CommandFactory, FromArgMatches, Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

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
    Show(ShowArgs),
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
    /// 比对容器 label 与 Traefik 实际加载的路由，报告未被接管的服务。
    Doctor,
    /// 查询或修改 daemon 配置。
    Config(ConfigArgs),
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
    /// 模板名称；可选值为 `app`、`authelia`、`traefik`、`static`，默认为 `app`。
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
    /// `--assets` 与既有站点内容的合并方式；支持 `merge`、`replace`。
    #[arg(
        long,
        default_value = "merge",
        hide_default_value = true,
        hide_possible_values = true
    )]
    pub assets_mode: AssetsModeArg,
    /// 上传到项目 `files/` 目录并挂载进容器的文件或目录；可重复指定。
    #[arg(long = "files", value_name = "路径")]
    pub files: Vec<PathBuf>,
    /// `--files` 在容器内的挂载目标。
    #[arg(long, default_value = "/opt/nsetup/files", hide_default_value = true)]
    pub files_into: String,
    /// 应用成功后启动项目。
    #[arg(long)]
    pub start: bool,
    /// 替换现有项目。
    #[arg(long)]
    pub force: bool,
    /// Authelia OIDC 客户端变化后顺带重启 authelia。
    #[arg(long)]
    pub restart_dependents: bool,
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
    /// 在结果前追加当前版本的带注释骨架。
    #[arg(long)]
    pub keep_comments: bool,
}

/// `show` 接受的参数。
#[derive(Debug, Args)]
pub struct ShowArgs {
    /// 项目名。
    pub name: String,
    /// 输出解析后的 Traefik router / service / middleware 表。
    #[arg(long)]
    pub routes: bool,
}

/// `config` 接受的参数。
#[derive(Debug, Args)]
pub struct ConfigArgs {
    /// 配置子命令。
    #[command(subcommand)]
    pub command: ConfigCommand,
}

/// `config` 支持的子命令。
#[derive(Debug, Subcommand)]
pub enum ConfigCommand {
    /// 显示当前 daemon 配置。
    Show,
    /// 修改 daemon 配置项。
    Set(ConfigSetArgs),
}

/// `config set` 支持的配置项。
#[derive(Debug, Args)]
pub struct ConfigSetArgs {
    /// 要修改的配置项。
    #[command(subcommand)]
    pub item: ConfigItem,
}

/// `config set` 支持的配置项列表。
#[derive(Debug, Subcommand)]
pub enum ConfigItem {
    /// 更新基础域名，无需重装 daemon。
    Domain(DomainArgs),
}

/// `config set domain` 接受的位置参数。
#[derive(Debug, Args)]
pub struct DomainArgs {
    /// 新的基础域名。
    pub domain: String,
}

/// `--assets` 的合并方式。
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum AssetsModeArg {
    /// 只覆盖同名文件，保留其它既有站点文件。
    Merge,
    /// 先清空站点目录再写入上传的文件。
    Replace,
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
    /// 新路由的 entrypoint；省略时使用 `https`。
    #[arg(long)]
    pub entrypoint: Option<String>,
    /// 新路由中间件；可重复指定，支持内置名称与自定义中间件名。
    #[arg(long = "middleware")]
    pub middlewares: Vec<String>,
    /// 新路由使用的后端协议；支持 `http`、`https`、`h2c`，默认为 `http`。
    #[arg(
        long,
        default_value = "http",
        hide_default_value = true,
        hide_possible_values = true
    )]
    pub protocol: String,
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
    /// `CMD`（argv）形式的健康检查参数；与 `--healthcheck-cmd` 互斥。
    #[arg(long = "healthcheck-exec", num_args = 1..)]
    pub healthcheck_exec: Vec<String>,
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
