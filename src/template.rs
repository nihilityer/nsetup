//! 版本化 TOML 模板及其与 IR 的双向转换。

use crate::config::Config;
use crate::spec::{Logging, ServiceHooks, StackSpec};
use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// Authelia 基础认证设施模板。
mod authelia;
/// `files/` 附属目录的挂载入口。
pub mod files;
/// TOML 声明到 IR 的模板生成。
mod generate;
/// 应用拥有的 Authelia OIDC 客户端声明。
pub mod oidc;
/// IR 到 TOML 声明的模板反解。
mod reverse;
/// CLI 使用的带注释配置骨架。
mod skeleton;
/// `template` 模块单元测试。
#[cfg(test)]
mod tests;
/// Traefik 基础设施模板及 main 分支兼容默认值。
mod traefik;

use generate::{generate_app, generate_static};
pub use reverse::detect_kind;
use reverse::{export_app, export_static, export_traefik};
use skeleton::{APP_SKELETON, AUTHELIA_SKELETON, STATIC_SKELETON, TRAEFIK_SKELETON};

/// 当前面向用户的配置格式版本。
pub const FORMAT_VERSION: u32 = 1;
/// app 模板在项目 `.env` 中持久化 OIDC 客户端映射的键。
const APP_OIDC_CLIENTS_KEY: &str = "NSETUP_APP_AUTHELIA_OIDC_CLIENTS_JSON";

/// 与 Compose 状态一同写入的项目附属文件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeneratedFile {
    /// 相对于项目目录的安全路径。
    pub path: PathBuf,
    /// 文件原始字节。
    pub content: Vec<u8>,
    /// Unix 权限模式。
    pub mode: u32,
    /// 是否替换已复制的现有附属文件。
    pub replace: bool,
}

/// 模板转换输出。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemplateOutput {
    /// 统一项目 IR。
    pub spec: StackSpec,
    /// 由模板拥有且不属于 IR 的文件。
    pub files: Vec<GeneratedFile>,
    /// 选定的模板类型。
    pub kind: TemplateKind,
}

/// 已注册的模板类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TemplateKind {
    /// 通用容器应用。
    App,
    /// Authelia 基础认证设施。
    Authelia,
    /// Traefik 反向代理。
    Traefik,
    /// Nginx 静态站点。
    Static,
}

impl TemplateKind {
    /// 解析 CLI 或 TOML 模板名称。
    ///
    /// # 错误
    ///
    /// 模板未注册时返回错误。
    pub fn parse(value: &str) -> anyhow::Result<Self> {
        match value {
            "app" => Ok(Self::App),
            "authelia" => Ok(Self::Authelia),
            "traefik" => Ok(Self::Traefik),
            "static" => Ok(Self::Static),
            _ => anyhow::bail!("未知模板 {value}；可用值: app, authelia, traefik, static"),
        }
    }

    /// 返回稳定的 TOML 名称。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::App => "app",
            Self::Authelia => "authelia",
            Self::Traefik => "traefik",
            Self::Static => "static",
        }
    }
}

/// 通用应用 TOML 文档。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppConfig {
    /// 配置模式版本。
    pub format: u32,
    /// 可选的明确模板选择器。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template: Option<String>,
    /// 项目名。
    pub name: String,
    /// 可选的应用级 Authelia 集成声明。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authelia: Option<AppAutheliaConfig>,
    /// 以 Compose 服务名为键的服务映射。
    pub services: BTreeMap<String, AppServiceConfig>,
}

/// 应用拥有的 Authelia 集成配置。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppAutheliaConfig {
    /// 以 `client_id` 为键的 OIDC 客户端映射。
    pub oidc_clients: BTreeMap<String, oidc::AutheliaOidcClientConfig>,
}

/// 应用模板中的单个服务。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppServiceConfig {
    /// 不含标签的镜像仓库。
    pub image: String,
    /// 明确的镜像版本。
    pub version: String,
    /// 可选容器名。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub container_name: Option<String>,
    /// 路由使用的默认容器端口。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    /// 发布端口映射。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub publish: Vec<String>,
    /// bind mount 列表。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub volumes: Vec<String>,
    /// 容器环境变量。
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub environment: BTreeMap<String, String>,
    /// 命令覆盖参数。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub command: Vec<String>,
    /// 重启策略。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restart: Option<String>,
    /// 容器环境文件路径。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env_file: Vec<String>,
    /// 逻辑网络模式。
    #[serde(default)]
    pub network: AppNetwork,
    /// 外部模式使用的 Docker 网络。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_network: Option<String>,
    /// 自定义 Docker label。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<String>,
    /// 覆盖镜像内置用户的 `UID[:GID]`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    /// 除主用户组外额外加入的补充组。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub group_add: Vec<String>,
    /// 可选的 Compose 启动钩子。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hooks: Option<ServiceHooks>,
    /// 可选健康检查。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub healthcheck: Option<HealthcheckConfig>,
    /// 可选日志驱动。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logging: Option<Logging>,
    /// 可选 Traefik 路由。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub traefik: Option<TraefikRoutesConfig>,
}

/// 应用逻辑网络选项。
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AppNetwork {
    /// Compose bridge 网络。
    #[default]
    Bridge,
    /// 宿主机网络。
    Host,
    /// 用户选择的外部 Docker 网络。
    External,
}

/// TOML 健康检查字段。
///
/// `command` 既可以是 shell 字符串（`CMD-SHELL`），也可以是 argv 数组（`CMD`），
/// 后者用于没有 shell 的镜像。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HealthcheckConfig {
    /// shell 命令或 argv 参数列表。
    pub command: HealthcheckCommand,
    /// 检查间隔。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interval: Option<String>,
    /// 超时时间。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<String>,
    /// 启动宽限期。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_period: Option<String>,
    /// 重试次数。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retries: Option<u32>,
}

/// 健康检查命令的两种受支持写法。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum HealthcheckCommand {
    /// `CMD-SHELL` 形式：交给容器内 shell 解释的字符串。
    Shell(String),
    /// `CMD` 形式：直接执行的 argv 参数列表。
    Exec(Vec<String>),
}

/// 服务的紧凑 Traefik 配置。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TraefikRoutesConfig {
    /// 紧凑单路由形式使用的主机名。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hosts: Vec<String>,
    /// 紧凑形式使用的可选路径前缀。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path_prefix: Option<String>,
    /// 共享中间件名称。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub middlewares: Vec<String>,
    /// 后端协议。
    #[serde(default)]
    pub protocol: TemplateRouteProtocol,
    /// 路由监听的 entrypoint；省略时使用 `https`。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub entrypoint: String,
    /// 粘性 Cookie 开关。
    #[serde(default)]
    pub sticky_cookie: bool,
    /// 可选的 `passHostHeader` 覆盖值。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pass_host_header: Option<bool>,
    /// 可选路由优先级。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<u32>,
    /// 以稳定路由名为键的展开路由。
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub routes: BTreeMap<String, TraefikRouteConfig>,
}

/// 单个展开的应用路由。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TraefikRouteConfig {
    /// 路由主机名。
    pub hosts: Vec<String>,
    /// 可选路径前缀。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path_prefix: Option<String>,
    /// 可选的路由专用容器端口。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    /// 路由专用中间件。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub middlewares: Vec<String>,
    /// 后端协议。
    #[serde(default)]
    pub protocol: TemplateRouteProtocol,
    /// 路由监听的 entrypoint；省略时继承紧凑配置。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub entrypoint: String,
    /// 粘性 Cookie 开关。
    #[serde(default)]
    pub sticky_cookie: bool,
    /// 可选的 `passHostHeader` 覆盖值。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pass_host_header: Option<bool>,
    /// 可选路由优先级。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<u32>,
}

/// 路由协议的 TOML 表示。
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TemplateRouteProtocol {
    /// 普通 HTTP。
    #[default]
    Http,
    /// 连接后端的 HTTPS。
    Https,
    /// 明文 HTTP/2。
    H2c,
}

/// Traefik 基础设施 TOML 文档。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TraefikConfig {
    /// 配置模式版本。
    pub format: u32,
    /// 必填模板选择器。
    pub template: String,
    /// 基础 DNS 域名。
    pub domain: String,
    /// ACME 联系邮箱。
    pub acme_email: String,
    /// 持久化到 `.env` 的 Cloudflare API 令牌。
    pub cloudflare_token: String,
    /// 明确的 Traefik 镜像版本。
    pub version: String,
    /// 宿主机 HTTP 端口。
    #[serde(default = "default_http_port")]
    pub http_port: u16,
    /// 宿主机 HTTPS 和 HTTP/3 端口。
    #[serde(default = "default_https_port")]
    pub https_port: u16,
    /// 是否使用 Authelia `ForwardAuth` 保护 Traefik dashboard。
    #[serde(default, skip_serializing_if = "is_false")]
    pub dashboard_authelia: bool,
    /// 是否暴露 Traefik 自身的 Prometheus 指标。
    #[serde(default = "default_true")]
    pub metrics: bool,
    /// 指标入口在容器内监听的端口。
    #[serde(default = "default_metrics_port")]
    pub metrics_port: u16,
    /// 追加到生成动态配置中的自定义 Traefik 中间件。
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub middlewares: BTreeMap<String, TraefikMiddlewareConfig>,
}

/// 追加到 Traefik 动态配置的单个自定义中间件。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TraefikMiddlewareConfig {
    /// 中间件类型，例如 `replacePath`、`stripPrefix`、`headers`。
    pub kind: String,
    /// 类型对应的参数表；值按 YAML 标量或列表渲染。
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub args: BTreeMap<String, TraefikMiddlewareValue>,
}

/// 自定义中间件参数的受支持取值。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum TraefikMiddlewareValue {
    /// 单个标量参数。
    Scalar(String),
    /// 列表参数。
    List(Vec<String>),
    /// 布尔参数。
    Flag(bool),
}

/// 静态站点 TOML 文档。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct StaticConfig {
    /// 配置模式版本。
    pub format: u32,
    /// 必填模板选择器。
    pub template: String,
    /// 项目名。
    pub name: String,
    /// 短形式或完整形式的路由主机名。
    pub host: String,
    /// 明确的 Nginx 镜像版本。
    pub version: String,
    /// 常用 Traefik 中间件。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub middlewares: Vec<String>,
}

/// 解析声明并生成统一 IR。
///
/// `entrypoint` 是保留给 `files/` 附属文件的容器挂载入口。
///
/// # 错误
///
/// 当 TOML 格式错误、模板字段无效或输出不安全时返回错误。
pub fn apply(
    input: &str,
    config: &Config,
    entrypoint: Option<&str>,
) -> anyhow::Result<TemplateOutput> {
    let value: toml::Value = toml::from_str(input).context("TOML 配置格式错误")?;
    let format = value
        .get("format")
        .and_then(toml::Value::as_integer)
        .ok_or_else(|| anyhow::anyhow!("TOML 配置缺少整数 format"))?;
    if format != i64::from(FORMAT_VERSION) {
        anyhow::bail!("不支持的配置 format: {format}");
    }
    let kind = TemplateKind::parse(
        value
            .get("template")
            .and_then(toml::Value::as_str)
            .unwrap_or("app"),
    )?;
    let mut output = match kind {
        TemplateKind::App => generate_app(toml::from_str(input)?, config)?,
        TemplateKind::Authelia => {
            let input = toml::from_str(input)?;
            authelia::generate(&input, config)?
        }
        TemplateKind::Traefik => traefik::generate(&toml::from_str(input)?, config)?,
        TemplateKind::Static => generate_static(toml::from_str(input)?, config)?,
    };
    if let Some(directory) = entrypoint {
        let project_directory = config.stacks_root.join(&output.spec.name);
        files::bind_directory(&mut output.spec, &project_directory, directory)?;
    }
    Ok(output)
}

/// 从当前由 Compose 承载的 IR 重建规范化 TOML。
///
/// # 错误
///
/// 无法恢复模板元数据或语义字段时返回错误。
pub fn export(spec: &StackSpec, config: &Config) -> anyhow::Result<String> {
    let kind = detect_kind(spec)?;
    let mut output = match kind {
        TemplateKind::App => toml::to_string_pretty(&export_app(spec)?)?,
        TemplateKind::Authelia => toml::to_string_pretty(&authelia::export(spec)?)?,
        TemplateKind::Traefik => toml::to_string_pretty(&export_traefik(spec, config)?)?,
        TemplateKind::Static => toml::to_string_pretty(&export_static(spec)?)?,
    };
    if !output.ends_with('\n') {
        output.push('\n');
    }
    Ok(output)
}

/// 返回应用项目声明的 OIDC client ID，用于跨项目冲突检查。
pub fn app_oidc_client_ids(spec: &StackSpec) -> anyhow::Result<Vec<String>> {
    Ok(app_oidc_clients(spec)?.into_keys().collect())
}

/// 生成应用拥有、由 Authelia 汇总加载的 OIDC 客户端片段。
pub fn app_oidc_client_fragment(spec: &StackSpec) -> anyhow::Result<Option<GeneratedFile>> {
    let clients = app_oidc_clients(spec)?;
    if clients.is_empty() {
        return Ok(None);
    }
    Ok(Some(GeneratedFile {
        path: PathBuf::from("config/oidc-clients").join(format!("{}.yml", spec.name)),
        content: oidc::clients_yaml(&clients)?.into_bytes(),
        mode: 0o640,
        replace: true,
    }))
}

/// 判断 Authelia 项目状态是否具备 OIDC provider 密钥。
pub fn authelia_oidc_enabled(spec: &StackSpec) -> bool {
    authelia::oidc_enabled(spec)
}

/// 从应用项目 `.env` 恢复并校验 OIDC 客户端映射。
fn app_oidc_clients(
    spec: &StackSpec,
) -> anyhow::Result<BTreeMap<String, oidc::AutheliaOidcClientConfig>> {
    let Some(value) = spec.environment.get(APP_OIDC_CLIENTS_KEY) else {
        return Ok(BTreeMap::new());
    };
    let clients = serde_json::from_str(value)?;
    oidc::validate_clients(&clients)?;
    Ok(clients)
}

/// 返回已注册模板的带注释配置骨架。
#[must_use]
pub const fn skeleton(kind: TemplateKind) -> &'static str {
    match kind {
        TemplateKind::App => APP_SKELETON,
        TemplateKind::Authelia => AUTHELIA_SKELETON,
        TemplateKind::Traefik => TRAEFIK_SKELETON,
        TemplateKind::Static => STATIC_SKELETON,
    }
}

/// 追加带完整注释的配置骨架，使导出结果可以直接作为仓库交付物。
///
/// 反解得到的是规范化 TOML，原始注释无法恢复；这里把当前版本骨架的注释逐行前置，
/// 得到“注释 + 当前值”的可读文件，而不是让注释整体丢失。
///
/// # 错误
///
/// 模板类型无法解析时返回错误。
pub fn annotate(config_toml: &str, kind: TemplateKind) -> anyhow::Result<String> {
    if toml::from_str::<toml::Value>(config_toml).is_err() {
        anyhow::bail!("无法为非法 TOML 追加注释");
    }
    let mut output = String::from(
        "# 由 nsetup export --keep-comments 生成：注释来自当前版本的模板骨架，\n\
         # 下方即为当前状态等价的规范化 TOML；重新 apply 时注释会被忽略。\n\n",
    );
    for line in skeleton(kind).lines() {
        if line.starts_with('#') || line.trim().is_empty() {
            output.push_str(line);
        } else {
            output.push_str("# ");
            output.push_str(line);
        }
        output.push('\n');
    }
    output.push('\n');
    output.push_str(config_toml);
    if !output.ends_with('\n') {
        output.push('\n');
    }
    Ok(output)
}

/// 返回 Traefik 默认 HTTP 端口。
const fn default_http_port() -> u16 {
    80
}

/// 返回 Traefik 默认 HTTPS 端口。
const fn default_https_port() -> u16 {
    443
}

/// 返回 Traefik 默认指标端口。
const fn default_metrics_port() -> u16 {
    crate::spec::DEFAULT_METRICS_PORT
}

/// 返回布尔字段的默认开启值。
const fn default_true() -> bool {
    true
}

/// 判断布尔值是否为默认关闭状态。
const fn is_false(value: &bool) -> bool {
    !*value
}
