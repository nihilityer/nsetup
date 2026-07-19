//! 基础设施与应用生成命令参数。

use clap::{Args, Subcommand, ValueEnum};
use std::path::PathBuf;
use std::str::FromStr;

/// 基础设施子命令。
#[derive(Debug, Subcommand)]
pub enum InfraCmd {
    /// 生成 Traefik 项目。
    Init {
        /// Nihility 服务使用的主域名。
        #[arg(long)]
        domain: String,
        /// ACME 证书通知邮箱。
        #[arg(long)]
        acme_email: String,
        /// 保存 Cloudflare DNS API 令牌的文件。
        #[arg(long)]
        cloudflare_token_file: PathBuf,
        /// Traefik 镜像版本。
        #[arg(long, default_value = "v3.7.8")]
        traefik_version: String,
        /// 生成后立即启动。
        #[arg(long)]
        start: bool,
        /// 覆盖已有基础设施配置。
        #[arg(long)]
        force: bool,
    },
}

/// 应用生成子命令。
#[derive(Debug, Subcommand)]
pub enum AppCmd {
    /// 从镜像生成常规单服务应用。
    Add(Box<AddArgs>),
    /// 从目录生成 Nginx 静态站点。
    AddStatic {
        /// Compose 项目名。
        name: String,
        /// 本地静态文件目录。
        #[arg(long)]
        source: PathBuf,
        /// 站点完整域名。
        #[arg(long)]
        host: String,
        /// Traefik 中间件；可重复指定。
        #[arg(long, value_enum)]
        middleware: Vec<MiddlewareArg>,
        /// Nginx 镜像版本。
        #[arg(long, default_value = "1.27-alpine")]
        nginx_version: String,
        /// 生成后立即启动。
        #[arg(long)]
        start: bool,
        /// 覆盖已有项目配置。
        #[arg(long)]
        force: bool,
    },
}

/// 常规单服务应用参数。
#[derive(Debug, Args)]
pub struct AddArgs {
    /// Compose 项目名。
    pub name: String,
    /// 容器镜像及标签。
    #[arg(long)]
    pub image: String,
    /// Compose 服务名。
    #[arg(long, default_value = "app")]
    pub service: String,
    /// 覆盖镜像默认命令；可重复传入参数。
    #[arg(long)]
    pub command: Vec<String>,
    /// 提供给 Traefik 的容器端口。
    #[arg(long, default_value_t = 80)]
    pub container_port: u16,
    /// 完整访问域名；可重复指定。
    #[arg(long = "host")]
    pub hosts: Vec<String>,
    /// 应用于全部域名的 URL 路径前缀。
    #[arg(long)]
    pub path_prefix: Option<String>,
    /// TCP 端口映射，格式为 HOST:CONTAINER。
    #[arg(long = "publish")]
    pub tcp_ports: Vec<PortMappingArg>,
    /// UDP 端口映射，格式为 HOST:CONTAINER。
    #[arg(long = "publish-udp")]
    pub udp_ports: Vec<PortMappingArg>,
    /// 可写卷挂载，格式为 HOST:CONTAINER。
    #[arg(long = "volume")]
    pub volumes: Vec<MountArg>,
    /// 只读卷挂载，格式为 HOST:CONTAINER。
    #[arg(long = "read-only-volume")]
    pub read_only_volumes: Vec<MountArg>,
    /// 环境变量，格式为 KEY=VALUE。
    #[arg(long = "env")]
    pub environment: Vec<EnvironmentArg>,
    /// 容器网络模式。
    #[arg(long, value_enum, default_value_t = NetworkArg::Bridge)]
    pub network: NetworkArg,
    /// external 网络模式使用的 Docker 网络名。
    #[arg(long)]
    pub external_network: Option<String>,
    /// Traefik 中间件；可重复指定。
    #[arg(long, value_enum)]
    pub middleware: Vec<MiddlewareArg>,
    /// 生成后立即启动。
    #[arg(long)]
    pub start: bool,
    /// 覆盖已有项目配置。
    #[arg(long)]
    pub force: bool,
}

/// CLI 网络模式。
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum NetworkArg {
    /// Compose 默认桥接网络。
    Bridge,
    /// 宿主机网络。
    Host,
    /// 指定的外部 Docker 网络。
    External,
}

/// CLI Traefik 中间件。
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum MiddlewareArg {
    /// `GZip` 响应压缩。
    Gzip,
    /// 注入 HTTPS 转发头。
    ForwardedHeaders,
    /// 仅允许内网访问。
    InternalOnly,
}

/// CLI 端口映射。
#[derive(Debug, Clone, Copy)]
pub struct PortMappingArg {
    /// 宿主机端口。
    pub host: u16,
    /// 容器端口。
    pub container: u16,
}

impl FromStr for PortMappingArg {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let (host, container) = value
            .split_once(':')
            .ok_or_else(|| String::from("端口映射格式必须为 HOST:CONTAINER"))?;
        Ok(Self {
            host: parse_port(host)?,
            container: parse_port(container)?,
        })
    }
}

/// CLI 卷挂载。
#[derive(Debug, Clone)]
pub struct MountArg {
    /// 宿主机路径。
    pub host: String,
    /// 容器路径。
    pub container: String,
}

impl FromStr for MountArg {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let (host, container) = value
            .split_once(':')
            .ok_or_else(|| String::from("卷挂载格式必须为 HOST:CONTAINER"))?;
        if host.is_empty() || container.is_empty() {
            return Err(String::from("卷挂载路径不能为空"));
        }
        Ok(Self {
            host: host.to_string(),
            container: container.to_string(),
        })
    }
}

/// CLI 环境变量。
#[derive(Debug, Clone)]
pub struct EnvironmentArg {
    /// 环境变量名。
    pub key: String,
    /// 环境变量值。
    pub value: String,
}

impl FromStr for EnvironmentArg {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let (key, value) = value
            .split_once('=')
            .ok_or_else(|| String::from("环境变量格式必须为 KEY=VALUE"))?;
        if key.is_empty()
            || !key
                .bytes()
                .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
        {
            return Err(String::from("环境变量名只能包含大写字母、数字和下划线"));
        }
        Ok(Self {
            key: key.to_string(),
            value: value.to_string(),
        })
    }
}

/// 解析非零 TCP/UDP 端口。
fn parse_port(value: &str) -> Result<u16, String> {
    value
        .parse::<u16>()
        .ok()
        .filter(|port| *port > 0)
        .ok_or_else(|| String::from("端口必须在 1..=65535 范围内"))
}
