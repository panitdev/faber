//! A demo configuration UI against the `faber_ui` ABI: a switch, an input and
//! a Save button, backed by `config_get` / `config_set`.

use std::cell::RefCell;

#[link(wasm_import_module = "faber_ui")]
extern "C" {
    fn ui_create(kind_ptr: *const u8, kind_len: usize) -> u32;
    fn ui_set(w: u32, key_ptr: *const u8, key_len: usize, val_ptr: *const u8, val_len: usize) -> i32;
    fn ui_add(parent: u32, child: u32, cell_ptr: *const u8, cell_len: usize);
    fn ui_on(w: u32, ev_ptr: *const u8, ev_len: usize) -> u32;
    fn rpc_call(fn_ptr: *const u8, fn_len: usize, args_ptr: *const u8, args_len: usize) -> u32;
    fn log(level: u32, ptr: *const u8, len: usize);
}

fn create(kind: &str) -> u32 {
    unsafe { ui_create(kind.as_ptr(), kind.len()) }
}
fn set(w: u32, key: &str, json: &str) -> i32 {
    unsafe { ui_set(w, key.as_ptr(), key.len(), json.as_ptr(), json.len()) }
}
fn add(parent: u32, child: u32) {
    unsafe { ui_add(parent, child, std::ptr::null(), 0) }
}
fn on(w: u32, event: &str) -> u32 {
    unsafe { ui_on(w, event.as_ptr(), event.len()) }
}
fn call(name: &str, args: &str) -> u32 {
    unsafe { rpc_call(name.as_ptr(), name.len(), args.as_ptr(), args.len()) }
}
fn info(text: &str) {
    unsafe { log(2, text.as_ptr(), text.len()) }
}

fn quote(text: &str) -> String {
    let mut out = String::from("\"");
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The text after `"key":` up to the next `,` or `}` — enough for this demo's flat payloads.
fn field<'a>(json: &'a str, key: &str) -> Option<&'a str> {
    let needle = format!("\"{key}\":");
    let start = json.find(&needle)? + needle.len();
    let rest = &json[start..];
    let end = if rest.starts_with('"') {
        rest[1..].find('"').map(|i| i + 2)?
    } else {
        rest.find([',', '}']).unwrap_or(rest.len())
    };
    Some(rest[..end].trim())
}

#[derive(Default)]
struct State {
    bind: u32,
    label: u32,
    save: u32,
    status: u32,
    bind_listener: u32,
    label_listener: u32,
    save_listener: u32,
    hang_listener: u32,
    bind_value: bool,
    label_value: String,
    get_call: u32,
    set_call: u32,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
}

unsafe fn bytes<'a>(ptr: *const u8, len: usize) -> &'a str {
    if len == 0 {
        return "";
    }
    std::str::from_utf8(std::slice::from_raw_parts(ptr, len)).unwrap_or("")
}

#[no_mangle]
pub extern "C" fn alloc(len: usize) -> *mut u8 {
    let mut buf = Vec::<u8>::with_capacity(len);
    let ptr = buf.as_mut_ptr();
    std::mem::forget(buf);
    ptr
}

#[no_mangle]
pub unsafe extern "C" fn free(ptr: *mut u8, len: usize) {
    drop(Vec::from_raw_parts(ptr, 0, len));
}

#[no_mangle]
pub unsafe extern "C" fn configure(ptr: *const u8, len: usize) {
    let context = bytes(ptr, len);
    let root: u32 = field(context, "root").and_then(|v| v.parse().ok()).unwrap_or(1);
    let project = field(context, "project").unwrap_or("?");
    info(&format!("configure for project {project}"));

    let page = create("box");
    set(page, "style.gap", "3");
    add(root, page);

    let title = create("text");
    set(title, "text", "\"Demo settings\"");
    set(title, "style.size", "\"lg\"");
    set(title, "style.weight", "\"semibold\"");
    add(page, title);

    let bind = create("switch");
    set(bind, "label", "\"Bind by default\"");
    add(page, bind);

    let label = create("input");
    set(label, "label", "\"Label\"");
    set(label, "placeholder", "\"devbox\"");
    add(page, label);

    let row = create("box");
    set(row, "style.direction", "\"row\"");
    set(row, "style.gap", "2");
    set(row, "style.align", "\"center\"");
    add(page, row);

    let save = create("button");
    set(save, "text", "\"Save\"");
    add(row, save);

    let hang = create("button");
    set(hang, "text", "\"Hang\"");
    set(hang, "variant", "\"ghost\"");
    add(row, hang);

    let status = create("label");
    set(status, "muted", "true");
    set(status, "text", "\"Loading…\"");
    add(row, status);

    // Rejected on purpose: shows the error path without ending the plugin.
    if set(status, "style.position", "\"absolute\"") != -2 {
        info("expected -2 for an unknown key");
    }

    STATE.with(|state| {
        let mut s = state.borrow_mut();
        s.bind = bind;
        s.label = label;
        s.save = save;
        s.status = status;
        s.bind_listener = on(bind, "change");
        s.label_listener = on(label, "change");
        s.save_listener = on(save, "click");
        s.hang_listener = on(hang, "click");
        s.get_call = call("config_get", "{}");
    });
}

#[no_mangle]
pub unsafe extern "C" fn on_event(listener: u32, ptr: *const u8, len: usize) {
    let payload = bytes(ptr, len);
    STATE.with(|state| {
        let mut s = state.borrow_mut();
        if listener == s.bind_listener {
            s.bind_value = field(payload, "value") == Some("true");
        } else if listener == s.label_listener {
            s.label_value = field(payload, "value")
                .map(|v| v.trim_matches('"').to_owned())
                .unwrap_or_default();
        } else if listener == s.hang_listener {
            // Never returns: the host's watchdog has to end it.
            loop {
                std::hint::spin_loop();
            }
        } else if listener == s.save_listener {
            set(s.save, "loading", "true");
            set(s.status, "text", "\"Saving…\"");
            let args = format!(
                "{{\"bind_by_default\":{},\"label\":{}}}",
                s.bind_value,
                quote(&s.label_value)
            );
            s.set_call = call("config_set", &args);
        }
    });
}

#[no_mangle]
pub unsafe extern "C" fn on_reply(id: u32, status: u32, ptr: *const u8, len: usize) {
    let payload = bytes(ptr, len);
    STATE.with(|state| {
        let mut s = state.borrow_mut();
        if status != 0 {
            let message = field(payload, "message").unwrap_or("\"failed\"");
            set(s.status, "text", message);
            set(s.save, "loading", "false");
            return;
        }
        if id == s.get_call {
            s.bind_value = field(payload, "bind_by_default") == Some("true");
            s.label_value = field(payload, "label")
                .map(|v| v.trim_matches('"').to_owned())
                .unwrap_or_default();
            set(s.bind, "checked", if s.bind_value { "true" } else { "false" });
            set(s.label, "value", &quote(&s.label_value));
            set(s.status, "text", "\"Loaded\"");
        } else if id == s.set_call {
            set(s.save, "loading", "false");
            set(s.status, "text", "\"Saved\"");
        }
    });
}
