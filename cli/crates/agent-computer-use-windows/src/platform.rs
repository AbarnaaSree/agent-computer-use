#[cfg(not(target_os = "windows"))]
mod stub {
    use agent_computer_use_core::action::{Action, ActionResult, MouseButton};
    use agent_computer_use_core::node::AccessibilityNode;
    use agent_computer_use_core::platform::{AppInfo, Platform, WindowInfo};
    use agent_computer_use_core::selector::Selector;
    use agent_computer_use_core::{Error, Result};
    use async_trait::async_trait;

    pub struct WindowsPlatform;

    impl WindowsPlatform {
        pub fn new() -> Self {
            Self
        }
    }

    impl Default for WindowsPlatform {
        fn default() -> Self {
            Self::new()
        }
    }

    fn not_implemented() -> Error {
        Error::UnsupportedPlatform {
            platform: "Windows backend requires Windows OS".into(),
        }
    }

    #[async_trait]
    impl Platform for WindowsPlatform {
        async fn tree(
            &self,
            _app: Option<&str>,
            _max_depth: Option<u32>,
        ) -> Result<AccessibilityNode> {
            Err(not_implemented())
        }
        async fn find(&self, _selector: &Selector) -> Result<Vec<AccessibilityNode>> {
            Err(not_implemented())
        }
        async fn perform(&self, _action: &Action) -> Result<ActionResult> {
            Err(not_implemented())
        }
        async fn focused(&self) -> Result<AccessibilityNode> {
            Err(not_implemented())
        }
        async fn applications(&self) -> Result<Vec<AppInfo>> {
            Err(not_implemented())
        }
        async fn windows(&self, _app: Option<&str>) -> Result<Vec<WindowInfo>> {
            Err(not_implemented())
        }
        async fn text(&self, _app: Option<&str>) -> Result<String> {
            Err(not_implemented())
        }
        async fn check_permissions(&self) -> Result<bool> {
            Err(not_implemented())
        }
        fn platform_name(&self) -> &'static str {
            "Windows"
        }
    }
}

#[cfg(not(target_os = "windows"))]
pub use stub::WindowsPlatform;

#[cfg(target_os = "windows")]
mod real {
    use agent_computer_use_core::action::{Action, ActionResult, MouseButton};
    use agent_computer_use_core::node::{AccessibilityNode, Point};
    use agent_computer_use_core::platform::{AppInfo, Platform, WindowInfo};
    use agent_computer_use_core::selector::Selector;
    use agent_computer_use_core::{Error, Result};
    use async_trait::async_trait;
    use std::collections::HashMap;
    use std::sync::OnceLock;
    use windows::Win32::Foundation::*;
    use windows::Win32::System::Threading::*;
    use windows::Win32::UI::WindowsAndMessaging::*;
    use windows::Win32::UI::HiDpi::{
        SetProcessDpiAwarenessContext,
        DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
    };
    use crate::input;
    use crate::uia::{self, UiaContext};

    pub struct WindowsPlatform {
        uia: UiaContext,
    }

    unsafe impl Send for WindowsPlatform {}
    unsafe impl Sync for WindowsPlatform {}

    static INIT: OnceLock<()> = OnceLock::new();

    impl WindowsPlatform {
        pub fn new() -> Self {
            unsafe {
                let _dpi_result = SetProcessDpiAwarenessContext(
                    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
                );
            }

            INIT.get_or_init(|| unsafe {
                let _ = windows::Win32::System::Com::CoInitializeEx(
                    None,
                    windows::Win32::System::Com::COINIT_MULTITHREADED,
                );
            });

            let uia = UiaContext::new().expect("failed to initialize UI Automation");
            Self { uia }
        }


        fn find_app_pid(&self, app_name: &str) -> Result<u32> {
            let apps = self.list_processes()?;

            crate::process_matching::find_process_id(app_name, &apps)
                .ok_or_else(|| Error::ApplicationNotFound {
                    name: app_name.to_string(),
                })
        }


        fn list_processes(&self) -> Result<Vec<(u32, String)>> {
            use windows::Win32::System::ProcessStatus::*;

            unsafe {
                let mut pids = vec![0u32; 4096];
                let mut bytes_returned = 0u32;
                EnumProcesses(
                    pids.as_mut_ptr(),
                    (pids.len() * std::mem::size_of::<u32>()) as u32,
                    &mut bytes_returned,
                )
                .map_err(|e| Error::PlatformError {
                    message: format!("EnumProcesses failed: {e}"),
                })?;

                let count = bytes_returned as usize / std::mem::size_of::<u32>();
                let mut result = Vec::new();

                for &pid in &pids[..count] {
                    if pid == 0 {
                        continue;
                    }
                    if let Ok(handle) =
                        OpenProcess(PROCESS_QUERY_INFORMATION | PROCESS_VM_READ, false, pid)
                    {
                        let mut name_buf = [0u16; 260];
                        let len = GetModuleBaseNameW(handle, None, &mut name_buf);
                        if len > 0 {
                            let name = String::from_utf16_lossy(&name_buf[..len as usize]);
                            let name = name.trim_end_matches(".exe").to_string();
                            result.push((pid, name));
                        }
                        let _ = CloseHandle(handle);
                    }
                }

                Ok(result)
            }
        }

        fn get_app_root(
            &self,
            app: Option<&str>,
        ) -> Result<windows::Win32::UI::Accessibility::IUIAutomationElement> {
            match app {
                Some(name) => {
                    let pid = self.find_app_pid(name)?;

                    let hwnd = find_window_for_pid(pid).ok_or_else(|| {
                        Error::ApplicationNotFound {
                            name: name.to_string(),
                        }
                    })?;

                    self.uia.element_from_handle(hwnd)
                }
                None => self.uia.root(),
            }
        }
    }

    impl Default for WindowsPlatform {
        fn default() -> Self {
            Self::new()
        }
    }

    #[async_trait]
    impl Platform for WindowsPlatform {
        async fn tree(
            &self,
            app: Option<&str>,
            max_depth: Option<u32>,
        ) -> Result<AccessibilityNode> {
            let root = self.get_app_root(app)?;
            Ok(uia::element_to_node(&root, max_depth, 0))
        }

        async fn find(&self, selector: &Selector) -> Result<Vec<AccessibilityNode>> {
            let root = self.get_app_root(selector.app.as_deref())?;
            Ok(uia::find_all(&root, selector))
        }

        async fn perform(&self, action: &Action) -> Result<ActionResult> {
            match action {
                Action::Click {
                    selector,
                    coordinates,
                    button,
                    count,
                } => {
                    let point = match (selector, coordinates) {
                        (_, Some(coords)) => *coords,
                        (Some(sel), None) => {
                            let node = self.find_one(sel).await?;
                            node.center().ok_or_else(|| Error::PlatformError {
                                message: "element has no position".into(),
                            })?
                        }
                        (None, None) => {
                            return Err(Error::PlatformError {
                                message: "click requires selector or coordinates".into(),
                            })
                        }
                    };

                    input::click(point, *button, *count)?;
                    Ok(ActionResult {
                        success: true,
                        message: Some(format!("clicked at ({}, {})", point.x, point.y)),
                        path: None,
                        data: None,
                    })
                }

                Action::Type {
                    text,
                    selector,
                    submit,
                } => {
                    if let Some(sel) = selector {
                        let root = self.get_app_root(sel.app.as_deref())?;
                        if let Some(element) = uia::find_first(&root, sel) {
                            uia::set_element_value(&element, text);
                        }
                    } else {
                        input::type_text(text)?;
                    }

                    if *submit {
                        std::thread::sleep(std::time::Duration::from_millis(100));
                        input::key_press("return")?;
                    }

                    Ok(ActionResult {
                        success: true,
                        message: Some(format!("typed {} characters", text.len())),
                        path: None,
                        data: None,
                    })
                }

                Action::KeyPress { key, app } => {
                    if let Some(app_name) = app {
                        self.activate(app_name).await?;
                    }
                    input::key_press(key)?;
                    Ok(ActionResult {
                        success: true,
                        message: Some(format!("pressed {key}")),
                        path: None,
                        data: None,
                    })
                }

                Action::Scroll {
                    direction,
                    amount,
                    selector: _,
                    app,
                } => {
                    if let Some(app_name) = app {
                        self.activate(app_name).await?;
                        std::thread::sleep(std::time::Duration::from_millis(100));
                        unsafe {
                            let pid = self.find_app_pid(app_name)?;
                            if let Some(hwnd) = find_window_for_pid(pid) {
                                let mut rect = RECT::default();
                                if GetWindowRect(hwnd, &mut rect).is_ok() {
                                    let cx = (rect.left + rect.right) as f64 / 2.0;
                                    let cy = (rect.top + rect.bottom) as f64 / 2.0;
                                    input::move_mouse(Point { x: cx, y: cy })?;
                                    std::thread::sleep(std::time::Duration::from_millis(20));
                                }
                            }
                        }
                    }
                    input::scroll(*direction, *amount)?;
                    Ok(ActionResult {
                        success: true,
                        message: Some(format!("scrolled {direction:?} by {amount}")),
                        path: None,
                        data: None,
                    })
                }

                Action::MoveMouse {
                    selector,
                    coordinates,
                } => {
                    let point = match (selector, coordinates) {
                        (_, Some(coords)) => *coords,
                        (Some(sel), None) => {
                            let node = self.find_one(sel).await?;
                            node.center().ok_or_else(|| Error::PlatformError {
                                message: "element has no position".into(),
                            })?
                        }
                        (None, None) => {
                            return Err(Error::PlatformError {
                                message: "move requires selector or coordinates".into(),
                            })
                        }
                    };

                    input::move_mouse(point)?;
                    Ok(ActionResult {
                        success: true,
                        message: Some(format!("moved to ({}, {})", point.x, point.y)),
                        path: None,
                        data: None,
                    })
                }

                Action::Screenshot { path, app: _ } => {
                    let save_path = path.clone().unwrap_or_else(|| {
                        format!(
                            "{}\\agent-computer-use-screenshot.png",
                            std::env::temp_dir().display()
                        )
                    });

                    let ps_script = format!(
                        r#"Add-Type -AssemblyName System.Windows.Forms; Add-Type -AssemblyName System.Drawing; $bounds = [System.Windows.Forms.Screen]::PrimaryScreen.Bounds; $bmp = New-Object System.Drawing.Bitmap($bounds.Width, $bounds.Height); $g = [System.Drawing.Graphics]::FromImage($bmp); $g.CopyFromScreen($bounds.Location, [System.Drawing.Point]::Empty, $bounds.Size); $bmp.Save('{}', [System.Drawing.Imaging.ImageFormat]::Png); $g.Dispose(); $bmp.Dispose()"#,
                        save_path.replace('\'', "''")
                    );

                    let result = std::process::Command::new("powershell")
                        .args(["-NoProfile", "-Command", &ps_script])
                        .output();

                    match result {
                        Ok(output) if output.status.success() => Ok(ActionResult {
                            success: true,
                            message: Some(format!("screenshot saved to {save_path}")),
                            path: Some(save_path),
                            data: None,
                        }),
                        Ok(output) => {
                            let stderr = String::from_utf8_lossy(&output.stderr);
                            Err(Error::PlatformError {
                                message: format!("screenshot failed: {stderr}"),
                            })
                        }
                        Err(e) => Err(Error::PlatformError {
                            message: format!("failed to run powershell for screenshot: {e}"),
                        }),
                    }
                }

                Action::Focus { selector } => {
                    if let Some(app) = selector.app.as_deref() {
                        self.activate(app).await?;
                    }

                    let root = self.get_app_root(selector.app.as_deref())?;

                    let element = uia::find_first(&root, selector)
                        .ok_or_else(|| Error::PlatformError {
                            message: "focus target element not found".into(),
                        })?;

                    unsafe {
                        let _ = element.SetFocus();
                        let target_pid = element.CurrentProcessId().ok();

                        let mut has_focus = match (target_pid, self.uia.focused_element()) {
                            (Some(target_pid), Ok(focused_element)) => {
                                focused_element
                                    .CurrentProcessId()
                                    .map(|focused_pid| focused_pid == target_pid)
                                    .unwrap_or(false)
                            }
                            _ => false,
                        };

                        if !has_focus {
                            let node = uia::element_to_node(&element, Some(0), 0);

                            let point = node.center().ok_or_else(|| Error::PlatformError {
                                message: "focus target has no clickable position".into(),
                            })?;

                            input::click(point, MouseButton::Left, 1)?;

                            std::thread::sleep(std::time::Duration::from_millis(100));

                            has_focus = match (target_pid, self.uia.focused_element()) {
                                (Some(target_pid), Ok(focused_element)) => {
                                    focused_element
                                        .CurrentProcessId()
                                        .map(|focused_pid| focused_pid == target_pid)
                                        .unwrap_or(false)
                                }
                                _ => false,
                            };
                        }

                        if !has_focus {
                            return Err(Error::PlatformError {
                                message: "keyboard focus could not be verified after clicking the target".into(),
                            });
                        }
                    }

                    Ok(ActionResult {
                        success: true,
                        message: Some("element focused and verified successfully".into()),
                        path: None,
                        data: None,
                    })
                }

                Action::Drag { from, to } => {
                    unsafe {
                        let pt = POINT {
                            x: from.x as i32,
                            y: from.y as i32,
                        };
                        let hwnd = WindowFromPoint(pt);
                        if hwnd != HWND::default() {
                            let root = GetAncestor(hwnd, GA_ROOT);
                            let target = if root != HWND::default() { root } else { hwnd };
                            force_foreground(target);
                            std::thread::sleep(std::time::Duration::from_millis(80));
                        }
                    }
                    input::drag(*from, *to)?;
                    Ok(ActionResult {
                        success: true,
                        message: Some(format!(
                            "dragged from ({},{}) to ({},{})",
                            from.x, from.y, to.x, to.y
                        )),
                        path: None,
                        data: None,
                    })
                }
            }
        }

        async fn focused(&self) -> Result<AccessibilityNode> {
            let element = self.uia.focused_element()?;
            Ok(uia::element_to_node(&element, Some(0), 0))
        }

        async fn applications(&self) -> Result<Vec<AppInfo>> {
            let processes = self.list_processes()?;
            let mut apps = Vec::new();
            let mut seen = std::collections::HashSet::new();

            unsafe {
                let foreground = GetForegroundWindow();
                let mut fg_pid = 0u32;
                if foreground != HWND::default() {
                    GetWindowThreadProcessId(foreground, Some(&mut fg_pid));
                }

                for (pid, name) in &processes {
                    if seen.contains(name) {
                        continue;
                    }
                    if has_visible_window(*pid) {
                        seen.insert(name.clone());
                        apps.push(AppInfo {
                            name: name.clone(),
                            pid: *pid,
                            frontmost: *pid == fg_pid,
                            bundle_id: None,
                        });
                    }
                }
            }

            Ok(apps)
        }

        async fn windows(&self, app: Option<&str>) -> Result<Vec<WindowInfo>> {
            let target_pid = app.map(|name| self.find_app_pid(name)).transpose()?;
            let mut windows = Vec::new();

            let pid_to_name: HashMap<u32, String> = self.list_processes()?.into_iter().collect();

            unsafe {
                let foreground = GetForegroundWindow();

                EnumWindows(
                    Some(enum_windows_callback),
                    LPARAM(&mut windows as *mut Vec<WindowInfo> as isize),
                )
                .ok();

                if let Some(pid) = target_pid {
                    windows.retain(|w: &WindowInfo| w.pid == pid);
                }

                for w in &mut windows {
                    if let Some(name) = pid_to_name.get(&w.pid) {
                        w.app = name.clone();
                    }
                    let hwnd = find_window_by_title(&w.title);
                    if let Some(h) = hwnd {
                        w.frontmost = Some(h == foreground);
                    }
                }
            }

            Ok(windows)
        }

        async fn text(&self, app: Option<&str>) -> Result<String> {
            let root = self.get_app_root(app)?;
            Ok(uia::collect_text(&root, Some(20), 0))
        }

        async fn check_permissions(&self) -> Result<bool> {
            Ok(true)
        }

        async fn activate(&self, app: &str) -> Result<()> {
            let pid = self.find_app_pid(app)?;

            let hwnd = find_window_for_pid(pid)
                .or_else(|| find_window_by_title(app))
                .ok_or_else(|| Error::PlatformError {
                    message: format!("no window found for '{app}'"),
                })?;

            if !force_foreground(hwnd) {
                return Err(Error::PlatformError {
                    message: format!(
                        "could not bring application '{app}' to the foreground"
                    ),
                });
            }
            Ok(())
        }

        async fn press(&self, selector: &Selector) -> Result<bool> {
            let root = self.get_app_root(selector.app.as_deref())?;
            if let Some(element) = uia::find_first(&root, selector) {
                Ok(uia::invoke_element(&element))
            } else {
                Ok(false)
            }
        }

        async fn set_value(&self, selector: &Selector, value: &str) -> Result<bool> {
            let root = self.get_app_root(selector.app.as_deref())?;
            if let Some(element) = uia::find_first(&root, selector) {
                Ok(uia::set_element_value(&element, value))
            } else {
                Ok(false)
            }
        }

        async fn scroll_to_visible(&self, selector: &Selector) -> Result<bool> {
            let root = self.get_app_root(selector.app.as_deref())?;
            if let Some(element) = uia::find_first(&root, selector) {
                Ok(uia::scroll_element_into_view(&element))
            } else {
                Ok(false)
            }
        }

        async fn open_application(&self, app: &str) -> Result<()> {
            use std::process::Command;
            let result = Command::new("cmd").args(["/C", "start", "", app]).spawn();

            match result {
                Ok(_) => Ok(()),
                Err(e) => Err(Error::PlatformError {
                    message: format!("failed to open '{app}': {e}"),
                }),
            }
        }

        async fn move_window(&self, app: &str, x: f64, y: f64) -> Result<bool> {
            unsafe {
                let pid = self.find_app_pid(app)?;
                let hwnd = find_window_for_pid(pid).ok_or_else(|| Error::PlatformError {
                    message: format!("no window found for '{app}'"),
                })?;

                let mut rect = RECT::default();
                let _ = GetWindowRect(hwnd, &mut rect);
                let width = rect.right - rect.left;
                let height = rect.bottom - rect.top;

                SetWindowPos(
                    hwnd,
                    None,
                    x as i32,
                    y as i32,
                    width,
                    height,
                    SWP_NOZORDER | SWP_NOSIZE | SWP_NOACTIVATE,
                )
                .map_err(|e| Error::PlatformError {
                    message: format!("SetWindowPos failed: {e}"),
                })?;

                Ok(true)
            }
        }

        async fn resize_window(&self, app: &str, width: f64, height: f64) -> Result<bool> {
            unsafe {
                let pid = self.find_app_pid(app)?;
                let hwnd = find_window_for_pid(pid).ok_or_else(|| Error::PlatformError {
                    message: format!("no window found for '{app}'"),
                })?;

                let mut rect = RECT::default();
                let _ = GetWindowRect(hwnd, &mut rect);

                SetWindowPos(
                    hwnd,
                    None,
                    rect.left,
                    rect.top,
                    width as i32,
                    height as i32,
                    SWP_NOZORDER | SWP_NOMOVE | SWP_NOACTIVATE,
                )
                .map_err(|e| Error::PlatformError {
                    message: format!("SetWindowPos failed: {e}"),
                })?;

                Ok(true)
            }
        }

        fn platform_name(&self) -> &'static str {
            "Windows"
        }
    }

    fn has_visible_window(pid: u32) -> bool {
        find_window_for_pid(pid).is_some()
    }




    fn force_foreground(hwnd: HWND) -> bool {
        unsafe {
            eprintln!(
                "[WINDOW DEBUG] Selected HWND: {:?}, IsIconic: {}",
                hwnd,
                IsIconic(hwnd).as_bool()
            );

            if IsIconic(hwnd).as_bool() {
                let _ = ShowWindow(hwnd, SW_RESTORE);
            } else {
                let _ = ShowWindow(hwnd, SW_SHOW);
            }

            eprintln!(
                "[WINDOW DEBUG] After restore: HWND: {:?}, IsIconic: {}",
                hwnd,
                IsIconic(hwnd).as_bool()
            );

            std::thread::sleep(std::time::Duration::from_millis(300));

            if GetForegroundWindow() == hwnd {
                return !IsIconic(hwnd).as_bool();
            }

            let current_thread = GetCurrentThreadId();
            let target_thread = GetWindowThreadProcessId(hwnd, None);
            let foreground = GetForegroundWindow();

            let foreground_thread = if foreground != HWND::default() {
                GetWindowThreadProcessId(foreground, None)
            } else {
                0
            };

            let attached_target = target_thread != 0
                && target_thread != current_thread
                && AttachThreadInput(current_thread, target_thread, true).as_bool();

            let attached_foreground = foreground_thread != 0
                && foreground_thread != current_thread
                && foreground_thread != target_thread
                && AttachThreadInput(current_thread, foreground_thread, true).as_bool();

            let _ = BringWindowToTop(hwnd);
            let _ = SetForegroundWindow(hwnd);

            if attached_foreground {
                let _ = AttachThreadInput(current_thread, foreground_thread, false);
            }

            if attached_target {
                let _ = AttachThreadInput(current_thread, target_thread, false);
            }

            std::thread::sleep(std::time::Duration::from_millis(300));

            GetForegroundWindow() == hwnd && !IsIconic(hwnd).as_bool()
        }
    }




    struct FindPidContext {
        target_pid: u32,
        windows: Vec<HWND>,
    }

    fn find_window_for_pid(pid: u32) -> Option<HWND> {
        unsafe {
            let mut ctx = FindPidContext {
                target_pid: pid,
                windows: Vec::new(),
            };

            let _ = EnumWindows(
                Some(find_pid_callback),
                LPARAM(&mut ctx as *mut FindPidContext as isize),
            );

            // Prefer a non-minimized window.
            ctx.windows
                .iter()
                .copied()
                .find(|hwnd| !IsIconic(*hwnd).as_bool())
                .or_else(|| ctx.windows.first().copied())
        }
    }

    unsafe extern "system" fn find_pid_callback(
        hwnd: HWND,
        lparam: LPARAM,
    ) -> BOOL {
        let ctx = &mut *(lparam.0 as *mut FindPidContext);

        let mut window_pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut window_pid));

        if window_pid != ctx.target_pid
            || !IsWindowVisible(hwnd).as_bool()
        {
            return TRUE;
        }

        let style = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;

        if style & WS_EX_TOOLWINDOW.0 != 0 {
            return TRUE;
        }

        ctx.windows.push(hwnd);

        TRUE
    }

    unsafe extern "system" fn enum_windows_callback(hwnd: HWND, lparam: LPARAM) -> BOOL {
        if !IsWindowVisible(hwnd).as_bool() {
            return TRUE;
        }

        let style = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
        if style & WS_EX_TOOLWINDOW.0 != 0 {
            return TRUE;
        }

        let mut title_buf = [0u16; 512];
        let len = GetWindowTextW(hwnd, &mut title_buf);
        if len == 0 {
            return TRUE;
        }
        let title = String::from_utf16_lossy(&title_buf[..len as usize]);

        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));

        let mut rect = RECT::default();
        let _ = GetWindowRect(hwnd, &mut rect);

        let windows = &mut *(lparam.0 as *mut Vec<WindowInfo>);
        windows.push(WindowInfo {
            title,
            app: String::new(),
            pid,
            position: Some(Point {
                x: rect.left as f64,
                y: rect.top as f64,
            }),
            size: Some(agent_computer_use_core::node::Size {
                width: (rect.right - rect.left) as f64,
                height: (rect.bottom - rect.top) as f64,
            }),
            minimized: Some(IsIconic(hwnd).as_bool()),
            frontmost: None,
        });

        TRUE
    }

    struct FindTitleContext {
        target_title: String,
        found: Option<HWND>,
    }

    unsafe extern "system" fn find_title_callback(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let ctx = &mut *(lparam.0 as *mut FindTitleContext);

        if !IsWindowVisible(hwnd).as_bool() || IsIconic(hwnd).as_bool() {
            return TRUE;
        }

        let style = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
        if style & WS_EX_TOOLWINDOW.0 != 0 {
            return TRUE;
        }

        let mut title_buf = [0u16; 512];
        let len = GetWindowTextW(hwnd, &mut title_buf);

        if len == 0 {
            return TRUE;
        }

        let window_title = String::from_utf16_lossy(&title_buf[..len as usize]);

        if window_title.eq_ignore_ascii_case(&ctx.target_title) {
            ctx.found = Some(hwnd);
            return FALSE;
        }

        TRUE
    }

    fn find_window_by_title(title: &str) -> Option<HWND> {
        unsafe {
            let mut ctx = FindTitleContext {
                target_title: title.to_string(),
                found: None,
            };

            let _ = EnumWindows(
                Some(find_title_callback),
                LPARAM(&mut ctx as *mut FindTitleContext as isize),
            );

            ctx.found
        }
    }
}

#[cfg(target_os = "windows")]
pub use real::WindowsPlatform;
