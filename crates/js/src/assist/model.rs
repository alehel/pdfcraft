//! What scripts can reach: the names on each object of the form object model, read from a live
//! sandbox set up exactly as [`crate::run`] sets one up, so the editor's suggestions and warnings
//! cannot drift from what the engine provides. A one-line description of each name lives in
//! [`DESCRIPTIONS`]; a test keeps the two equal.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, OnceLock};

use boa_engine::property::PropertyKey;
use boa_engine::{Context, JsObject, JsString};

use crate::globals::{event_object, install};
use crate::host::Host;
use crate::model::{DocInfo, Event};
use crate::{RefCell, document::field_object};

/// An object of the form object model.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Object {
    /// The document: `this` in a script, and the global object, so its names are also bare
    /// globals (`getField`, `numFields`, `app`, …).
    Document,
    App,
    Util,
    Console,
    Display,
    Color,
    Event,
    /// What `getField` returns, and `event.target`.
    Field,
}

impl Object {
    /// The global name of the object, for those scripts reach by name.
    pub fn global(self) -> Option<&'static str> {
        match self {
            Object::App => Some("app"),
            Object::Util => Some("util"),
            Object::Console => Some("console"),
            Object::Display => Some("display"),
            Object::Color => Some("color"),
            Object::Event => Some("event"),
            Object::Document | Object::Field => None,
        }
    }

    /// The object a global name refers to.
    pub fn named(name: &str) -> Option<Object> {
        Object::ALL.into_iter().find(|o| o.global() == Some(name))
    }

    /// How a message names the object: "app", "Field objects", "the document".
    pub fn label(self) -> &'static str {
        match self {
            Object::Document => "the document",
            Object::Field => "Field objects",
            other => other.global().unwrap_or_default(),
        }
    }

    /// "app has", "Field objects have": the subject of a message about the object's names.
    pub fn has(self) -> String {
        match self {
            Object::Field => "Field objects have".into(),
            other => format!("{} has", other.label()),
        }
    }

    pub const ALL: [Object; 8] =
        [Object::Document, Object::App, Object::Util, Object::Console, Object::Display, Object::Color, Object::Event, Object::Field];
}

/// The names scripts can reach.
#[derive(Debug, Default)]
pub struct Model {
    /// Each object's own names.
    pub members: BTreeMap<Object, BTreeSet<String>>,
    /// Every global name: JavaScript's own (`Math`, `Number`, …) and the document's.
    pub globals: BTreeSet<String>,
    /// The names whose value is a function (methods such as `app.alert`, `getField`).
    pub functions: BTreeSet<(Object, String)>,
}

/// Names every object has through `Object.prototype`.
pub(crate) const OBJECT_PROTOTYPE: &[&str] =
    &["constructor", "hasOwnProperty", "isPrototypeOf", "propertyIsEnumerable", "toLocaleString", "toString", "valueOf"];

impl Model {
    /// Whether `object` has `name`, its own or through `Object.prototype`.
    pub fn has(&self, object: Object, name: &str) -> bool {
        OBJECT_PROTOTYPE.contains(&name) || self.members.get(&object).is_some_and(|m| m.contains(name))
    }
}

/// The model, read once from a sandbox. Empty if the sandbox could not be built, which turns the
/// warnings that depend on it off rather than making every name unknown.
pub fn model() -> Arc<Model> {
    static MODEL: OnceLock<Arc<Model>> = OnceLock::new();
    MODEL.get_or_init(|| Arc::new(read().unwrap_or_default())).clone()
}

/// The names of `object` whose own value is a function. Accessors are not called (a Field's
/// `value` getter needs a form).
fn functions(object: &JsObject, names: &BTreeSet<String>, ctx: &mut Context) -> BTreeSet<String> {
    // Through `Object.getOwnPropertyDescriptor`, which reads a descriptor without calling a getter.
    let describe = ctx
        .global_object()
        .get(JsString::from("Object"), ctx)
        .ok()
        .and_then(|o| o.as_object())
        .and_then(|o| o.get(JsString::from("getOwnPropertyDescriptor"), ctx).ok())
        .and_then(|f| f.as_callable());
    let Some(describe) = describe else { return BTreeSet::new() };
    names
        .iter()
        .filter(|name| {
            let args = [object.clone().into(), JsString::from(name.as_str()).into()];
            let descriptor = describe.call(&boa_engine::JsValue::undefined(), &args, ctx).ok().and_then(|d| d.as_object());
            descriptor.and_then(|d| d.get(JsString::from("value"), ctx).ok()).is_some_and(|v| v.is_callable())
        })
        .cloned()
        .collect()
}

fn keys(object: &JsObject, ctx: &mut Context) -> BTreeSet<String> {
    let Ok(keys) = object.own_property_keys(ctx) else { return BTreeSet::new() };
    keys.into_iter()
        .filter_map(|k| match k {
            PropertyKey::String(s) => Some(s.to_std_string_lossy()),
            PropertyKey::Index(i) => Some(i.get().to_string()),
            PropertyKey::Symbol(_) => None,
        })
        .collect()
}

fn read() -> Option<Model> {
    let mut ctx = Context::default();
    let builtin = keys(&ctx.global_object(), &mut ctx);
    // An XFA document, so `xfa_installed` is among the names (it is only there in XFA forms).
    let doc = DocInfo { xfa: true, ..DocInfo::default() };
    ctx.insert_data(RefCell::new(Host::new(Arc::default(), doc)));
    install(&mut ctx).ok()?;
    let event = event_object(&mut ctx, &Event::field("Keystroke", "f", ""));
    let field = field_object(&mut ctx, "f");
    let global = ctx.global_object();
    let all = keys(&global, &mut ctx);
    let mut members = BTreeMap::new();
    let mut callable = BTreeSet::new();
    let mut add = |object: Object, o: &JsObject, names: BTreeSet<String>, ctx: &mut Context| {
        callable.extend(functions(o, &names, ctx).into_iter().map(|n| (object, n)));
        members.insert(object, names);
    };
    add(Object::Document, &global, all.difference(&builtin).cloned().collect(), &mut ctx);
    for object in Object::ALL {
        let Some(name) = object.global().filter(|n| *n != "event") else { continue };
        let value = global.get(JsString::from(name), &mut ctx).ok()?;
        let o = value.as_object()?;
        let names = keys(&o, &mut ctx);
        add(object, &o, names, &mut ctx);
    }
    let names = keys(&event, &mut ctx);
    add(Object::Event, &event, names, &mut ctx);
    let names = keys(&field, &mut ctx);
    add(Object::Field, &field, names, &mut ctx);
    let mut globals = all;
    globals.insert("event".into());
    Some(Model { members, globals, functions: callable })
}

/// One line about each name of the object model, for suggestions.
pub const DESCRIPTIONS: &[(Object, &str, &str)] = &[
    (Object::Document, "app", "the app object: alerts, beeps, the viewer"),
    (Object::Document, "calculateNow", "calculateNow() — recalculate the form"),
    (Object::Document, "color", "the color object: colour arrays such as color.red"),
    (Object::Document, "console", "the console object: println to the JavaScript console"),
    (Object::Document, "dirty", "dirty — accepted; PdfCraft tracks changes itself"),
    (Object::Document, "display", "the display object: visible, hidden, noPrint, noView"),
    (Object::Document, "documentFileName", "the document's file name"),
    (Object::Document, "getField", "getField(name) — the field called name"),
    (Object::Document, "getNthFieldName", "getNthFieldName(n) — the name of field n (0-based)"),
    (Object::Document, "info", "the document information: info.title, info.author, …"),
    (Object::Document, "mailDoc", "mailDoc() — accepted; PdfCraft sends no mail"),
    (Object::Document, "numFields", "the number of fields in the form"),
    (Object::Document, "numPages", "the number of pages"),
    (Object::Document, "pageNum", "the current page (0-based); set it to go to a page"),
    (Object::Document, "print", "print(…) — print the document"),
    (Object::Document, "resetForm", "resetForm([names]) — reset the named fields, or all"),
    (Object::Document, "submitForm", "submitForm(url) — explained, not sent"),
    (Object::Document, "syncAnnotScan", "syncAnnotScan() — accepted; nothing to wait for"),
    (Object::Document, "util", "the util object: printf, printd, printx, scand"),
    (Object::Document, "xfa_installed", "true in an XFA form"),
    (Object::Document, "xfa_version", "the XFA version, 3.5"),
    (Object::App, "alert", "app.alert(message, icon, buttons, title) — show a message"),
    (Object::App, "beep", "app.beep() — sound a beep"),
    (Object::App, "clearInterval", "app.clearInterval(timer) — accepted; timers don't run"),
    (Object::App, "clearTimeOut", "app.clearTimeOut(timer) — accepted; timers don't run"),
    (Object::App, "execMenuItem", "app.execMenuItem(name) — run a menu item"),
    (Object::App, "formsVersion", "the forms version number"),
    (Object::App, "language", "the viewer's language code"),
    (Object::App, "launchURL", "app.launchURL(url) — open a web link"),
    (Object::App, "platform", "the platform: WIN, MAC or UNIX"),
    (Object::App, "response", "app.response(question) — returns null in PdfCraft"),
    (Object::App, "setInterval", "app.setInterval(code, ms) — accepted; timers don't run"),
    (Object::App, "setTimeOut", "app.setTimeOut(code, ms) — accepted; timers don't run"),
    (Object::App, "viewerType", "the viewer type"),
    (Object::App, "viewerVariation", "the viewer variation"),
    (Object::App, "viewerVersion", "the viewer version number"),
    (Object::Util, "printd", "util.printd(format, date) — format a date"),
    (Object::Util, "printf", "util.printf(format, …) — format numbers and text"),
    (Object::Util, "printx", "util.printx(mask, text) — format text with a mask"),
    (Object::Util, "scand", "util.scand(format, text) — read a date"),
    (Object::Console, "clear", "console.clear() — empty the console"),
    (Object::Console, "hide", "console.hide() — accepted"),
    (Object::Console, "println", "console.println(text) — write a line to the console"),
    (Object::Console, "show", "console.show() — accepted"),
    (Object::Display, "hidden", "display.hidden — not shown or printed"),
    (Object::Display, "noPrint", "display.noPrint — shown, not printed"),
    (Object::Display, "noView", "display.noView — printed, not shown"),
    (Object::Display, "visible", "display.visible — shown and printed"),
    (Object::Color, "black", "color.black — a colour array"),
    (Object::Color, "blue", "color.blue — a colour array"),
    (Object::Color, "cyan", "color.cyan — a colour array"),
    (Object::Color, "dkGray", "color.dkGray — a colour array"),
    (Object::Color, "gray", "color.gray — a colour array"),
    (Object::Color, "green", "color.green — a colour array"),
    (Object::Color, "ltGray", "color.ltGray — a colour array"),
    (Object::Color, "magenta", "color.magenta — a colour array"),
    (Object::Color, "red", "color.red — a colour array"),
    (Object::Color, "transparent", "color.transparent — a colour array"),
    (Object::Color, "white", "color.white — a colour array"),
    (Object::Color, "yellow", "color.yellow — a colour array"),
    (Object::Event, "change", "the text being typed or pasted (Keystroke)"),
    (Object::Event, "changeEx", "the export value of the chosen item"),
    (Object::Event, "commitKey", "how the value was committed"),
    (Object::Event, "modifier", "whether a modifier key was held"),
    (Object::Event, "name", "the event's name: Keystroke, Validate, Calculate, Format, …"),
    (Object::Event, "rc", "false to reject the change or value"),
    (Object::Event, "selEnd", "the end of the selection (Keystroke)"),
    (Object::Event, "selStart", "the start of the selection (Keystroke)"),
    (Object::Event, "shift", "whether Shift was held"),
    (Object::Event, "target", "the field the event is for"),
    (Object::Event, "targetName", "the name of the field the event is for"),
    (Object::Event, "type", "the event's type: Field or Doc"),
    (Object::Event, "value", "the field's value; set it to change the value"),
    (Object::Event, "willCommit", "true when the value is being committed"),
    (Object::Field, "charLimit", "the most characters the field takes"),
    (Object::Field, "checkThisBox", "checkThisBox(index, checked) — check or clear a box"),
    (Object::Field, "currentValueIndices", "the index of the chosen item(s)"),
    (Object::Field, "defaultValue", "the value a reset gives back"),
    (Object::Field, "display", "display.visible, hidden, noPrint or noView"),
    (Object::Field, "exportValues", "the export values of check boxes or radio buttons"),
    (Object::Field, "fillColor", "the background colour, as a colour array"),
    (Object::Field, "getItemAt", "getItemAt(index, export) — a list item"),
    (Object::Field, "hidden", "true when hidden"),
    (Object::Field, "isBoxChecked", "isBoxChecked(index) — whether a box is checked"),
    (Object::Field, "name", "the field's full name"),
    (Object::Field, "numItems", "the number of list items"),
    (Object::Field, "readonly", "true when read-only"),
    (Object::Field, "required", "true when required"),
    (Object::Field, "setFocus", "setFocus() — move the focus to the field"),
    (Object::Field, "textColor", "the text colour, as a colour array"),
    (Object::Field, "type", "text, checkbox, radiobutton, combobox, listbox, button or signature"),
    (Object::Field, "value", "the field's value"),
    (Object::Field, "valueAsString", "the value as text"),
];

/// The description of `object`'s `name`, if it has one.
pub fn describe(object: Object, name: &str) -> Option<&'static str> {
    DESCRIPTIONS.iter().find(|(o, n, _)| *o == object && *n == name).map(|(_, _, d)| *d)
}
