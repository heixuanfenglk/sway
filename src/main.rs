// Release 下不弹出黑色控制台窗口；Debug 仍保留，方便看日志
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod config;
mod fonts;
mod keys;
mod proxy;
mod stats;
mod tunnel;
mod ui;

use std::sync::Arc;
use tracing_subscriber::EnvFilter;
use ui::ProxyApp;

fn main() -> eframe::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_target(false)
        .init();

    let rt = Arc::new(
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("创建 Tokio runtime 失败"),
    );

    // 进入 egui 前先进入 runtime，方便异步任务
    let _guard = rt.enter();

    let icon = eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon-256.png"))
        .unwrap_or_else(|e| panic!("加载应用图标失败: {e}"));

    let viewport = egui::ViewportBuilder::default()
        .with_inner_size([720.0, 640.0])
        .with_min_inner_size([640.0, 520.0])
        .with_title("Sway · 全局代理")
        .with_icon(icon);

    // Windows Server / RDP 常只有 OpenGL 1.1，glow 会静默失败或空白窗口。
    // 优先 wgpu + DX12；失败再回退 glow。
    let wgpu_err = {
        let app_rt = Arc::clone(&rt);
        let options = eframe::NativeOptions {
            viewport: viewport.clone(),
            renderer: eframe::Renderer::Wgpu,
            wgpu_options: windows_friendly_wgpu_options(),
            ..Default::default()
        };
        match eframe::run_native(
            "Sway",
            options,
            Box::new(move |cc| {
                fonts::setup_cjk_fonts(&cc.egui_ctx);
                Ok(Box::new(ProxyApp::new(app_rt)))
            }),
        ) {
            Ok(()) => return Ok(()),
            Err(e) => {
                tracing::warn!("wgpu 渲染启动失败，尝试 glow: {e}");
                Some(e)
            }
        }
    };

    let app_rt = Arc::clone(&rt);
    let options = eframe::NativeOptions {
        viewport,
        renderer: eframe::Renderer::Glow,
        ..Default::default()
    };
    match eframe::run_native(
        "Sway",
        options,
        Box::new(move |cc| {
            fonts::setup_cjk_fonts(&cc.egui_ctx);
            Ok(Box::new(ProxyApp::new(app_rt)))
        }),
    ) {
        Ok(()) => Ok(()),
        Err(glow_err) => {
            let detail = match wgpu_err {
                Some(w) => format!("wgpu: {w}\nglow: {glow_err}"),
                None => format!("{glow_err}"),
            };
            show_startup_error(&detail);
            Err(glow_err)
        }
    }
}

fn windows_friendly_wgpu_options() -> eframe::egui_wgpu::WgpuConfiguration {
    use eframe::egui_wgpu::{WgpuConfiguration, WgpuSetup, WgpuSetupCreateNew};
    use eframe::wgpu;

    let mut setup = WgpuSetupCreateNew::default();
    // DX12 在 Windows Server / 远程桌面下通常可用；Vulkan/GL 作补充
    setup.instance_descriptor.backends = wgpu::Backends::from_env().unwrap_or(
        wgpu::Backends::DX12 | wgpu::Backends::VULKAN | wgpu::Backends::GL,
    );
    // 服务器/虚拟机常见低性能适配器，LowPower 更稳
    setup.power_preference =
        wgpu::PowerPreference::from_env().unwrap_or(wgpu::PowerPreference::LowPower);

    WgpuConfiguration {
        wgpu_setup: WgpuSetup::CreateNew(setup),
        ..Default::default()
    }
}

fn show_startup_error(detail: &str) {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows::core::PCWSTR;
        use windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK};

        let title: Vec<u16> = std::ffi::OsStr::new("Sway 启动失败")
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let body = format!(
            "无法创建图形界面。\n\n\
             常见原因：Windows Server / 远程桌面缺少可用的 DirectX 或 OpenGL。\n\n\
             详情：\n{detail}"
        );
        let body: Vec<u16> = std::ffi::OsStr::new(&body)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        unsafe {
            let _ = MessageBoxW(
                None,
                PCWSTR(body.as_ptr()),
                PCWSTR(title.as_ptr()),
                MB_OK | MB_ICONERROR,
            );
        }
    }
    #[cfg(not(windows))]
    {
        eprintln!("Sway 启动失败: {detail}");
    }
}
