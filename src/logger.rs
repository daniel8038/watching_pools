use anyhow::{Ok, Result};

use fern::colors::{Color, ColoredLevelConfig};

pub fn setup_logger() -> Result<()> {
    let colors = ColoredLevelConfig {
        trace: Color::Cyan,
        debug: Color::Magenta,
        info: Color::Green,
        warn: Color::Red,
        error: Color::BrightRed,
        ..ColoredLevelConfig::new()
    };
    fern::Dispatch::new()
        // 向记录器添加格式化程序，修改发送的所有消息。
        .format(move |out, message, record| {
            out.finish(
                // finish 方法本身只接受一个参数，这个参数是通过 format_args! 宏生成的格式化参数。
                format_args!(
                    "{}[{}][{}] {}",
                    // 使用chrono库获取本地时区的当前时间。使用 chrono 的惰性格式说明符将时间转换为可读的字符串。
                    chrono::Local::now().format("[%Y-%m-%d][%H:%M:%S]"),
                    record.target(),
                    colors.color(record.level()),
                    message
                ),
            );
        })
        // 将输出所需的最低水平设置为Info
        .level(log::LevelFilter::Info)
        .chain(std::io::stdout())
        // 使用配置并将其实例化为当前运行时全局记录器。
        // 当且仅.apply()当此运行时已经使用另一个板条箱或等效形式时，此操作才会失败。
        .apply()?;
    Ok(())
}
