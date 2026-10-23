//! The app window and the tray icon (`bananasplit --window URL …`).
//!
//! `bananasplit --gui` runs the mixer and starts this as a separate process,
//! so the audio process never loads a toolkit: if the window crashes, the
//! mixer keeps running and the window can be opened again. GTK 3, WebKitGTK
//! and GLib are loaded at run time with `dlopen`, like libjack, so BananaSplit
//! still has no build dependencies, and one binary runs with or without them.
//!
//! * **Window:** a GTK window holding a WebKitGTK view of the mixer page
//!   (WebKitGTK 4.1 or 4.0). Closing it keeps BananaSplit running in the
//!   tray (or minimizes it if there is no tray), like Voicemeeter; with
//!   `close_to_tray = false` closing it quits.
//! * **Tray icon:** a StatusNotifierItem with its menu (`com.canonical.dbusmenu`),
//!   spoken directly over D-Bus with GIO, so it needs no AppIndicator
//!   library. KDE, Xfce, Cinnamon, Budgie, MATE and GNOME with the
//!   AppIndicator extension (Ubuntu's default) show it. Left click shows or
//!   hides the window; the menu has Show, Start/Stop recording and Quit.
//! * **Without WebKitGTK** this process only keeps the tray icon, and the
//!   mixer opens the page in a browser's app window instead.
//!
//! Talking to the mixer: one line per message. Mixer → here (stdin): `show
//! TOKEN|-`, `hide`, `rec 0|1`, `close-to-tray 0|1`, `quit`; end of input
//! means the mixer is gone. Here → mixer (stdout): `ready app|tray`, `fail
//! REASON`, `open` (show the page in a browser), `record`, `quit`, `size W H`.

use std::cell::RefCell;
use std::ffi::{CStr, CString, c_char, c_int, c_uint, c_ulong, c_void};
use std::io::Write;
use std::process::ExitCode;

type Ptr = *mut c_void;
type FdFn = unsafe extern "C" fn(c_int, c_uint, Ptr) -> c_int;
type SourceFn = unsafe extern "C" fn(Ptr) -> c_int;

unsafe extern "C" {
    fn dlopen(filename: *const c_char, flag: c_int) -> Ptr;
    fn dlsym(handle: Ptr, symbol: *const c_char) -> Ptr;
    fn dlerror() -> *mut c_char;
    fn read(fd: c_int, buf: *mut c_void, n: usize) -> isize;
}
const RTLD_NOW: c_int = 2;

fn dl_error() -> String {
    // SAFETY: dlerror returns NULL or a valid C string.
    let e = unsafe { dlerror() };
    if e.is_null() { "unknown error".into() } else { unsafe { CStr::from_ptr(e) }.to_string_lossy().into_owned() }
}

fn open_lib(names: &[&str]) -> Result<Ptr, String> {
    let mut errs = Vec::new();
    for n in names {
        let c = CString::new(*n).map_err(|e| e.to_string())?;
        // SAFETY: NUL-terminated library name.
        let h = unsafe { dlopen(c.as_ptr(), RTLD_NOW) };
        if !h.is_null() {
            return Ok(h);
        }
        errs.push(dl_error());
    }
    Err(errs.join("; "))
}

macro_rules! api {
    ($name:ident { $( $f:ident : $ty:ty ),* $(,)? }) => {
        struct $name { $( $f: $ty, )* }
        impl $name {
            /// # Safety
            /// `h` must be a handle of the library that declares these C functions.
            unsafe fn load(h: Ptr) -> Result<Self, String> {
                Ok(Self { $( $f: {
                    let sym = concat!(stringify!($f), "\0");
                    // SAFETY: valid handle and NUL-terminated name.
                    let p = unsafe { dlsym(h, sym.as_ptr().cast()) };
                    if p.is_null() {
                        return Err(format!("no symbol {}", stringify!($f)));
                    }
                    // SAFETY: the symbol has the C signature declared here (GLib/GTK/WebKit headers).
                    unsafe { std::mem::transmute::<Ptr, $ty>(p) }
                }, )* })
            }
        }
    };
}

api!(Glib {
    g_main_loop_new: unsafe extern "C" fn(Ptr, c_int) -> Ptr,
    g_main_loop_run: unsafe extern "C" fn(Ptr),
    g_main_loop_quit: unsafe extern "C" fn(Ptr),
    g_unix_fd_add: unsafe extern "C" fn(c_int, c_uint, FdFn, Ptr) -> c_uint,
    g_timeout_add: unsafe extern "C" fn(c_uint, SourceFn, Ptr) -> c_uint,
    g_variant_new_parsed: unsafe extern "C" fn(*const c_char, ...) -> Ptr,
    g_variant_new_int32: unsafe extern "C" fn(i32) -> Ptr,
    g_variant_new_tuple: unsafe extern "C" fn(*const Ptr, usize) -> Ptr,
    g_variant_new_array: unsafe extern "C" fn(Ptr, *const Ptr, usize) -> Ptr,
    g_variant_new_fixed_array: unsafe extern "C" fn(Ptr, *const c_void, usize, usize) -> Ptr,
    g_variant_type_new: unsafe extern "C" fn(*const c_char) -> Ptr,
    g_variant_type_free: unsafe extern "C" fn(Ptr),
    g_variant_get_child_value: unsafe extern "C" fn(Ptr, usize) -> Ptr,
    g_variant_get_int32: unsafe extern "C" fn(Ptr) -> i32,
    g_variant_get_boolean: unsafe extern "C" fn(Ptr) -> c_int,
    g_variant_get_string: unsafe extern "C" fn(Ptr, *mut usize) -> *const c_char,
    g_variant_get_variant: unsafe extern "C" fn(Ptr) -> Ptr,
    g_variant_get_type_string: unsafe extern "C" fn(Ptr) -> *const c_char,
    g_variant_get_uint32: unsafe extern "C" fn(Ptr) -> u32,
    g_variant_n_children: unsafe extern "C" fn(Ptr) -> usize,
    g_variant_unref: unsafe extern "C" fn(Ptr),
    g_set_prgname: unsafe extern "C" fn(*const c_char),
    g_set_application_name: unsafe extern "C" fn(*const c_char),
});

api!(Gobject {
    g_signal_connect_data: unsafe extern "C" fn(Ptr, *const c_char, Ptr, Ptr, Ptr, c_int) -> c_ulong,
    g_object_set: unsafe extern "C" fn(Ptr, *const c_char, ...),
});

#[repr(C)]
struct VTable {
    method_call: unsafe extern "C" fn(Ptr, *const c_char, *const c_char, *const c_char, *const c_char, Ptr, Ptr, Ptr),
    get_property: unsafe extern "C" fn(Ptr, *const c_char, *const c_char, *const c_char, *const c_char, *mut Ptr, Ptr) -> Ptr,
    set_property: Ptr,
    padding: [Ptr; 8],
}
// SAFETY: plain function pointers and NULLs, never mutated.
unsafe impl Sync for VTable {}

api!(Gio {
    g_bus_get_sync: unsafe extern "C" fn(c_int, Ptr, *mut Ptr) -> Ptr,
    g_dbus_node_info_new_for_xml: unsafe extern "C" fn(*const c_char, *mut Ptr) -> Ptr,
    g_dbus_node_info_lookup_interface: unsafe extern "C" fn(Ptr, *const c_char) -> Ptr,
    g_dbus_connection_register_object: unsafe extern "C" fn(Ptr, *const c_char, Ptr, *const VTable, Ptr, Ptr, *mut Ptr) -> c_uint,
    g_dbus_connection_emit_signal: unsafe extern "C" fn(Ptr, *const c_char, *const c_char, *const c_char, *const c_char, Ptr, *mut Ptr) -> c_int,
    g_dbus_connection_call: unsafe extern "C" fn(Ptr, *const c_char, *const c_char, *const c_char, *const c_char, Ptr, Ptr, c_int, c_int, Ptr, Ptr, Ptr),
    g_dbus_connection_call_finish: unsafe extern "C" fn(Ptr, Ptr, *mut Ptr) -> Ptr,
    g_dbus_connection_call_sync: unsafe extern "C" fn(Ptr, *const c_char, *const c_char, *const c_char, *const c_char, Ptr, Ptr, c_int, c_int, Ptr, *mut Ptr) -> Ptr,
    g_bus_own_name_on_connection: unsafe extern "C" fn(Ptr, *const c_char, c_int, Ptr, Ptr, Ptr, Ptr) -> c_uint,
    g_bus_watch_name_on_connection: unsafe extern "C" fn(Ptr, *const c_char, c_int, Ptr, Ptr, Ptr, Ptr) -> c_uint,
    g_dbus_method_invocation_return_value: unsafe extern "C" fn(Ptr, Ptr),
    g_dbus_method_invocation_return_dbus_error: unsafe extern "C" fn(Ptr, *const c_char, *const c_char),
    g_dbus_connection_signal_subscribe: unsafe extern "C" fn(
        Ptr,
        *const c_char,
        *const c_char,
        *const c_char,
        *const c_char,
        *const c_char,
        c_int,
        Ptr,
        Ptr,
        Ptr,
    ) -> c_uint,
});

api!(Gtk {
    gtk_init_check: unsafe extern "C" fn(*mut c_int, *mut Ptr) -> c_int,
    gtk_window_new: unsafe extern "C" fn(c_int) -> Ptr,
    gtk_window_set_title: unsafe extern "C" fn(Ptr, *const c_char),
    gtk_window_set_default_size: unsafe extern "C" fn(Ptr, c_int, c_int),
    gtk_window_set_icon_name: unsafe extern "C" fn(Ptr, *const c_char),
    gtk_window_set_default_icon_from_file: unsafe extern "C" fn(*const c_char, *mut Ptr) -> c_int,
    gtk_widget_set_size_request: unsafe extern "C" fn(Ptr, c_int, c_int),
    gtk_container_add: unsafe extern "C" fn(Ptr, Ptr),
    gtk_widget_show_all: unsafe extern "C" fn(Ptr),
    gtk_widget_hide: unsafe extern "C" fn(Ptr),
    gtk_widget_get_visible: unsafe extern "C" fn(Ptr) -> c_int,
    gtk_window_present: unsafe extern "C" fn(Ptr),
    gtk_window_set_startup_id: unsafe extern "C" fn(Ptr, *const c_char),
    gtk_window_iconify: unsafe extern "C" fn(Ptr),
    gtk_window_deiconify: unsafe extern "C" fn(Ptr),
    gtk_window_is_active: unsafe extern "C" fn(Ptr) -> c_int,
    gtk_window_get_size: unsafe extern "C" fn(Ptr, *mut c_int, *mut c_int),
    gtk_message_dialog_new: unsafe extern "C" fn(Ptr, c_int, c_int, c_int, *const c_char, ...) -> Ptr,
    gtk_dialog_run: unsafe extern "C" fn(Ptr) -> c_int,
    gtk_settings_get_default: unsafe extern "C" fn() -> Ptr,
    gtk_widget_destroy: unsafe extern "C" fn(Ptr),
});

#[repr(C)]
#[derive(Default)]
struct GdkRectangle {
    x: c_int,
    y: c_int,
    width: c_int,
    height: c_int,
}

#[repr(C)]
struct GdkRgba {
    red: f64,
    green: f64,
    blue: f64,
    alpha: f64,
}

api!(Gdk {
    gdk_set_program_class: unsafe extern "C" fn(*const c_char),
    gdk_display_get_default: unsafe extern "C" fn() -> Ptr,
    gdk_display_get_primary_monitor: unsafe extern "C" fn(Ptr) -> Ptr,
    gdk_display_get_monitor: unsafe extern "C" fn(Ptr, c_int) -> Ptr,
    gdk_monitor_get_workarea: unsafe extern "C" fn(Ptr, *mut GdkRectangle),
});

api!(WebKit {
    webkit_web_view_new: unsafe extern "C" fn() -> Ptr,
    webkit_web_view_load_uri: unsafe extern "C" fn(Ptr, *const c_char),
    webkit_web_view_reload: unsafe extern "C" fn(Ptr),
    webkit_web_view_set_background_color: unsafe extern "C" fn(Ptr, *const GdkRgba),
    webkit_hit_test_result_context_is_editable: unsafe extern "C" fn(Ptr) -> c_int,
});

/// The libraries this process uses. Loaded once, kept for the whole run.
struct Libs {
    glib: Glib,
    gobject: Gobject,
    gio: Gio,
    /// GTK, GDK and WebKitGTK: all three, or none (tray only).
    ui: Option<(Gtk, Gdk, WebKit)>,
    /// Why there is no window (for the mixer's message).
    no_ui: String,
}

fn load_glib() -> Result<(Glib, Gobject, Gio), String> {
    // SAFETY: each handle is the library declaring the symbols.
    unsafe {
        Ok((
            Glib::load(open_lib(&["libglib-2.0.so.0"])?)?,
            Gobject::load(open_lib(&["libgobject-2.0.so.0"])?)?,
            Gio::load(open_lib(&["libgio-2.0.so.0"])?)?,
        ))
    }
}

fn load_gtk() -> Result<(Gtk, Gdk), String> {
    // SAFETY: as above.
    unsafe { Ok((Gtk::load(open_lib(&["libgtk-3.so.0"])?)?, Gdk::load(open_lib(&["libgdk-3.so.0"])?)?)) }
}

fn load_webkit() -> Result<WebKit, String> {
    let h = open_lib(&["libwebkit2gtk-4.1.so.0", "libwebkit2gtk-4.0.so.37"])
        .map_err(|_| "WebKitGTK is not installed (libwebkit2gtk-4.1 or 4.0)".to_string())?;
    // SAFETY: as above.
    unsafe { WebKit::load(h) }
}

fn cstr(s: &str) -> CString {
    CString::new(s.replace('\0', "")).unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Messages to the mixer
// ---------------------------------------------------------------------------

fn tell(msg: &str) {
    let mut out = std::io::stdout().lock();
    if out.write_all(msg.as_bytes()).and_then(|_| out.write_all(b"\n")).and_then(|_| out.flush()).is_err() {
        // The mixer is gone.
        quit();
    }
}

// ---------------------------------------------------------------------------
// State (GLib calls every callback on this one thread)
// ---------------------------------------------------------------------------

struct App {
    libs: &'static Libs,
    main_loop: Ptr,
    window: Ptr,
    view: Ptr,
    conn: Ptr,
    close_to_tray: bool,
    recording: bool,
    /// Our StatusNotifierItem is registered with a watcher.
    tray_registered: bool,
    watcher_present: bool,
    menu_rev: u32,
    stdin_buf: Vec<u8>,
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

/// Run `f` on the state. Never call GTK or D-Bus inside `f` (a signal they
/// emit synchronously would find the state borrowed); copy out what is
/// needed instead.
fn with<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    APP.with(|a| a.try_borrow_mut().ok().and_then(|mut g| g.as_mut().map(f)))
}

fn quit() {
    if let Some((libs, ml, win)) = with(|a| (a.libs, a.main_loop, a.window)) {
        if let (Some((gtk, _, _)), false) = (&libs.ui, win.is_null()) {
            let (mut w, mut h) = (0, 0);
            // SAFETY: valid window.
            unsafe { (gtk.gtk_window_get_size)(win, &mut w, &mut h) };
            if w > 0 && h > 0 {
                let _ = writeln!(std::io::stdout(), "size {w} {h}");
                let _ = std::io::stdout().flush();
            }
        }
        // SAFETY: valid main loop.
        unsafe { (libs.glib.g_main_loop_quit)(ml) };
    }
}

fn window_visible() -> bool {
    with(|a| (a.libs, a.window))
        .and_then(|(libs, win)| {
            let (gtk, _, _) = libs.ui.as_ref()?;
            // SAFETY: valid window.
            (!win.is_null()).then(|| unsafe { (gtk.gtk_widget_get_visible)(win) } != 0)
        })
        .unwrap_or(false)
}

fn show_window(token: Option<&str>) {
    let Some((libs, win)) = with(|a| (a.libs, a.window)) else { return };
    let Some((gtk, _, _)) = &libs.ui else {
        tell("open");
        return;
    };
    if win.is_null() {
        return;
    }
    // SAFETY: valid window; `t` outlives the call.
    unsafe {
        if let Some(t) = token.filter(|t| !t.is_empty() && *t != "-") {
            let t = cstr(t);
            (gtk.gtk_window_set_startup_id)(win, t.as_ptr());
        }
        (gtk.gtk_widget_show_all)(win);
        (gtk.gtk_window_deiconify)(win);
        (gtk.gtk_window_present)(win);
    }
    menu_changed();
}

fn hide_window() {
    let Some((libs, win)) = with(|a| (a.libs, a.window)) else { return };
    if let (Some((gtk, _, _)), false) = (&libs.ui, win.is_null()) {
        let (mut w, mut h) = (0, 0);
        // SAFETY: valid window.
        unsafe {
            (gtk.gtk_window_get_size)(win, &mut w, &mut h);
            (gtk.gtk_widget_hide)(win);
        }
        if w > 0 && h > 0 {
            tell(&format!("size {w} {h}"));
        }
    }
    menu_changed();
}

/// Left click on the tray icon: bring the window up, or hide it if it is
/// already in front.
fn toggle_window() {
    let Some((libs, win)) = with(|a| (a.libs, a.window)) else { return };
    let Some((gtk, _, _)) = &libs.ui else {
        tell("open");
        return;
    };
    // SAFETY: valid window.
    let front = !win.is_null() && unsafe { (gtk.gtk_widget_get_visible)(win) != 0 && (gtk.gtk_window_is_active)(win) != 0 };
    if front { hide_window() } else { show_window(None) }
}

// ---------------------------------------------------------------------------
// Tray: StatusNotifierItem + dbusmenu over GIO
// ---------------------------------------------------------------------------

const ITEM_PATH: &CStr = c"/StatusNotifierItem";
const MENU_PATH: &CStr = c"/MenuBar";
const ITEM_IFACE: &CStr = c"org.kde.StatusNotifierItem";
const MENU_IFACE: &CStr = c"com.canonical.dbusmenu";
const WATCHER: &CStr = c"org.kde.StatusNotifierWatcher";

const ITEM_XML: &CStr = c"<node><interface name='org.kde.StatusNotifierItem'>
<property name='Category' type='s' access='read'/><property name='Id' type='s' access='read'/>
<property name='Title' type='s' access='read'/><property name='Status' type='s' access='read'/>
<property name='WindowId' type='i' access='read'/><property name='IconName' type='s' access='read'/>
<property name='IconPixmap' type='a(iiay)' access='read'/><property name='OverlayIconName' type='s' access='read'/>
<property name='OverlayIconPixmap' type='a(iiay)' access='read'/><property name='AttentionIconName' type='s' access='read'/>
<property name='AttentionIconPixmap' type='a(iiay)' access='read'/><property name='AttentionMovieName' type='s' access='read'/>
<property name='ToolTip' type='(sa(iiay)ss)' access='read'/><property name='ItemIsMenu' type='b' access='read'/>
<property name='Menu' type='o' access='read'/><property name='IconThemePath' type='s' access='read'/>
<method name='ContextMenu'><arg name='x' type='i' direction='in'/><arg name='y' type='i' direction='in'/></method>
<method name='Activate'><arg name='x' type='i' direction='in'/><arg name='y' type='i' direction='in'/></method>
<method name='SecondaryActivate'><arg name='x' type='i' direction='in'/><arg name='y' type='i' direction='in'/></method>
<method name='Scroll'><arg name='delta' type='i' direction='in'/><arg name='orientation' type='s' direction='in'/></method>
<signal name='NewTitle'/><signal name='NewIcon'/><signal name='NewAttentionIcon'/><signal name='NewOverlayIcon'/>
<signal name='NewToolTip'/><signal name='NewStatus'><arg name='status' type='s'/></signal>
</interface></node>";

const MENU_XML: &CStr = c"<node><interface name='com.canonical.dbusmenu'>
<property name='Version' type='u' access='read'/><property name='TextDirection' type='s' access='read'/>
<property name='Status' type='s' access='read'/><property name='IconThemePath' type='as' access='read'/>
<method name='GetLayout'><arg type='i' name='parentId' direction='in'/><arg type='i' name='recursionDepth' direction='in'/>
<arg type='as' name='propertyNames' direction='in'/><arg type='u' name='revision' direction='out'/>
<arg type='(ia{sv}av)' name='layout' direction='out'/></method>
<method name='GetGroupProperties'><arg type='ai' name='ids' direction='in'/><arg type='as' name='propertyNames' direction='in'/>
<arg type='a(ia{sv})' name='properties' direction='out'/></method>
<method name='GetProperty'><arg type='i' name='id' direction='in'/><arg type='s' name='name' direction='in'/>
<arg type='v' name='value' direction='out'/></method>
<method name='Event'><arg type='i' name='id' direction='in'/><arg type='s' name='eventId' direction='in'/>
<arg type='v' name='data' direction='in'/><arg type='u' name='timestamp' direction='in'/></method>
<method name='EventGroup'><arg type='a(isvu)' name='events' direction='in'/><arg type='ai' name='idErrors' direction='out'/></method>
<method name='AboutToShow'><arg type='i' name='id' direction='in'/><arg type='b' name='needUpdate' direction='out'/></method>
<method name='AboutToShowGroup'><arg type='ai' name='ids' direction='in'/><arg type='ai' name='updatesNeeded' direction='out'/>
<arg type='ai' name='idErrors' direction='out'/></method>
<signal name='ItemsPropertiesUpdated'><arg type='a(ia{sv})' name='updatedProps'/><arg type='a(ias)' name='removedProps'/></signal>
<signal name='LayoutUpdated'><arg type='u' name='revision'/><arg type='i' name='parent'/></signal>
<signal name='ItemActivationRequested'><arg type='i' name='id'/><arg type='u' name='timestamp'/></signal>
</interface></node>";

pub const MENU_SHOW: i32 = 1;
pub const MENU_RECORD: i32 = 3;
pub const MENU_QUIT: i32 = 5;

/// The tray menu: (id, label or "" for a separator).
pub fn menu_items(window_shown: bool, has_window: bool, recording: bool) -> [(i32, &'static str); 5] {
    [
        (MENU_SHOW, if has_window && window_shown { "Hide BananaSplit" } else { "Show BananaSplit" }),
        (2, ""),
        (MENU_RECORD, if recording { "Stop recording" } else { "Start recording" }),
        (4, ""),
        (MENU_QUIT, "Quit BananaSplit"),
    ]
}

/// GVariant text of one menu item's properties (`a{sv}`).
pub fn item_props(label: &str) -> String {
    if label.is_empty() {
        "{'type': <'separator'>}".into()
    } else {
        format!("{{'label': <'{label}'>, 'enabled': <true>, 'visible': <true>}}")
    }
}

/// GVariant text of the `GetLayout` reply: `(u(ia{sv}av))`.
pub fn layout_text(rev: u32, items: &[(i32, &str)], parent: i32) -> String {
    if parent != 0 {
        let label = items.iter().find(|(id, _)| *id == parent).map_or("", |(_, l)| l);
        return format!("(uint32 {rev}, ({parent}, {}, @av []))", item_props(label));
    }
    let children: Vec<String> = items.iter().map(|(id, l)| format!("<({id}, {}, @av [])>", item_props(l))).collect();
    format!("(uint32 {rev}, (0, {{'children-display': <'submenu'>}}, [{}]))", children.join(", "))
}

/// GVariant text of a `GetGroupProperties` reply: `(a(ia{sv}))`.
pub fn group_text(items: &[(i32, &str)], ids: &[i32]) -> String {
    let v: Vec<String> = items
        .iter()
        .filter(|(id, _)| ids.is_empty() || ids.contains(id))
        .map(|(id, l)| format!("({id}, {})", item_props(l)))
        .collect();
    if v.is_empty() { "(@a(ia{sv}) [],)".into() } else { format!("([{}],)", v.join(", ")) }
}

const IDLE_PIX: [(i32, &[u8]); 4] = [
    (22, include_bytes!("../assets/tray/idle-22.argb")),
    (32, include_bytes!("../assets/tray/idle-32.argb")),
    (48, include_bytes!("../assets/tray/idle-48.argb")),
    (64, include_bytes!("../assets/tray/idle-64.argb")),
];
const REC_PIX: [(i32, &[u8]); 4] = [
    (22, include_bytes!("../assets/tray/rec-22.argb")),
    (32, include_bytes!("../assets/tray/rec-32.argb")),
    (48, include_bytes!("../assets/tray/rec-48.argb")),
    (64, include_bytes!("../assets/tray/rec-64.argb")),
];

fn parsed(libs: &Libs, text: &str) -> Ptr {
    let c = cstr(text);
    // SAFETY: valid GVariant text with no `%` (no varargs needed).
    unsafe { (libs.glib.g_variant_new_parsed)(c.as_ptr()) }
}

/// `a(iiay)` for the tray icon.
fn pixmaps(libs: &Libs, recording: bool) -> Ptr {
    let g = &libs.glib;
    // SAFETY: GVariant constructors with valid types and buffers; the
    // floating references are consumed by the containers.
    unsafe {
        let ty_y = (g.g_variant_type_new)(c"y".as_ptr());
        let ty_px = (g.g_variant_type_new)(c"(iiay)".as_ptr());
        let src = if recording { &REC_PIX } else { &IDLE_PIX };
        let items: Vec<Ptr> = src
            .iter()
            .map(|(s, data)| {
                let bytes = (g.g_variant_new_fixed_array)(ty_y, data.as_ptr().cast(), data.len(), 1);
                let kids = [(g.g_variant_new_int32)(*s), (g.g_variant_new_int32)(*s), bytes];
                (g.g_variant_new_tuple)(kids.as_ptr(), 3)
            })
            .collect();
        let arr = (g.g_variant_new_array)(ty_px, items.as_ptr(), items.len());
        (g.g_variant_type_free)(ty_y);
        (g.g_variant_type_free)(ty_px);
        arr
    }
}

fn emit(sig_iface: &CStr, path: &CStr, name: &CStr, params: Ptr) {
    let Some((libs, conn)) = with(|a| (a.libs, a.conn)) else { return };
    if conn.is_null() {
        return;
    }
    // SAFETY: valid connection; `params` is NULL or a floating GVariant tuple.
    unsafe {
        (libs.gio.g_dbus_connection_emit_signal)(
            conn,
            std::ptr::null(),
            path.as_ptr(),
            sig_iface.as_ptr(),
            name.as_ptr(),
            params,
            std::ptr::null_mut(),
        )
    };
}

/// The menu's labels changed (window shown/hidden, recording on/off).
fn menu_changed() {
    let Some((libs, rev)) = with(|a| {
        a.menu_rev = a.menu_rev.wrapping_add(1);
        (a.libs, a.menu_rev)
    }) else {
        return;
    };
    emit(MENU_IFACE, MENU_PATH, c"LayoutUpdated", parsed(libs, &format!("(uint32 {rev}, 0)")));
}

fn str_of(libs: &Libs, v: Ptr) -> String {
    // SAFETY: `v` holds a string-typed GVariant.
    let p = unsafe { (libs.glib.g_variant_get_string)(v, std::ptr::null_mut()) };
    if p.is_null() { String::new() } else { unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned() }
}

fn child(libs: &Libs, v: Ptr, i: usize) -> Ptr {
    // SAFETY: `i` < number of children (callers follow the introspected signature).
    unsafe { (libs.glib.g_variant_get_child_value)(v, i) }
}

fn int_child(libs: &Libs, v: Ptr, i: usize) -> i32 {
    let c = child(libs, v, i);
    // SAFETY: an `i` child; unref the new reference.
    unsafe {
        let x = (libs.glib.g_variant_get_int32)(c);
        (libs.glib.g_variant_unref)(c);
        x
    }
}

fn menu_event(id: i32) {
    match id {
        MENU_SHOW => {
            if window_visible() { hide_window() } else { show_window(None) }
        }
        MENU_RECORD => tell("record"),
        MENU_QUIT => tell("quit"),
        _ => {}
    }
}

unsafe extern "C" fn method_call(
    _conn: Ptr,
    _sender: *const c_char,
    _path: *const c_char,
    iface: *const c_char,
    method: *const c_char,
    params: Ptr,
    inv: Ptr,
    _data: Ptr,
) {
    let Some((libs, has_window, recording, rev)) = with(|a| (a.libs, a.libs.ui.is_some(), a.recording, a.menu_rev)) else {
        return;
    };
    // SAFETY: GDBus passes valid strings.
    let (iface, method) = unsafe { (CStr::from_ptr(iface), CStr::from_ptr(method)) };
    let ret = |v: Ptr| unsafe { (libs.gio.g_dbus_method_invocation_return_value)(inv, v) };
    let items = menu_items(window_visible(), has_window, recording);
    if iface == ITEM_IFACE {
        match method.to_bytes() {
            b"Activate" | b"SecondaryActivate" => toggle_window(),
            b"ContextMenu" | b"Scroll" => {}
            _ => {}
        }
        return ret(std::ptr::null_mut());
    }
    match method.to_bytes() {
        b"GetLayout" => {
            let parent = int_child(libs, params, 0);
            if parent != 0 && !items.iter().any(|(id, _)| *id == parent) {
                // SAFETY: valid invocation and strings.
                return unsafe {
                    (libs.gio.g_dbus_method_invocation_return_dbus_error)(inv, c"org.freedesktop.DBus.Error.InvalidArgs".as_ptr(), c"no such menu item".as_ptr())
                };
            }
            ret(parsed(libs, &layout_text(rev, &items, parent)))
        }
        b"GetGroupProperties" => {
            let ids_v = child(libs, params, 0);
            // SAFETY: `ai` array.
            let n = unsafe { (libs.glib.g_variant_n_children)(ids_v) };
            let ids: Vec<i32> = (0..n).map(|i| int_child(libs, ids_v, i)).collect();
            unsafe { (libs.glib.g_variant_unref)(ids_v) };
            ret(parsed(libs, &group_text(&items, &ids)))
        }
        b"GetProperty" => {
            let id = int_child(libs, params, 0);
            let label = items.iter().find(|(i, _)| *i == id).map_or("", |(_, l)| l);
            ret(parsed(libs, &format!("(<'{}'>,)", label)))
        }
        b"Event" => {
            let id = int_child(libs, params, 0);
            let ev = child(libs, params, 1);
            let what = str_of(libs, ev);
            unsafe { (libs.glib.g_variant_unref)(ev) };
            ret(std::ptr::null_mut());
            if what == "clicked" {
                menu_event(id);
            }
        }
        b"EventGroup" => {
            let evs = child(libs, params, 0);
            // SAFETY: `a(isvu)` array.
            let n = unsafe { (libs.glib.g_variant_n_children)(evs) };
            let mut clicked = Vec::new();
            for i in 0..n {
                let e = child(libs, evs, i);
                let id = int_child(libs, e, 0);
                let ev = child(libs, e, 1);
                if str_of(libs, ev) == "clicked" {
                    clicked.push(id);
                }
                unsafe {
                    (libs.glib.g_variant_unref)(ev);
                    (libs.glib.g_variant_unref)(e);
                }
            }
            unsafe { (libs.glib.g_variant_unref)(evs) };
            ret(parsed(libs, "(@ai [],)"));
            for id in clicked {
                menu_event(id);
            }
        }
        b"AboutToShow" => ret(parsed(libs, "(false,)")),
        b"AboutToShowGroup" => ret(parsed(libs, "(@ai [], @ai [])")),
        _ => unsafe {
            (libs.gio.g_dbus_method_invocation_return_dbus_error)(inv, c"org.freedesktop.DBus.Error.UnknownMethod".as_ptr(), c"unknown method".as_ptr())
        },
    }
}

unsafe extern "C" fn get_property(
    _conn: Ptr,
    _sender: *const c_char,
    _path: *const c_char,
    iface: *const c_char,
    prop: *const c_char,
    _err: *mut Ptr,
    _data: Ptr,
) -> Ptr {
    let Some((libs, recording)) = with(|a| (a.libs, a.recording)) else { return std::ptr::null_mut() };
    // SAFETY: GDBus passes valid strings.
    let (iface, prop) = unsafe { (CStr::from_ptr(iface), CStr::from_ptr(prop)) };
    let p = |t: &str| parsed(libs, t);
    if iface == MENU_IFACE {
        return match prop.to_bytes() {
            b"Version" => p("uint32 3"),
            b"TextDirection" => p("'ltr'"),
            b"Status" => p("'normal'"),
            b"IconThemePath" => p("@as []"),
            _ => std::ptr::null_mut(),
        };
    }
    let tip = if recording { "Recording" } else { "Mixer running" };
    match prop.to_bytes() {
        b"Category" => p("'ApplicationStatus'"),
        b"Id" => p("'bananasplit'"),
        b"Title" => p("'BananaSplit'"),
        b"Status" => p("'Active'"),
        b"WindowId" => p("0"),
        // An empty name makes every host use the pixmap (which shows the
        // recording dot), whether or not the icon theme has our icon.
        b"IconName" | b"OverlayIconName" | b"AttentionIconName" | b"AttentionMovieName" | b"IconThemePath" => p("''"),
        b"IconPixmap" => pixmaps(libs, recording),
        b"OverlayIconPixmap" | b"AttentionIconPixmap" => p("@a(iiay) []"),
        b"ToolTip" => p(&format!("('', @a(iiay) [], 'BananaSplit', '{tip}')")),
        b"ItemIsMenu" => p("false"),
        b"Menu" => p("objectpath '/MenuBar'"),
        _ => std::ptr::null_mut(),
    }
}

static VTABLE: VTable = VTable { method_call, get_property, set_property: std::ptr::null_mut(), padding: [std::ptr::null_mut(); 8] };

fn bus_name() -> String {
    format!("org.kde.StatusNotifierItem-{}-1", std::process::id())
}

unsafe extern "C" fn registered(conn: Ptr, res: Ptr, _data: Ptr) {
    let Some(libs) = with(|a| a.libs) else { return };
    let mut err: Ptr = std::ptr::null_mut();
    // SAFETY: the result of our g_dbus_connection_call.
    let v = unsafe { (libs.gio.g_dbus_connection_call_finish)(conn, res, &mut err) };
    let ok = !v.is_null();
    if ok {
        unsafe { (libs.glib.g_variant_unref)(v) };
    } else {
        eprintln!("bananasplit: the tray did not take the icon: {}", gerror_text(err));
    }
    with(|a| a.tray_registered = ok);
}

fn register_with_watcher() {
    let Some((libs, conn)) = with(|a| (a.libs, a.conn)) else { return };
    let name = bus_name();
    let params = parsed(libs, &format!("('{name}',)"));
    // SAFETY: valid connection and strings; the callback runs on this thread.
    unsafe {
        (libs.gio.g_dbus_connection_call)(
            conn,
            WATCHER.as_ptr(),
            c"/StatusNotifierWatcher".as_ptr(),
            WATCHER.as_ptr(),
            c"RegisterStatusNotifierItem".as_ptr(),
            params,
            std::ptr::null_mut(),
            0,
            3000,
            std::ptr::null_mut(),
            registered as *const () as Ptr,
            std::ptr::null_mut(),
        )
    };
}

unsafe extern "C" fn watcher_appeared(_c: Ptr, _name: *const c_char, _owner: *const c_char, _d: Ptr) {
    with(|a| a.watcher_present = true);
    register_with_watcher();
}

unsafe extern "C" fn watcher_vanished(_c: Ptr, _name: *const c_char, _d: Ptr) {
    with(|a| {
        a.watcher_present = false;
        a.tray_registered = false;
    });
}

unsafe extern "C" fn name_lost(_c: Ptr, _name: *const c_char, _d: Ptr) {}
unsafe extern "C" fn name_acquired(_c: Ptr, _name: *const c_char, _d: Ptr) {}

/// Is there a tray that shows our icon right now?
fn tray_available() -> bool {
    let Some((libs, conn, registered, present)) = with(|a| (a.libs, a.conn, a.tray_registered, a.watcher_present)) else {
        return false;
    };
    if conn.is_null() || !registered || !present {
        return false;
    }
    // A watcher without a host (a panel that shows the items) is no tray.
    let params = parsed(libs, "('org.kde.StatusNotifierWatcher', 'IsStatusNotifierHostRegistered')");
    let mut err: Ptr = std::ptr::null_mut();
    // SAFETY: valid connection and strings.
    let v = unsafe {
        (libs.gio.g_dbus_connection_call_sync)(
            conn,
            WATCHER.as_ptr(),
            c"/StatusNotifierWatcher".as_ptr(),
            c"org.freedesktop.DBus.Properties".as_ptr(),
            c"Get".as_ptr(),
            params,
            std::ptr::null_mut(),
            0,
            1000,
            std::ptr::null_mut(),
            &mut err,
        )
    };
    if v.is_null() {
        return false;
    }
    // (v) holding b
    let inner = child(libs, v, 0);
    // SAFETY: `inner` is the `v`; its content should be a boolean.
    let b = unsafe { (libs.glib.g_variant_get_variant)(inner) };
    let tyok = !b.is_null();
    let on = tyok && unsafe { (libs.glib.g_variant_get_boolean)(b) } != 0;
    unsafe {
        if tyok {
            (libs.glib.g_variant_unref)(b);
        }
        (libs.glib.g_variant_unref)(inner);
        (libs.glib.g_variant_unref)(v);
    }
    on
}

/// The message of a GError (`struct { GQuark domain; gint code; gchar *message; }`).
fn gerror_text(e: Ptr) -> String {
    #[repr(C)]
    struct GError {
        domain: u32,
        code: c_int,
        message: *const c_char,
    }
    if e.is_null() {
        return "unknown error".into();
    }
    // SAFETY: GLib hands out GError structs of exactly this layout.
    let m = unsafe { (*(e as *const GError)).message };
    if m.is_null() { "unknown error".into() } else { unsafe { CStr::from_ptr(m) }.to_string_lossy().into_owned() }
}

fn setup_tray(libs: &'static Libs) -> Ptr {
    let g = &libs.gio;
    let mut err: Ptr = std::ptr::null_mut();
    let warn = |what: &str, e: Ptr| eprintln!("bananasplit: no tray icon: {what}: {}", gerror_text(e));
    // SAFETY: GIO calls with valid arguments; the introspection data and
    // the vtable live for the whole process.
    unsafe {
        let conn = (g.g_bus_get_sync)(2 /* G_BUS_TYPE_SESSION */, std::ptr::null_mut(), &mut err);
        if conn.is_null() {
            warn("no D-Bus session", err);
            return conn;
        }
        for (xml, path, iface) in [(ITEM_XML, ITEM_PATH, ITEM_IFACE), (MENU_XML, MENU_PATH, MENU_IFACE)] {
            let mut e: Ptr = std::ptr::null_mut();
            let node = (g.g_dbus_node_info_new_for_xml)(xml.as_ptr(), &mut e);
            if node.is_null() {
                warn("introspection data", e);
                return std::ptr::null_mut();
            }
            let info = (g.g_dbus_node_info_lookup_interface)(node, iface.as_ptr());
            let mut e: Ptr = std::ptr::null_mut();
            if (g.g_dbus_connection_register_object)(conn, path.as_ptr(), info, &VTABLE, std::ptr::null_mut(), std::ptr::null_mut(), &mut e) == 0 {
                warn("cannot export the icon", e);
                return std::ptr::null_mut();
            }
        }
        let name = cstr(&bus_name());
        (g.g_bus_own_name_on_connection)(
            conn,
            name.as_ptr(),
            0,
            name_acquired as *const () as Ptr,
            name_lost as *const () as Ptr,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        );
        (g.g_bus_watch_name_on_connection)(
            conn,
            WATCHER.as_ptr(),
            0,
            watcher_appeared as *const () as Ptr,
            watcher_vanished as *const () as Ptr,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        );
        conn
    }
}

// ---------------------------------------------------------------------------
// The desktop's dark style (GNOME 42 and later keep it in the settings
// portal, which GTK 3 and so WebKitGTK do not read): passed on as GTK's
// "prefer dark theme", which WebKitGTK turns into prefers-color-scheme for
// the page's Auto theme.
// ---------------------------------------------------------------------------

const PORTAL: &CStr = c"org.freedesktop.portal.Desktop";
const PORTAL_PATH: &CStr = c"/org/freedesktop/portal/desktop";
const PORTAL_SETTINGS: &CStr = c"org.freedesktop.portal.Settings";

thread_local! {
    /// We set "prefer dark" (and so may take it back).
    static DARK_SET: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// The color-scheme value in a reply or signal: a `u`, possibly boxed in
/// one or two `v`s. `v` stays the caller's.
fn scheme_of(libs: &Libs, v: Ptr) -> Option<u32> {
    let g = &libs.glib;
    if v.is_null() {
        return None;
    }
    // SAFETY: valid GVariants; every box taken out here is unreferenced here.
    unsafe {
        let ty = |x: Ptr| CStr::from_ptr((g.g_variant_get_type_string)(x)).to_bytes().to_vec();
        match ty(v).as_slice() {
            b"u" => return Some((g.g_variant_get_uint32)(v)),
            b"v" => {}
            _ => return None,
        }
        let mut cur = (g.g_variant_get_variant)(v);
        for _ in 0..3 {
            if cur.is_null() {
                return None;
            }
            match ty(cur).as_slice() {
                b"v" => {
                    let inner = (g.g_variant_get_variant)(cur);
                    (g.g_variant_unref)(cur);
                    cur = inner;
                }
                b"u" => {
                    let s = (g.g_variant_get_uint32)(cur);
                    (g.g_variant_unref)(cur);
                    return Some(s);
                }
                _ => break,
            }
        }
        if !cur.is_null() {
            (g.g_variant_unref)(cur);
        }
        None
    }
}

/// 1 = prefer dark, 2 = prefer light, 0 = no preference.
fn apply_scheme(libs: &Libs, scheme: u32) {
    let Some((gtk, _, _)) = &libs.ui else { return };
    let dark = scheme == 1;
    if !dark && !DARK_SET.get() {
        return; // leave the user's own GTK setting alone
    }
    // SAFETY: main thread, GTK initialised; a gboolean property set
    // through the NULL-terminated varargs list.
    unsafe {
        let settings = (gtk.gtk_settings_get_default)();
        if !settings.is_null() {
            (libs.gobject.g_object_set)(
                settings,
                c"gtk-application-prefer-dark-theme".as_ptr(),
                c_int::from(dark),
                std::ptr::null::<c_char>(),
            );
            DARK_SET.set(dark);
        }
    }
}

unsafe extern "C" fn scheme_read(conn: Ptr, res: Ptr, _d: Ptr) {
    let Some(libs) = with(|a| a.libs) else { return };
    let mut err: Ptr = std::ptr::null_mut();
    // SAFETY: the result of our g_dbus_connection_call; a (v) reply.
    unsafe {
        let r = (libs.gio.g_dbus_connection_call_finish)(conn, res, &mut err);
        if r.is_null() {
            return; // no portal: GTK's own setting stays
        }
        let v = (libs.glib.g_variant_get_child_value)(r, 0);
        if let Some(s) = scheme_of(libs, v) {
            apply_scheme(libs, s);
        }
        (libs.glib.g_variant_unref)(v);
        (libs.glib.g_variant_unref)(r);
    }
}

unsafe extern "C" fn scheme_changed(
    _c: Ptr,
    _sender: *const c_char,
    _path: *const c_char,
    _iface: *const c_char,
    _signal: *const c_char,
    params: Ptr,
    _d: Ptr,
) {
    let Some(libs) = with(|a| a.libs) else { return };
    let g = &libs.glib;
    // SAFETY: `params` is the signal's tuple, owned by GIO; anything that is
    // not (ssv) (a bad sender) is ignored before any child is read.
    unsafe {
        if params.is_null() || CStr::from_ptr((g.g_variant_get_type_string)(params)) != c"(ssv)" {
            return;
        }
        let key = (g.g_variant_get_child_value)(params, 1);
        let is_scheme = !key.is_null() && CStr::from_ptr((g.g_variant_get_string)(key, std::ptr::null_mut())) == c"color-scheme";
        if !key.is_null() {
            (g.g_variant_unref)(key);
        }
        if !is_scheme {
            return;
        }
        let v = (g.g_variant_get_child_value)(params, 2);
        if let Some(s) = scheme_of(libs, v) {
            apply_scheme(libs, s);
        }
        if !v.is_null() {
            (g.g_variant_unref)(v);
        }
    }
}

fn follow_dark_style(libs: &'static Libs, conn: Ptr) {
    if conn.is_null() || libs.ui.is_none() {
        return;
    }
    let gio = &libs.gio;
    let params = parsed(libs, "('org.freedesktop.appearance', 'color-scheme')");
    // SAFETY: valid connection and NUL-terminated strings; the callbacks
    // run on this thread from the main loop.
    unsafe {
        (gio.g_dbus_connection_signal_subscribe)(
            conn,
            PORTAL.as_ptr(),
            PORTAL_SETTINGS.as_ptr(),
            c"SettingChanged".as_ptr(),
            PORTAL_PATH.as_ptr(),
            c"org.freedesktop.appearance".as_ptr(),
            0,
            scheme_changed as *const () as Ptr,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        );
        (gio.g_dbus_connection_call)(
            conn,
            PORTAL.as_ptr(),
            PORTAL_PATH.as_ptr(),
            PORTAL_SETTINGS.as_ptr(),
            c"Read".as_ptr(),
            params,
            std::ptr::null_mut(),
            0,
            3000,
            std::ptr::null_mut(),
            scheme_read as *const () as Ptr,
            std::ptr::null_mut(),
        );
    }
}

fn set_recording(on: bool) {
    let Some(changed) = with(|a| std::mem::replace(&mut a.recording, on) != on) else { return };
    if changed {
        emit(ITEM_IFACE, ITEM_PATH, c"NewIcon", std::ptr::null_mut());
        emit(ITEM_IFACE, ITEM_PATH, c"NewToolTip", std::ptr::null_mut());
        menu_changed();
    }
}

// ---------------------------------------------------------------------------
// Window callbacks
// ---------------------------------------------------------------------------

unsafe extern "C" fn on_delete(_w: Ptr, _ev: Ptr, _d: Ptr) -> c_int {
    let Some((libs, win, close_to_tray)) = with(|a| (a.libs, a.window, a.close_to_tray)) else { return 1 };
    if !close_to_tray {
        hide_window();
        tell("quit");
    } else if tray_available() {
        hide_window();
    } else if let Some((gtk, _, _)) = &libs.ui {
        // No tray to come back from: keep it in the taskbar.
        // SAFETY: valid window.
        unsafe { (gtk.gtk_window_iconify)(win) };
    }
    1 // TRUE: never destroy the window
}

unsafe extern "C" fn on_context_menu(_v: Ptr, _menu: Ptr, _ev: Ptr, hit: Ptr, _d: Ptr) -> c_int {
    // Keep Cut/Copy/Paste in text fields; no Back/Reload/Inspect elsewhere.
    let Some(libs) = with(|a| a.libs) else { return 1 };
    let Some((_, _, wk)) = &libs.ui else { return 1 };
    // SAFETY: WebKit passes a valid hit-test result.
    let editable = !hit.is_null() && unsafe { (wk.webkit_hit_test_result_context_is_editable)(hit) } != 0;
    c_int::from(!editable)
}

unsafe extern "C" fn reload_later(_d: Ptr) -> c_int {
    if let Some((libs, view)) = with(|a| (a.libs, a.view)) {
        if let (Some((_, _, wk)), false) = (&libs.ui, view.is_null()) {
            // SAFETY: valid view.
            unsafe { (wk.webkit_web_view_reload)(view) };
        }
    }
    0 // G_SOURCE_REMOVE
}

unsafe extern "C" fn on_crashed(_v: Ptr, _reason: c_int, _d: Ptr) {
    if let Some(libs) = with(|a| a.libs) {
        // SAFETY: valid callback; runs once.
        unsafe { (libs.glib.g_timeout_add)(500, reload_later, std::ptr::null_mut()) };
    }
}

unsafe extern "C" fn on_load_failed(_v: Ptr, _ev: c_int, _uri: *const c_char, _err: Ptr, _d: Ptr) -> c_int {
    if let Some(libs) = with(|a| a.libs) {
        // SAFETY: valid callback; runs once.
        unsafe { (libs.glib.g_timeout_add)(1000, reload_later, std::ptr::null_mut()) };
    }
    1 // no WebKit error page: the mixer's page reconnects by itself
}

/// `--hidden`: stay in the tray if there is one; else start minimized.
unsafe extern "C" fn hidden_check(_d: Ptr) -> c_int {
    if !tray_available() {
        let Some((libs, win)) = with(|a| (a.libs, a.window)) else { return 0 };
        if let Some((gtk, _, _)) = &libs.ui {
            // SAFETY: valid window.
            unsafe {
                (gtk.gtk_window_iconify)(win);
                (gtk.gtk_widget_show_all)(win);
            }
        } else {
            tell("open");
        }
    }
    0
}

fn handle_line(line: &str) {
    let (cmd, arg) = line.split_once(' ').unwrap_or((line, ""));
    match cmd {
        "show" => show_window(Some(arg)),
        "hide" => hide_window(),
        "rec" => set_recording(arg == "1"),
        "close-to-tray" => {
            with(|a| a.close_to_tray = arg != "0");
        }
        "quit" => quit(),
        _ => {}
    }
}

unsafe extern "C" fn on_stdin(fd: c_int, cond: c_uint, _d: Ptr) -> c_int {
    const G_IO_IN: c_uint = 1;
    let mut buf = [0u8; 4096];
    let n = if cond & G_IO_IN != 0 {
        // SAFETY: valid buffer.
        unsafe { read(fd, buf.as_mut_ptr().cast(), buf.len()) }
    } else {
        0
    };
    if n <= 0 {
        // The mixer is gone (or closed our input): so are we.
        quit();
        return 0;
    }
    let lines = with(|a| {
        a.stdin_buf.extend_from_slice(&buf[..n as usize]);
        let mut out = Vec::new();
        while let Some(p) = a.stdin_buf.iter().position(|&b| b == b'\n') {
            let l: Vec<u8> = a.stdin_buf.drain(..=p).collect();
            out.push(String::from_utf8_lossy(&l[..l.len() - 1]).trim().to_string());
        }
        out
    })
    .unwrap_or_default();
    for l in lines {
        handle_line(&l);
    }
    1
}

// ---------------------------------------------------------------------------
// Entry points
// ---------------------------------------------------------------------------

pub struct Opts {
    pub url: String,
    pub hidden: bool,
    pub close_to_tray: bool,
    pub size: Option<(i32, i32)>,
    pub token: Option<String>,
}

/// Parse `--window URL [--hidden] [--close-to-tray 0|1] [--size WxH] [--token T]`.
pub fn parse_args(args: &[String]) -> Result<Opts, String> {
    let mut it = args.iter();
    let url = it.next().ok_or("--window needs a URL")?.clone();
    if !url.starts_with("http://127.0.0.1:") {
        return Err("--window only opens BananaSplit's own page".into());
    }
    let mut o = Opts { url, hidden: false, close_to_tray: true, size: None, token: None };
    while let Some(a) = it.next() {
        match a.as_str() {
            "--hidden" => o.hidden = true,
            "--close-to-tray" => o.close_to_tray = it.next().map(String::as_str) != Some("0"),
            "--size" => {
                let v = it.next().ok_or("--size needs WxH")?;
                let (w, h) = v.split_once('x').ok_or("--size needs WxH")?;
                let (w, h) = (w.parse::<i32>().map_err(|e| e.to_string())?, h.parse::<i32>().map_err(|e| e.to_string())?);
                o.size = Some((w.clamp(640, 8192), h.clamp(480, 8192)));
            }
            "--token" => o.token = it.next().cloned(),
            x => return Err(format!("unknown argument {x:?}")),
        }
    }
    Ok(o)
}

/// Default window size: the design's size, within the screen's work area.
pub fn fit_size(want: Option<(i32, i32)>, work: Option<(i32, i32)>) -> (i32, i32) {
    let (w, h) = want.unwrap_or((1780, 1000));
    match work {
        Some((ww, wh)) if ww > 0 && wh > 0 => (w.min(ww - 32).max(640), h.min(wh - 32).max(480)),
        _ => (w, h),
    }
}

fn icon_file() -> Option<std::path::PathBuf> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .filter(|v| !v.is_empty())
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".cache")))?;
    let p = base.join("bananasplit").join("icon-256.png");
    let png = crate::desktop::ICON_PNGS[6].1;
    if std::fs::read(&p).ok().as_deref() != Some(png) {
        std::fs::create_dir_all(p.parent()?).ok()?;
        std::fs::write(&p, png).ok()?;
    }
    Some(p)
}

/// The window process. Returns when the mixer goes away or says `quit`.
pub fn main(args: &[String]) -> ExitCode {
    crate::sys::close_inherited_fds();
    let opts = match parse_args(args) {
        Ok(o) => o,
        Err(e) => {
            tell(&format!("fail {e}"));
            return ExitCode::from(2);
        }
    };
    // WebKitGTK's DMA-BUF renderer shows an empty window on some drivers
    // (NVIDIA); the shared-memory path always works and is plenty here.
    if std::env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER").is_none() {
        // SAFETY: still single-threaded (no library loaded yet).
        unsafe { std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1") };
    }
    let (glib, gobject, gio) = match load_glib() {
        Ok(l) => l,
        Err(e) => {
            tell(&format!("fail GLib: {e}"));
            return ExitCode::from(2);
        }
    };
    let (ui, no_ui) = match load_gtk().and_then(|(g, d)| load_webkit().map(|w| (g, d, w))) {
        Ok(u) => (Some(u), String::new()),
        Err(e) => (None, e),
    };
    let libs: &'static Libs = Box::leak(Box::new(Libs { glib, gobject, gio, ui, no_ui }));
    let g = &libs.glib;
    // SAFETY: GLib/GTK calls on the main thread with valid arguments.
    unsafe {
        (g.g_set_prgname)(c"bananasplit".as_ptr());
        (g.g_set_application_name)(c"BananaSplit".as_ptr());
    }
    let mut window: Ptr = std::ptr::null_mut();
    let mut view: Ptr = std::ptr::null_mut();
    let mut ui_ok = false;
    if let Some((gtk, gdk, wk)) = &libs.ui {
        unsafe {
            ui_ok = (gtk.gtk_init_check)(std::ptr::null_mut(), std::ptr::null_mut()) != 0;
            // After gtk_init, which sets the class from the program name
            // ("Bananasplit"): WM_CLASS must match the desktop file's
            // StartupWMClass for the dock to group the window with its icon.
            (gdk.gdk_set_program_class)(c"bananasplit".as_ptr());
        }
        if ui_ok {
            unsafe {
                let display = (gdk.gdk_display_get_default)();
                let mut work = None;
                if !display.is_null() {
                    let mut mon = (gdk.gdk_display_get_primary_monitor)(display);
                    if mon.is_null() {
                        mon = (gdk.gdk_display_get_monitor)(display, 0);
                    }
                    if !mon.is_null() {
                        let mut r = GdkRectangle::default();
                        (gdk.gdk_monitor_get_workarea)(mon, &mut r);
                        work = Some((r.width, r.height));
                    }
                }
                let (w, h) = fit_size(opts.size, work);
                if let Some(icon) = icon_file().and_then(|p| CString::new(p.to_string_lossy().as_bytes()).ok()) {
                    (gtk.gtk_window_set_default_icon_from_file)(icon.as_ptr(), std::ptr::null_mut());
                }
                window = (gtk.gtk_window_new)(0);
                (gtk.gtk_window_set_title)(window, c"BananaSplit".as_ptr());
                (gtk.gtk_window_set_icon_name)(window, c"bananasplit".as_ptr());
                (gtk.gtk_window_set_default_size)(window, w, h);
                (gtk.gtk_widget_set_size_request)(window, 640, 480);
                view = (wk.webkit_web_view_new)();
                let bg = GdkRgba { red: 11.0 / 255.0, green: 16.0 / 255.0, blue: 21.0 / 255.0, alpha: 1.0 };
                (wk.webkit_web_view_set_background_color)(view, &bg);
                (gtk.gtk_container_add)(window, view);
                let con = libs.gobject.g_signal_connect_data;
                let nul = std::ptr::null_mut();
                con(window, c"delete-event".as_ptr(), on_delete as *const () as Ptr, nul, nul, 0);
                con(view, c"context-menu".as_ptr(), on_context_menu as *const () as Ptr, nul, nul, 0);
                con(view, c"web-process-terminated".as_ptr(), on_crashed as *const () as Ptr, nul, nul, 0);
                con(view, c"load-failed".as_ptr(), on_load_failed as *const () as Ptr, nul, nul, 0);
                let url = cstr(&opts.url);
                (wk.webkit_web_view_load_uri)(view, url.as_ptr());
            }
        }
    }
    // SAFETY: as above.
    let main_loop = unsafe { (g.g_main_loop_new)(std::ptr::null_mut(), 0) };
    APP.with(|a| {
        *a.borrow_mut() = Some(App {
            libs,
            main_loop,
            window,
            view,
            conn: std::ptr::null_mut(),
            close_to_tray: opts.close_to_tray,
            recording: false,
            tray_registered: false,
            watcher_present: false,
            menu_rev: 1,
            stdin_buf: Vec::new(),
        })
    });
    let conn = setup_tray(libs);
    with(|a| a.conn = conn);
    if ui_ok {
        follow_dark_style(libs, conn);
    }
    // SAFETY: fd 0 is the pipe from the mixer.
    unsafe { (g.g_unix_fd_add)(0, 1 | 8 | 16 /* IN | ERR | HUP */, on_stdin, std::ptr::null_mut()) };

    if ui_ok {
        tell("ready app");
        if opts.hidden {
            // Give the tray a moment to pick up our icon.
            unsafe { (g.g_timeout_add)(2500, hidden_check, std::ptr::null_mut()) };
        } else {
            show_window(opts.token.as_deref());
        }
    } else {
        let why = if libs.ui.is_some() { "no display".to_string() } else { libs.no_ui.clone() };
        if conn.is_null() {
            tell(&format!("fail {why}; no D-Bus session for a tray icon either"));
            return ExitCode::from(3);
        }
        tell(&format!("ready tray {why}"));
        if opts.hidden {
            unsafe { (g.g_timeout_add)(2500, hidden_check, std::ptr::null_mut()) };
        }
    }
    // SAFETY: the loop runs until quit().
    unsafe { (g.g_main_loop_run)(main_loop) };
    ExitCode::SUCCESS
}

/// Show an error to someone who started BananaSplit from the app menu (no
/// terminal to print to): a GTK dialog, else zenity / kdialog / notify-send.
pub fn error_dialog(text: &str) {
    if let Ok((glib, _, _)) = load_glib() {
        if let Ok((gtk, _)) = load_gtk() {
            // SAFETY: GTK calls on this thread with valid arguments; the
            // message goes through "%s", never as a format string itself.
            unsafe {
                (glib.g_set_prgname)(c"bananasplit".as_ptr());
                if (gtk.gtk_init_check)(std::ptr::null_mut(), std::ptr::null_mut()) != 0 {
                    let msg = cstr(text);
                    let d = (gtk.gtk_message_dialog_new)(std::ptr::null_mut(), 0, 3 /* ERROR */, 2 /* CLOSE */, c"%s".as_ptr(), msg.as_ptr());
                    if !d.is_null() {
                        (gtk.gtk_window_set_title)(d, c"BananaSplit".as_ptr());
                        (gtk.gtk_dialog_run)(d);
                        (gtk.gtk_widget_destroy)(d);
                        return;
                    }
                }
            }
        }
    }
    let plain: String = text.chars().map(|c| match c { '<' => '‹', '>' => '›', '&' => '+', c => c }).collect();
    let tries: [(&str, Vec<&str>); 3] = [
        ("zenity", vec!["--error", "--title=BananaSplit", "--text", &plain]),
        ("kdialog", vec!["--title", "BananaSplit", "--error", text]),
        ("notify-send", vec!["--app-name=BananaSplit", "BananaSplit could not start", text]),
    ];
    for (cmd, args) in tries {
        if std::process::Command::new(cmd).args(&args).stdin(std::process::Stdio::null()).status().is_ok() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn menu_layouts() {
        let items = menu_items(false, true, false);
        assert_eq!(items[0], (MENU_SHOW, "Show BananaSplit"));
        assert_eq!(menu_items(true, true, true)[0].1, "Hide BananaSplit");
        assert_eq!(menu_items(true, false, true)[0].1, "Show BananaSplit", "a browser window cannot be hidden from here");
        assert_eq!(menu_items(false, true, true)[2], (MENU_RECORD, "Stop recording"));
        let t = layout_text(7, &items, 0);
        assert_eq!(
            t,
            "(uint32 7, (0, {'children-display': <'submenu'>}, [<(1, {'label': <'Show BananaSplit'>, 'enabled': <true>, 'visible': <true>}, @av [])>, \
             <(2, {'type': <'separator'>}, @av [])>, <(3, {'label': <'Start recording'>, 'enabled': <true>, 'visible': <true>}, @av [])>, \
             <(4, {'type': <'separator'>}, @av [])>, <(5, {'label': <'Quit BananaSplit'>, 'enabled': <true>, 'visible': <true>}, @av [])>]))"
        );
        assert_eq!(layout_text(2, &items, 5), "(uint32 2, (5, {'label': <'Quit BananaSplit'>, 'enabled': <true>, 'visible': <true>}, @av []))");
        assert_eq!(group_text(&items, &[2]), "([(2, {'type': <'separator'>})],)");
        assert!(group_text(&items, &[]).starts_with("([(1, "));
        assert_eq!(group_text(&items, &[99]), "(@a(ia{sv}) [],)");
        for (_, l) in items {
            assert!(!l.contains('%') && !l.contains('\''), "GVariant text needs no escaping");
        }
    }

    #[test]
    fn window_args_and_size() {
        let a = |v: &[&str]| parse_args(&v.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        let o = a(&["http://127.0.0.1:8790/", "--hidden", "--close-to-tray", "0", "--size", "1200x700", "--token", "abc"]).unwrap();
        assert!(o.hidden && !o.close_to_tray && o.size == Some((1200, 700)) && o.token.as_deref() == Some("abc"));
        assert!(a(&["https://evil.example/"]).is_err());
        assert!(a(&[]).is_err());
        assert!(a(&["http://127.0.0.1:1/", "--size", "big"]).is_err());
        assert_eq!(a(&["http://127.0.0.1:1/", "--size", "10x10"]).unwrap().size, Some((640, 480)));
        assert_eq!(fit_size(None, Some((1366, 728))), (1334, 696));
        assert_eq!(fit_size(None, Some((2560, 1400))), (1780, 1000));
        assert_eq!(fit_size(Some((1500, 900)), None), (1500, 900));
    }

    #[test]
    fn tray_pixmaps_are_argb_squares() {
        for (s, d) in IDLE_PIX.iter().chain(REC_PIX.iter()) {
            assert_eq!(d.len(), (s * s * 4) as usize);
        }
        // the corner is transparent, the middle is opaque
        let (s, d) = IDLE_PIX[1];
        assert_eq!(d[0], 0);
        let mid = ((s / 2 * s + s / 2) * 4) as usize;
        assert_eq!(d[mid], 255);
    }
}
