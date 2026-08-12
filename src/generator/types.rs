//! 配置生成使用的类型化输入与输出。

use std::collections::BTreeMap;
use std::path::PathBuf;

/// 基础设施生成参数。
#[derive(Debug, Clone)]
pub struct InfraSpec {
    /// 主域名。
    pub domain: String,
    /// ACME 通知邮箱。
    pub acme_email: String,
    /// Cloudflare API 令牌。
    pub cloudflare_token: String,
    /// Traefik 镜像版本。
    pub traefik_version: String,
    /// 映射到宿主机的 HTTP 端口。
    pub http_port: u16,
    /// 映射到宿主机的 HTTPS 和 HTTP/3 端口。
    pub https_port: u16,
}

/// 常规单服务应用生成参数。
#[derive(Debug, Clone)]
pub struct AppSpec {
    /// Compose 项目名。
    pub name: String,
    /// Compose 服务名。
    pub service: String,
    /// 不含版本标签的容器镜像名。
    pub image: String,
    /// 容器镜像版本标签。
    pub version: String,
    /// 覆盖镜像默认命令的参数。
    pub command: Vec<String>,
    /// Traefik 连接的容器端口。
    pub container_port: u16,
    /// HTTP 路由。
    pub routes: Vec<Route>,
    /// 宿主机端口映射。
    pub published_ports: Vec<PublishedPort>,
    /// 数据卷。
    pub volumes: Vec<Volume>,
    /// 环境变量。
    pub environment: BTreeMap<String, String>,
    /// 网络模式。
    pub network_mode: NetworkMode,
    /// Traefik 中间件。
    pub middlewares: Vec<Middleware>,
    /// 附加到容器的自定义 Docker 标签。
    pub labels: Vec<String>,
    /// 应用命名卷。
    pub named_volumes: Vec<NamedVolume>,
    /// 容器健康检查。
    pub healthcheck: Option<HealthcheckSpec>,
}

/// 应用健康检查参数。
#[derive(Debug, Clone)]
pub struct HealthcheckSpec {
    /// 健康检查命令（`CMD-SHELL` 形式）。
    pub command: String,
    /// 检查间隔。
    pub interval: String,
    /// 单次检查超时。
    pub timeout: String,
    /// 启动宽限期。
    pub start_period: Option<String>,
    /// 失败重试次数。
    pub retries: u32,
}

/// 应用命名卷。
#[derive(Debug, Clone)]
pub struct NamedVolume {
    /// 命名卷名。
    pub name: String,
    /// 容器路径。
    pub container_path: String,
}

/// 静态站点生成参数。
#[derive(Debug, Clone)]
pub struct StaticSiteSpec {
    /// Compose 项目名。
    pub name: String,
    /// 站点完整域名。
    pub host: String,
    /// 站点文件。
    pub assets: Vec<StaticAsset>,
    /// Traefik 中间件。
    pub middlewares: Vec<Middleware>,
    /// Nginx 镜像版本。
    pub nginx_version: String,
}

/// 应用 HTTP 路由。
#[derive(Debug, Clone)]
pub struct Route {
    /// 完整域名。
    pub host: String,
    /// 可选的 URL 路径前缀。
    pub path_prefix: Option<String>,
    /// 此路由连接的容器端口。
    pub container_port: u16,
}

/// 宿主机端口映射。
#[derive(Debug, Clone, Copy)]
pub struct PublishedPort {
    /// 宿主机端口。
    pub host_port: u16,
    /// 容器端口。
    pub container_port: u16,
    /// 传输层协议。
    pub protocol: PortProtocol,
}

/// 端口协议。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PortProtocol {
    /// TCP。
    Tcp,
    /// UDP。
    Udp,
}

impl PortProtocol {
    /// 返回 Compose 端口映射使用的协议后缀。
    pub const fn compose_suffix(self) -> &'static str {
        match self {
            Self::Tcp => "tcp",
            Self::Udp => "udp",
        }
    }
}

/// 应用数据卷。
#[derive(Debug, Clone)]
pub struct Volume {
    /// 宿主机路径。
    pub host_path: String,
    /// 容器路径。
    pub container_path: String,
    /// 是否只读。
    pub read_only: bool,
}

/// 应用网络模式。
#[derive(Debug, Clone)]
pub enum NetworkMode {
    /// Compose 默认桥接网络。
    Bridge,
    /// 宿主机网络。
    Host,
    /// 指定名称的外部网络。
    External(String),
}

/// 普通应用可引用的 Traefik 中间件。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Middleware {
    /// `GZip` 响应压缩。
    Gzip,
    /// HTTPS 转发请求头。
    ForwardedHeaders,
    /// 内网地址白名单。
    InternalOnly,
}

impl Middleware {
    /// 返回 Traefik 文件提供者引用。
    pub const fn label_ref(self) -> &'static str {
        match self {
            Self::Gzip => "gzip@file",
            Self::ForwardedHeaders => "forwarded-headers@file",
            Self::InternalOnly => "internal-only@file",
        }
    }

    /// 从 Traefik 文件提供者引用还原中间件。
    pub fn from_label_ref(value: &str) -> Option<Self> {
        match value {
            "gzip@file" => Some(Self::Gzip),
            "forwarded-headers@file" => Some(Self::ForwardedHeaders),
            "internal-only@file" => Some(Self::InternalOnly),
            _ => None,
        }
    }
}

/// 静态站点中的单个文件。
#[derive(Debug, Clone)]
pub struct StaticAsset {
    /// 相对站点根目录的路径。
    pub path: PathBuf,
    /// 文件内容。
    pub content: Vec<u8>,
}

/// 生成后需要部署的 Compose 项目。
#[derive(Debug, Clone)]
pub struct GeneratedStack {
    /// Compose 项目名。
    pub name: String,
    /// Compose YAML。
    pub compose_yaml: String,
    /// 环境变量文件内容。
    pub env_file: String,
    /// 附属文件。
    pub files: Vec<GeneratedFile>,
}

/// Compose 项目中的附属文件。
#[derive(Debug, Clone)]
pub struct GeneratedFile {
    /// 相对项目目录的路径。
    pub path: PathBuf,
    /// 文件内容。
    pub content: Vec<u8>,
    /// Unix 权限模式。
    pub mode: u32,
}
