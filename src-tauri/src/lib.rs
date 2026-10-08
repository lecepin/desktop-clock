// Learn more about Tauri commands at https://tauri.app/develop/calling-rust/
#[tauri::command]
fn greet(name: &str) -> String {
    format!("Hello, {}! You've been greeted from Rust!", name)
}

use tauri::{
    menu::{CheckMenuItem, CheckMenuItemBuilder, MenuBuilder, MenuItemBuilder},
    tray::TrayIconBuilder,
    Emitter, Manager, PhysicalPosition, PhysicalSize,
};

/// 托盘菜单项句柄，放进全局状态，供恢复命令同步 Checkbox 的勾选显示
/// 注意：CheckMenuItem 没有默认泛型参数，必须显式写 <tauri::Wry>，否则报 E0107
struct TrayMenuState {
    penetrate: CheckMenuItem<tauri::Wry>,
    always_on_top: CheckMenuItem<tauri::Wry>,
}

/// 前端启动时调用一次：把 localStorage 中保存的托盘开关状态恢复回来，
/// 同时把托盘菜单上两个 Checkbox 的勾选显示同步成恢复后的值。
#[tauri::command]
fn restore_app_state(app: tauri::AppHandle, penetrate: bool, always_on_top: bool) {
    if let Some(state) = app.try_state::<TrayMenuState>() {
        let _ = state.penetrate.set_checked(penetrate);
        let _ = state.always_on_top.set_checked(always_on_top);
    }
    apply_always_on_top(&app, always_on_top);
    apply_penetrate(&app, penetrate);
}

/// 前端启动时调用一次：把 localStorage 中保存的窗口尺寸与位置恢复回来。
/// 单位均为物理像素，与前端 onResized / onMoved 事件给出的值保持一致。
#[tauri::command]
fn restore_window_state(app: tauri::AppHandle, x: i32, y: i32, width: u32, height: u32) -> bool {
    let win = match app.get_webview_window("main") {
        Some(w) => w,
        None => return false,
    };

    // 尺寸：过滤异常值，避免把窗口恢复成无法操作的尺寸
    if width >= 80 && height >= 80 {
        let _ = win.set_size(tauri::Size::Physical(PhysicalSize::new(width, height)));
    }

    // 位置：多显示器场景下，旧坐标可能落在已拔掉/改过分辨率的屏幕上。
    // 先校验左上角仍落在某台已连接显示器的可视区内，否则放弃恢复（保持默认居中）。
    if position_on_any_monitor(&app, x, y) {
        let _ = win.set_position(tauri::Position::Physical(PhysicalPosition::new(x, y)));
        return true;
    }

    false
}

/// 判断物理坐标是否仍落在任意一台已连接显示器的可视区域内
fn position_on_any_monitor(app: &tauri::AppHandle, x: i32, y: i32) -> bool {
    match app.available_monitors() {
        Ok(monitors) => monitors.iter().any(|m| {
            let p = m.position();
            let s = m.size();
            x >= p.x && x < p.x + s.width as i32 && y >= p.y && y < p.y + s.height as i32
        }),
        Err(_) => false,
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![
            greet,
            restore_window_state,
            restore_app_state
        ])
        .setup(|app| {
            // 系统托盘：Checkbox「允许点击穿透」控制是否点击穿透
            // 默认未勾选 = 正常模式（显示标题栏/边框、可交互、白底）
            let penetrate_item = CheckMenuItemBuilder::with_id("penetrate", "允许点击穿透")
                .checked(false)
                .enabled(true)
                .build(app)?;
            let top_item = CheckMenuItemBuilder::with_id("always_on_top", "窗口置顶")
                .checked(true) // 与 tauri.conf.json 的 alwaysOnTop: true 保持一致
                .enabled(true)
                .build(app)?;
            let quit_item = MenuItemBuilder::with_id("quit", "退出").build(app)?;
            // 任务栏图标已隐藏，若取消置顶后窗口被其它窗口盖住，靠这项把它拉回前台
            let show_item = MenuItemBuilder::with_id("show", "显示窗口（置前）").build(app)?;

            let menu = MenuBuilder::new(app)
                .item(&penetrate_item)
                .item(&top_item)
                .separator()
                .item(&show_item)
                .item(&quit_item)
                .build()?;

            // menu item 句柄是 Clone（内部 Arc），克隆一份进全局状态与事件闭包
            app.manage(TrayMenuState {
                penetrate: penetrate_item.clone(),
                always_on_top: top_item.clone(),
            });
            let penetrate_item_ev = penetrate_item.clone();
            let top_item_ev = top_item.clone();

            TrayIconBuilder::with_id("main-tray")
                .icon(app.default_window_icon().unwrap().clone())
                .menu(&menu)
                // 左键点击也弹出菜单（右键本来就会弹）
                .show_menu_on_left_click(true)
                .on_menu_event(move |app, event| match event.id().as_ref() {
                    "quit" => {
                        // 先显式关闭窗口再退出进程。
                        // 直接 exit 的话，WebView2 会在进程退场时反注册窗口类失败，
                        // 打出 "Failed to unregister class Chrome_WidgetWin_0. Error = 1412"
                        // （Chromium 内部问题，无害，但先关窗可以让它不再出现）。
                        if let Some(win) = app.get_webview_window("main") {
                            if win.close().is_err() {
                                let _ = win.destroy(); // 兜底：强制销毁
                            }
                        }
                        app.exit(0);
                    }
                    "penetrate" => {
                        // 从菜单项本身读取勾选状态（穿透 / 正常）
                        let checked = penetrate_item_ev.is_checked().unwrap_or(false);
                        apply_penetrate(app, checked);
                    }
                    "show" => {
                        if let Some(win) = app.get_webview_window("main") {
                            let _ = win.unminimize();
                            let _ = win.show();
                            let _ = win.set_focus();
                        }
                    }
                    "always_on_top" => {
                        // 从菜单项本身读取勾选状态（置顶 / 不置顶）
                        let checked = top_item_ev.is_checked().unwrap_or(true);
                        apply_always_on_top(app, checked);
                    }
                    _ => {}
                })
                .build(app)?;

            // 启动即按默认（正常模式）初始化一次
            apply_penetrate(app.handle(), false);

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

// 切换窗口置顶状态（托盘 Checkbox「窗口置顶」）
fn apply_always_on_top<R: tauri::Runtime>(app: &tauri::AppHandle<R>, on_top: bool) {
    if let Some(win) = app.get_webview_window("main") {
        let _ = win.set_always_on_top(on_top);
    }
    // 通知前端落盘（localStorage）
    let _ = app.emit("always-on-top-change", on_top);
}

// 切换穿透状态：
//   穿透(on)   -> 去标题栏/边框、关阴影、忽略鼠标事件、透明背景
//   正常(off)  -> 恢复标题栏/边框、可拖动可缩放、白底
fn apply_penetrate<R: tauri::Runtime>(app: &tauri::AppHandle<R>, penetrate: bool) {
    if let Some(win) = app.get_webview_window("main") {
        // 关键顺序：必须先关 shadow 再去边框。
        // Windows 下「透明 + 无边框 + shadow=true」时，DWM 的窗口阴影会渲染成
        // 一块去不掉的黑色边框（Tauri v2 已知问题），所以穿透态一律 shadow=false。
        let _ = win.set_shadow(!penetrate);
        let _ = win.set_decorations(!penetrate);
        let _ = win.set_resizable(!penetrate);
        let _ = win.set_ignore_cursor_events(penetrate);

        // 强制重绘：部分 Windows 环境下切换边框后会残留黑色边框，
        // 用一次 1px 尺寸抖动把它刷掉（随即恢复原尺寸）。
        if let Ok(size) = win.inner_size() {
            let _ = win.set_size(tauri::Size::Physical(PhysicalSize::new(
                size.width + 1,
                size.height,
            )));
            let _ = win.set_size(tauri::Size::Physical(PhysicalSize::new(
                size.width,
                size.height,
            )));
        }
    }
    // 通知前端切换 CSS（透明 / 白底）
    let _ = app.emit("penetrate-change", penetrate);
}
