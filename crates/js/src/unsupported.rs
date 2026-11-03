//! What a script asked of Acrobat's object model that PdfCraft doesn't give it, so the gaps
//! show up in [`crate::Outcome::unsupported`] and [`crate::Outcome::inert`] instead of as a
//! script that silently does less than it does in Acrobat.
//!
//! Each object of the model (`app`, `util`, `console`, `color`, `display`, `event`, Field
//! objects and the document, which is the global object) gets a Proxy as its prototype. A
//! property the object has never reaches it; one it lacks does, and the Proxy records the name
//! (as the *JavaScript for Acrobat API Reference* writes it: `Doc.getAnnots`, `Field.textSize`)
//! and answers `undefined`, as the plain prototype would have. Writing a property the object
//! lacks is recorded too (`f.textSize = 10` changes nothing here) and then stored on the object,
//! as an ordinary write would.
//!
//! The document is the global object, where a name a script never defined is an ordinary
//! `ReferenceError` or an `undefined` feature test (`typeof this.initialized`), so only the
//! names Acrobat's Doc object has ([`DOC_NAMES`]) are recorded there, and only those names
//! resolve as bare identifiers (`getAnnots()` as well as `this.getAnnots()`).
//!
//! Names the model has but that do nothing here, or give a fixed answer (`app.response`, a
//! question asked through `app.alert`, `this.calculateNow`), are recorded by those functions
//! themselves as inert ([`inert`]).

use boa_engine::object::builtins::JsProxy;

use super::host::{arg, host};
use super::{Context, JsObject, JsResult, JsValue};

/// The objects whose missing names are recorded, by the name the API reference gives each.
const OWNERS: [&str; 8] = ["Doc", "app", "util", "console", "color", "display", "event", "Field"];

pub(crate) const DOC: usize = 0;
pub(crate) const APP: usize = 1;
pub(crate) const UTIL: usize = 2;
pub(crate) const CONSOLE: usize = 3;
pub(crate) const COLOR: usize = 4;
pub(crate) const DISPLAY: usize = 5;
pub(crate) const EVENT: usize = 6;
pub(crate) const FIELD: usize = 7;

/// Most names kept in each of the two lists of a run; a script that probes more is cut off there.
pub(crate) const MAX_NAMES: usize = 64;

/// Longest name kept, in characters (a property name can be any string).
pub(crate) const MAX_NAME_CHARS: usize = 80;

/// Names that runtime machinery, not a script asking for a feature, looks up on any object:
/// promise resolution (`then`) and `JSON.stringify` (`toJSON`).
const IGNORED: [&str; 2] = ["then", "toJSON"];

/// The Doc object's methods and properties in the public *JavaScript for Acrobat API Reference*
/// (names only). The ones PdfCraft provides are own properties of the global object and never
/// reach the recorder; the list needs no updating when one is added.
const DOC_NAMES: &[&str] = &[
    "addAnnot",
    "addField",
    "addIcon",
    "addLink",
    "addRecipientListCryptFilter",
    "addRequirement",
    "addScript",
    "addThumbnails",
    "addWatermarkFromFile",
    "addWatermarkFromText",
    "addWeblinks",
    "alternatePresentations",
    "author",
    "baseURL",
    "bookmarkRoot",
    "bringToFront",
    "calculate",
    "closeDoc",
    "colorConvertPage",
    "createDataObject",
    "createTemplate",
    "creationDate",
    "creator",
    "dataObjects",
    "delay",
    "deletePages",
    "disclosed",
    "docID",
    "documentFileName",
    "dynamicXFAForm",
    "embedDocAsDataObject",
    "embedOutputIntent",
    "encryptForRecipients",
    "encryptUsingPolicy",
    "exportAsFDF",
    "exportAsFDFStr",
    "exportAsText",
    "exportAsXFDF",
    "exportAsXFDFStr",
    "exportDataObject",
    "exportXFAData",
    "external",
    "extractPages",
    "filesize",
    "flattenPages",
    "getAnnot",
    "getAnnot3D",
    "getAnnots",
    "getAnnots3D",
    "getColorConvertAction",
    "getDataObject",
    "getDataObjectContents",
    "getIcon",
    "getLegalWarnings",
    "getLinks",
    "getNthTemplate",
    "getOCGOrder",
    "getOCGs",
    "getPageBox",
    "getPageLabel",
    "getPageNthWord",
    "getPageNthWordQuads",
    "getPageNumWords",
    "getPageRotation",
    "getPageTransition",
    "getPrintParams",
    "getSound",
    "getTemplate",
    "getURL",
    "gotoNamedDest",
    "hidden",
    "hostContainer",
    "icons",
    "importAnFDF",
    "importAnXFDF",
    "importDataObject",
    "importIcon",
    "importSound",
    "importTextData",
    "importXFAData",
    "innerAppWindowRect",
    "innerDocWindowRect",
    "insertPages",
    "isModal",
    "keywords",
    "layout",
    "mailForm",
    "media",
    "metadata",
    "modDate",
    "mouseX",
    "mouseY",
    "movePage",
    "newPage",
    "noautocomplete",
    "numTemplates",
    "openDataObject",
    "outerAppWindowRect",
    "outerDocWindowRect",
    "pageWindowRect",
    "path",
    "permStatusReady",
    "preflight",
    "producer",
    "removeDataObject",
    "removeField",
    "removeIcon",
    "removeLinks",
    "removeRequirement",
    "removeScript",
    "removeTemplate",
    "removeThumbnails",
    "removeWeblinks",
    "replacePages",
    "requirements",
    "resetForm",
    "saveAs",
    "scroll",
    "securityHandler",
    "selectPageNthWord",
    "selectedAnnots",
    "setAction",
    "setDataObjectContents",
    "setOCGOrder",
    "setPageAction",
    "setPageBoxes",
    "setPageLabels",
    "setPageRotations",
    "setPageTabOrder",
    "setPageTransitions",
    "sounds",
    "spawnPageFromTemplate",
    "spellDictionaryOrder",
    "spellLanguageOrder",
    "subject",
    "submitForm",
    "templates",
    "timestampSign",
    "title",
    "URL",
    "userUnit",
    "viewState",
    "xfa",
    "XFAForeground",
    "zoom",
    "zoomType",
];

/// `name` cut to [`MAX_NAME_CHARS`] characters.
fn clip(name: &str) -> String {
    match name.char_indices().nth(MAX_NAME_CHARS) {
        Some((end, _)) => format!("{}…", name.get(..end).unwrap_or_default()),
        None => name.to_string(),
    }
}

/// The start of `s` as text, long enough for [`clip`] to see that it is longer than a name, without
/// first copying all of a longer string (a property name can be any string).
fn head(s: &boa_engine::JsString) -> String {
    // Two UTF-16 units per character at most.
    let keep = MAX_NAME_CHARS.saturating_add(1).saturating_mul(2);
    let s = if s.len() > keep { s.get(..keep).unwrap_or_default() } else { s.clone() };
    s.to_std_string_escaped()
}

/// Record `name` (`owner.member`) in `list`, up to [`MAX_NAMES`] names.
fn note(list: &mut std::collections::BTreeSet<String>, name: &str) {
    if list.len() < MAX_NAMES {
        list.insert(clip(name));
    }
}

/// Record that the script used `name`, which the model has but which does nothing here or gives
/// a fixed answer.
pub(crate) fn inert(ctx: &Context, name: &str) -> JsResult<()> {
    note(&mut host(ctx)?.borrow_mut().inert, name);
    Ok(())
}

/// Record that the script used `owner.key`, which the model lacks. Symbols and runtime lookups
/// ([`IGNORED`]) aren't a script asking for a feature, and on the document only the Doc object's
/// own names are ([`DOC_NAMES`]).
fn missing(ctx: &Context, owner: usize, key: &JsValue) -> JsResult<()> {
    let Some(key) = key.as_string() else { return Ok(()) };
    if host(ctx)?.borrow().unsupported.len() >= MAX_NAMES {
        return Ok(());
    }
    let key = head(&key);
    if IGNORED.contains(&key.as_str()) || owner == DOC && !DOC_NAMES.contains(&key.as_str()) {
        return Ok(());
    }
    let owner = OWNERS.get(owner).copied().unwrap_or("?");
    note(&mut host(ctx)?.borrow_mut().unsupported, &format!("{owner}.{key}"));
    Ok(())
}

/// The prototype's `get` trap, `(target, key, receiver)`: what `Object.prototype` has (its
/// `toString`, `hasOwnProperty`, …) as it is, anything else recorded and `undefined`.
fn get<const O: usize>(_: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let Some(target) = arg(args, 0).as_object() else { return Ok(JsValue::undefined()) };
    let key = arg(args, 1);
    let Ok(prop) = key.to_property_key(ctx) else { return Ok(JsValue::undefined()) };
    if target.has_property(prop.clone(), ctx)? {
        // `__proto__` read through the receiver is its own prototype, this Proxy; read on the
        // target it would be the target's.
        if key.as_string().is_some_and(|k| k.to_std_string_escaped() == "__proto__") {
            return Ok(arg(args, 2).as_object().and_then(|r| r.prototype()).map_or(JsValue::null(), JsValue::from));
        }
        return target.get(prop, ctx);
    }
    missing(ctx, O, &key)?;
    Ok(JsValue::undefined())
}

/// The prototype's `set` trap, `(target, key, value, receiver)`: a property the object lacks is
/// recorded and stored on the object itself, as an ordinary write stores it.
fn set<const O: usize>(_: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let (Some(target), Some(receiver)) = (arg(args, 0).as_object(), arg(args, 3).as_object()) else { return Ok(JsValue::from(false)) };
    let key = arg(args, 1);
    let Ok(prop) = key.to_property_key(ctx) else { return Ok(JsValue::from(false)) };
    // Setting the prototype through `__proto__` would take the recorder away: refused, as a
    // write to a read-only property is (silently, outside strict mode).
    if key.as_string().is_some_and(|k| k.to_std_string_escaped() == "__proto__") {
        return Ok(JsValue::from(false));
    }
    if !target.has_property(prop.clone(), ctx)? {
        missing(ctx, O, &key)?;
    }
    Ok(JsValue::from(receiver.create_data_property(prop, arg(args, 2), ctx)?))
}

/// The prototype's `has` trap, `(target, key)`: on the document, the Doc object's names are
/// there, so that a bare `getAnnots()` reaches [`get`] instead of failing to resolve.
fn has<const O: usize>(_: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let Some(target) = arg(args, 0).as_object() else { return Ok(JsValue::from(false)) };
    let key = arg(args, 1);
    let Ok(prop) = key.to_property_key(ctx) else { return Ok(JsValue::from(false)) };
    if target.has_property(prop, ctx)? {
        return Ok(JsValue::from(true));
    }
    let doc_name = O == DOC && key.as_string().is_some_and(|k| DOC_NAMES.contains(&k.to_std_string_escaped().as_str()));
    Ok(JsValue::from(doc_name))
}

/// Give `object` (one of [`OWNERS`], by index `O`) the recording prototype.
pub(crate) fn guard<const O: usize>(ctx: &mut Context, object: &JsObject) -> JsResult<()> {
    let target = ctx.intrinsics().constructors().object().prototype();
    let proxy: JsObject = JsProxy::builder(target).get(get::<O>).set(set::<O>).has(has::<O>).build(ctx)?.into();
    // A fresh object always takes it; one made non-extensible would keep its own prototype and
    // only go unrecorded.
    object.set_prototype(Some(proxy));
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::{DocInfo, Event, FieldState, FieldType, Limits, Outcome, run};

    fn go_with(script: &str, doc_scripts: &[String]) -> Outcome {
        let doc = DocInfo { file_name: "form.pdf".into(), num_pages: 2, page: 0, info: Vec::new(), xfa: false };
        let fields = [FieldState::new("price", FieldType::Text, vec!["12".into()]), FieldState::new("total", FieldType::Text, vec![])];
        run(script, &Event::field("Calculate", "total", ""), &doc, &fields, doc_scripts, Limits::default())
    }

    fn go(script: &str) -> Outcome {
        go_with(script, &[])
    }

    /// Names each object lacks are reported by the object the API reference puts them on.
    #[test]
    fn names_the_model_lacks_are_reported_by_object() {
        let o = go("var f = getField('price'); f.textSize = 10; f.borderStyle; app.openDoc; util.printn; console.foo;
             event.richValue; color.convert; display.foo; this.getAnnots; event.target.userName;");
        assert_eq!(o.error, None);
        let want = [
            "Doc.getAnnots",
            "Field.borderStyle",
            "Field.textSize",
            "Field.userName",
            "app.openDoc",
            "color.convert",
            "console.foo",
            "display.foo",
            "event.richValue",
            "util.printn",
        ];
        assert_eq!(o.unsupported, want);
        assert!(o.inert.is_empty());
    }

    /// A script that only uses what the model has reports nothing, and Object.prototype's own
    /// methods and the runtime's lookups (`toJSON`, `then`, symbols) are not a script's request.
    #[test]
    fn supported_names_and_runtime_lookups_report_nothing() {
        let o = go("var f = this.getField('price'); event.value = f.value * 2 + util.printf('%d', 1).length;
             app.alert('hi'); console.println(String(app) + JSON.stringify(event) + app.hasOwnProperty('alert'));
             f.toString(); [].concat(f); Promise.resolve(f); color.black.length + display.hidden + this.numPages;");
        assert_eq!(o.error, None);
        assert_eq!(o.value, "25");
        assert!(o.unsupported.is_empty(), "{:?}", o.unsupported);
        assert!(o.inert.is_empty(), "{:?}", o.inert);
    }

    /// Missing names behave as before: `undefined` when read, a TypeError when called, and a
    /// write is kept on the object. A bare Doc name resolves (and is reported); any other unknown
    /// identifier is still a ReferenceError and a feature test of the document reports nothing.
    #[test]
    fn missing_names_behave_as_plain_javascript() {
        let o =
            go("var f = getField('price'); f.textSize = 10; [typeof app.openDoc, f.textSize, typeof this.initialized, 'getAnnots' in this].join()");
        assert_eq!(o.result.as_deref(), Some("undefined,10,undefined,true"));
        assert_eq!(o.unsupported, ["Field.textSize", "app.openDoc"]);
        let o = go("getAnnots()");
        assert!(o.error.as_deref().is_some_and(|e| e.contains("TypeError")), "{:?}", o.error);
        assert_eq!(o.unsupported, ["Doc.getAnnots"]);
        let o = go("notDefinedAnywhere");
        assert!(o.error.as_deref().is_some_and(|e| e.contains("ReferenceError")), "{:?}", o.error);
        assert!(o.unsupported.is_empty());
    }

    /// Scripts that use Doc names as their own variables and functions keep working, and once
    /// they define a name it is theirs and not reported. Assigning one they never declared is
    /// reported: in Acrobat, `zoom = 2` zooms the view.
    #[test]
    fn scripts_can_define_doc_names_themselves() {
        let docs = ["function getAnnots() { return 3; }".to_string(), "var title = 'mine';".to_string()];
        let o = go_with("zoom = 2; this.path = 'p'; [getAnnots(), title, zoom, path, this.zoom].join()", &docs);
        assert_eq!(o.error, None);
        assert_eq!(o.result.as_deref(), Some("3,mine,2,p,2"));
        assert_eq!(o.unsupported, ["Doc.path", "Doc.zoom"]);
    }

    /// The prototype can't be swapped out from under the recorder, and `__proto__` still reads.
    #[test]
    fn the_recorder_stays_in_place() {
        let o = go("app.__proto__ = null; Object.getPrototypeOf(app) === app.__proto__ && app.__proto__ !== null && app.openDoc === undefined");
        assert_eq!(o.error, None);
        assert_eq!(o.result.as_deref(), Some("true"));
        assert_eq!(o.unsupported, ["app.openDoc"]);
    }

    /// What the model has but can't do here: a question answered OK, a prompt answered with
    /// null, timers that never fire, menu items it doesn't know, Doc methods that do nothing.
    #[test]
    fn inert_calls_are_reported() {
        let o = go("app.alert('Sure?', 2, 2); app.alert('Plain'); app.response('Name?'); app.setTimeOut('x', 10);
             app.execMenuItem('Spelling'); app.execMenuItem('Print'); this.calculateNow(); mailDoc();");
        assert_eq!(o.error, None);
        let want = ["Doc.calculateNow", "Doc.mailDoc", "app.alert", "app.execMenuItem(\"Spelling\")", "app.response", "app.setTimeOut"];
        assert_eq!(o.inert, want);
        assert!(o.unsupported.is_empty());
    }

    /// Document scripts' uses are reported with the script's, since they ran for it.
    #[test]
    fn document_scripts_are_reported_too() {
        let o = go_with("1", &["this.getOCGs();".to_string()]);
        assert_eq!(o.doc_errors.len(), 1);
        assert_eq!(o.unsupported, ["Doc.getOCGs"]);
    }

    /// A script probing endless names keeps at most 64 of each list, each name cut to 80
    /// characters and an ellipsis.
    #[test]
    fn the_lists_are_bounded() {
        let o = go("for (var i = 0; i < 1000; i++) { app['x' + i]; app.execMenuItem('m' + i); } app['y'.repeat(100000)];");
        assert_eq!(o.error, None);
        assert_eq!((o.unsupported.len(), o.inert.len()), (super::MAX_NAMES, super::MAX_NAMES));
        assert!(o.unsupported.iter().chain(&o.inert).all(|n| n.chars().count() <= super::MAX_NAME_CHARS + 1));
        let o = go("app['ÿ'.repeat(100000)]");
        let name = o.unsupported.first().map(String::as_str).unwrap_or_default();
        assert_eq!(name.chars().count(), super::MAX_NAME_CHARS + 1);
        assert!(name.starts_with("app.ÿÿ") && name.ends_with("ÿ…"), "{name}");
    }
}
