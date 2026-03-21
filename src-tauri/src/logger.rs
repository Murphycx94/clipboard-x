use once_cell::sync::OnceCell;
use simplelog::{CombinedLogger, Config, LevelFilter, WriteLogger};
use std::fs::OpenOptions;
use std::path::{Path, PathBuf};

static LOG_PATH: OnceCell<PathBuf> = OnceCell::new();

pub fn init(data_dir: &Path) {
    let log_path = data_dir.join("clipboardx.log");
    LOG_PATH.set(log_path.clone()).ok();

    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .expect("Failed to open log file");

    CombinedLogger::init(vec![WriteLogger::new(LevelFilter::Info, Config::default(), file)]).ok();
}

pub fn get_log_contents() -> String {
    match LOG_PATH.get() {
        Some(path) => std::fs::read_to_string(path).unwrap_or_else(|_| "日志文件读取失败".to_string()),
        None => "日志未初始化".to_string(),
    }
}
