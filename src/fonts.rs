use eframe::egui;
use std::sync::Arc;
use tracing::{info, warn};

/// 为 egui 注入系统中文字体，避免界面汉字显示为方框/乱码。
pub fn setup_cjk_fonts(ctx: &egui::Context) {
    let Some((name, bytes)) = load_system_cjk_font() else {
        warn!("未找到可用中文字体，界面中文可能无法正确显示");
        return;
    };

    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        "cjk".to_owned(),
        Arc::new(egui::FontData::from_owned(bytes)),
    );

    // 优先用中文字体，再回退到 egui 默认字体（英文/数字）
    fonts
        .families
        .entry(egui::FontFamily::Proportional)
        .or_default()
        .insert(0, "cjk".to_owned());
    fonts
        .families
        .entry(egui::FontFamily::Monospace)
        .or_default()
        .insert(0, "cjk".to_owned());

    ctx.set_fonts(fonts);
    info!("已加载中文字体: {name}");
}

fn load_system_cjk_font() -> Option<(String, Vec<u8>)> {
    let candidates = system_font_candidates();
    for path in candidates {
        match std::fs::read(&path) {
            Ok(bytes) if !bytes.is_empty() => {
                return Some((path, bytes));
            }
            Ok(_) => warn!("字体文件为空: {path}"),
            Err(e) => {
                tracing::debug!("跳过字体 {path}: {e}");
            }
        }
    }
    None
}

fn system_font_candidates() -> Vec<String> {
    let mut paths = Vec::new();

    #[cfg(windows)]
    {
        let windir = std::env::var("WINDIR").unwrap_or_else(|_| r"C:\Windows".into());
        let fonts = format!(r"{windir}\Fonts");
        // 优先微软雅黑，覆盖常见中文 Windows
        for name in [
            // 优先单字体 TTF，TTC 在部分 egui/ab_glyph 版本上不稳定
            "simhei.ttf",
            "simfang.ttf",
            "simkai.ttf",
            "Deng.ttf",
            "Dengb.ttf",
            "msyh.ttc",
            "msyh.ttf",
            "msyhl.ttc",
            "simsun.ttc",
            "NotoSansSC-Regular.otf",
            "SourceHanSansSC-Regular.otf",
        ] {
            paths.push(format!(r"{fonts}\{name}"));
        }
    }

    #[cfg(target_os = "macos")]
    {
        paths.extend(
            [
                "/System/Library/Fonts/PingFang.ttc",
                "/System/Library/Fonts/STHeiti Light.ttc",
                "/System/Library/Fonts/Hiragino Sans GB.ttc",
                "/Library/Fonts/Arial Unicode.ttf",
            ]
            .map(str::to_string),
        );
    }

    #[cfg(target_os = "linux")]
    {
        paths.extend(
            [
                "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
                "/usr/share/fonts/truetype/noto/NotoSansCJK-Regular.ttc",
                "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
                "/usr/share/fonts/truetype/wqy/wqy-microhei.ttc",
                "/usr/share/fonts/truetype/droid/DroidSansFallbackFull.ttf",
            ]
            .map(str::to_string),
        );
    }

    paths
}
