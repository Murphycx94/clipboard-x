//! The app's log file, which the Telegram dialog shows.

use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use simplelog::{ConfigBuilder, LevelFilter, WriteLogger};

static LOG_PATH: OnceLock<PathBuf> = OnceLock::new();

/// The most of the log file shown, from its end.
const SHOWN_BYTES: usize = 64 * 1024;

/// Logs this crate's messages (info and above) to `clipboardx.log` in
/// `data_dir`. Only the first call sets the logger.
pub fn init(data_dir: &Path) {
    let path = data_dir.join("clipboardx.log");
    let file = match OpenOptions::new().create(true).append(true).open(&path) {
        Ok(file) => file,
        Err(e) => {
            eprintln!("cannot open the log file {}: {e}", path.display());
            return;
        }
    };
    let config = ConfigBuilder::new()
        .add_filter_allow_str("clipboardx")
        .build();
    if WriteLogger::init(LevelFilter::Info, config, file).is_ok() {
        let _ = LOG_PATH.set(path);
    }
}

/// The end of the log file, or why there is none.
pub fn contents() -> String {
    let Some(path) = LOG_PATH.get() else {
        return "日志未初始化".to_owned();
    };
    match std::fs::read_to_string(path) {
        Ok(text) => tail(&text, SHOWN_BYTES).to_owned(),
        Err(_) => "日志文件读取失败".to_owned(),
    }
}

/// The last `max` bytes of `text`, starting on a line.
fn tail(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut start = text.len() - max;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    let rest = &text[start..];
    rest.find('\n').map_or(rest, |nl| &rest[nl + 1..])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tail_keeps_whole_lines() {
        assert_eq!(tail("short", 10), "short");
        assert_eq!(tail("line one\nline two\nline three", 14), "line three");
    }

    #[test]
    fn tail_respects_char_boundaries() {
        assert_eq!(tail("一二三\n四五", 7), "四五");
    }
}
