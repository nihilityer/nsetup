//! 全局文件名与系统路径常量。

// ── 文件名 ──
/// Docker Compose 文件名
pub const COMPOSE_FILE: &str = "compose.yaml";
/// 环境变量文件名
pub const ENV_FILE: &str = ".env";
// ── nsetup 系统路径 ──
/// 系统配置目录
pub const SYSTEM_CONFIG_DIR: &str = "/etc/nsetup";
/// 系统持久状态目录
pub const SYSTEM_STATE_DIR: &str = "/var/lib/nsetup";
/// 本机 gRPC Unix domain socket
pub const GRPC_SOCKET: &str = "/run/nsetup/nsetup.sock";
/// Compose 项目目录名
pub const STACKS_DIR: &str = "stacks";
/// 全局配置文件名
pub const CONFIG_FILE: &str = "config.toml";
/// gRPC 认证令牌文件名
pub const AUTH_TOKEN_FILE: &str = "auth.token";
