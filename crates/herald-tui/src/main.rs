use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    let mut config = None;
    let mut account = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--config" => match args.next() {
                Some(value) => config = Some(PathBuf::from(value)),
                None => {
                    eprintln!("--config 缺少路径参数");
                    return ExitCode::FAILURE;
                }
            },
            "--account" | "-a" => match args.next() {
                Some(value) => account = Some(value),
                None => {
                    eprintln!("--account 缺少账户名");
                    return ExitCode::FAILURE;
                }
            },
            "--help" | "-h" => {
                print_help();
                return ExitCode::SUCCESS;
            }
            "--version" | "-V" => {
                println!("herald-tui {}", herald_core::VERSION);
                return ExitCode::SUCCESS;
            }
            other if other.starts_with("--config=") => {
                config = Some(PathBuf::from(&other["--config=".len()..]));
            }
            other if other.starts_with("--account=") => {
                account = Some(other["--account=".len()..].to_string());
            }
            other => {
                eprintln!("未知参数：{other}（--help 查看用法）");
                return ExitCode::FAILURE;
            }
        }
    }

    match herald_tui::run(config, account) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("Herald TUI 启动失败：{err:#}");
            ExitCode::FAILURE
        }
    }
}

fn print_help() {
    println!(
        "用法：herald-tui [--config <path>] [--account <name>]

只读 POP3 邮箱的终端界面。配置文件默认位于可执行文件目录；
数据库、模型目录与实际生效路径可在界面内按 h 查看。
"
    );
}
