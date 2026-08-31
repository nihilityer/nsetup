//! CLI 本地命令与远程 RPC 命令执行。

use super::args::{Cli, Command};
use super::edit::edit_request;
use super::io::{
    compact_status, read_assets, read_limited, write_line, write_new_file, write_operation,
    write_text,
};
use crate::constants::MAX_CONFIG_SIZE;
use crate::install::{InstallOptions, install};
use crate::rpc::proto;
use crate::rpc::{Action, RpcClient};
use crate::template::{TemplateKind, skeleton};
use dialoguer::Confirm;
use tokio_stream::StreamExt;

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
