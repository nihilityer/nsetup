//! CLI 与 daemon 两种角色共享的文件系统和协议常量。

use std::path::PathBuf;

/// 系统配置目录。
pub const CONFIG_DIR: &str = "/etc/nsetup";
/// daemon 主配置文件名。
pub const CONFIG_FILE: &str = "config.toml";
/// TCP 认证令牌文件名。
pub const AUTH_TOKEN_FILE: &str = "auth.token";
/// 默认本机 gRPC socket。
pub const GRPC_SOCKET: &str = "/run/nsetup/nsetup.sock";
/// 二进制安装路径。
pub const BINARY_PATH: &str = "/usr/local/bin/nsetup";
/// systemd unit 安装路径。
pub const UNIT_PATH: &str = "/etc/systemd/system/nsetup.service";
/// Compose 状态文件名。
pub const COMPOSE_FILE: &str = "compose.yaml";
/// Compose 环境变量文件名。
pub const ENV_FILE: &str = ".env";
/// 管理员 Unix 用户组。
pub const ADMIN_GROUP: &str = "nihility";
/// 声明式配置允许的最大字节数。
pub const MAX_CONFIG_SIZE: usize = 1024 * 1024;
/// RPC 消息允许的最大字节数。
pub const MAX_RPC_MESSAGE_SIZE: usize = 64 * 1024 * 1024;
/// Traefik 发现的服务共用的 Docker 网络。
pub const PROXY_NETWORK: &str = "nsetup-proxy";

/// 返回当前配置文件路径。
#[must_use]
pub fn config_path() -> PathBuf {
    std::env::var_os("NSETUP_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(CONFIG_DIR).join(CONFIG_FILE))
}

/// 返回系统认证令牌路径。
#[must_use]
pub fn auth_token_path() -> PathBuf {
    PathBuf::from(CONFIG_DIR).join(AUTH_TOKEN_FILE)
}
