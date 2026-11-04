//! Scripts crafted to abort the process or stall it: boa's parser, compiler and tree walks recurse
//! on a script's nesting, a stack overflow is not a panic and takes the whole app down, and several
//! passes cost the square of what they walk. These hold the limits in `vendor/boa_parser`.
//!
//! A test that stops the run with a stack overflow fails too: that is what these guard against.
//! Each input has its own test, so a run names the one that did it.

use std::sync::mpsc;
use std::time::Duration;

use boa_engine::{Context, Script, Source};
use pdfcraft_js::xfa::{XfaDoc, XfaEvent, XfaNode, run_xfa};
use pdfcraft_js::{DocInfo, Event, Limits, Outcome, run};

/// Far longer than a limited script should ever take here, milliseconds, so that a busy machine is
/// no failure. An input that takes less than this unlimited still fails: on its missing error.
const TIME_LIMIT: Duration = Duration::from_secs(30);

const NESTED: &str = "nested too deeply";
const CHAIN: &str = "too long to run safely";
const NAMES: &str = "too many names";

fn parens() -> String {
    "(".repeat(200_000)
}

/// `what()` on a thread of its own, within [`TIME_LIMIT`]. A stalled script's thread is left to
/// run: the test has failed, and the process ends with the run.
fn in_time<T: Send + 'static>(what: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .stack_size(1 << 30)
        .spawn(move || {
            let _ = tx.send(what());
        })
        .expect("a thread for the script");
    rx.recv_timeout(TIME_LIMIT).expect("the script stalled, or the engine stopped with an error")
}

fn field_script(script: String) -> Outcome {
    in_time(move || run(&script, &Event::field("Calculate", "total", ""), &doc(), &[], &[], Limits::default()))
}

fn doc() -> DocInfo {
    DocInfo { file_name: "order.pdf".into(), num_pages: 1, page: 0, info: vec![], xfa: false }
}

/// `script` fails with an error saying `why` wherever a document can put a script: as a field
/// script, as a document script (the field script still runs), and as an XFA script, which runs
/// through `eval`.
fn fails_everywhere(script: String, why: &str) {
    let o = field_script(script.clone());
    assert!(o.error.as_deref().is_some_and(|e| e.contains(why)), "as a field script: {:?}", o.error);

    let docs = [script.clone()];
    let o = in_time(move || run("event.value = 'ran';", &Event::field("Calculate", "total", ""), &doc(), &[], &docs, Limits::default()));
    assert_eq!((o.error.as_deref(), o.value.as_str()), (None, "ran"));
    assert!(o.doc_errors.len() == 1 && o.doc_errors[0].contains(why), "as a document script: {:?}", o.doc_errors);

    let o = in_time(move || {
        let event = XfaEvent { activity: "calculate".into(), target: "form1[0]".into(), ..Default::default() };
        let root = XfaNode { name: "form1".into(), som: "form1[0]".into(), ..Default::default() };
        run_xfa(&script, &event, &XfaDoc::default(), &root, Limits::default())
    });
    assert!(o.error.as_deref().is_some_and(|e| e.contains(why)), "as an XFA script: {:?}", o.error);
}

macro_rules! fail {
    ($($name:ident: $why:expr, $script:expr;)*) => {
        $(#[test] fn $name() { fails_everywhere($script, $why); })*
    };
}

// Nesting that a byte scanner in front of the parser could not see, which overflowed the stack and
// aborted: closers inside regular expressions and templates cancelled real openers, comments it
// did not know hid them, and a regular expression read as a division (or the other way round)
// started a "string" that ran to the end of the line.
fail! {
    comment_ended_by_a_carriage_return: NESTED, format!("// c\r{}", parens());
    closers_inside_regular_expressions: NESTED, "[/]/".repeat(65_000);
    closers_inside_templates: NESTED, "(`)`".repeat(65_000);
    html_open_comment_hiding_a_block_comment: NESTED, format!("<!-- /*\n{}", parens());
    html_close_comment_hiding_a_block_comment: NESTED, format!("\n--> /*\n{}", parens());
    hashbang_hiding_a_block_comment: NESTED, format!("#!/*\n{}", parens());
    html_open_comment_after_code: NESTED, format!("x=1<!--/*\n{}", parens());
    division_after_a_postfix_operator_read_as_a_regular_expression: NESTED, format!("x++ / '/'; {}", parens());
    division_after_an_object_read_as_a_regular_expression: NESTED, format!("a={{}}/'/';{}", parens());
    division_after_a_function_read_as_a_regular_expression: NESTED, format!("f=function(){{}}/'/';{}", parens());
    regular_expression_after_extends_read_as_a_division: NESTED, format!("class A extends /'/ {{}} {}", parens());
    regular_expression_after_debugger_read_as_a_division: NESTED, format!("debugger\n/'/;{}", parens());
    regular_expression_holding_a_quote: NESTED, format!("/'/{}", parens());
}

// Scripts that stalled while parsing: a span walks the whole tree under it, and the parser took
// one at each link of a chain; an arrow function or a loop searches its whole body for direct
// `eval`, at each level.
fail! {
    member_chain: CHAIN, format!("x{}", ".target".repeat(36_000));
    member_chain_with_spaces: CHAIN, format!("x{}", ". a".repeat(87_000));
    member_chain_with_escapes: CHAIN, format!("x{}", ".\\u0061".repeat(37_000));
    member_chain_with_comments: CHAIN, format!("x{}", "/**/.a".repeat(43_000));
    member_chain_with_indexes: CHAIN, "[0].a".repeat(52_000);
    index_chain_after_a_call: CHAIN, format!("f(){}", "[0]".repeat(87_000));
    index_chain: CHAIN, format!("x{}", "[0]".repeat(87_000));
    arrow_functions: NESTED, format!("{}a", "a=>".repeat(20_000));
    many_arrow_functions: NESTED, format!("{}a", "a=>".repeat(87_000));
    loops: NESTED, format!("{};", "for(;;)".repeat(20_000));
    arrow_functions_with_assignments: NESTED, format!("{}a", "a=>c=".repeat(52_000));
    async_arrow_functions: NESTED, format!("{}a", "async a=>".repeat(29_000));
    member_chains_nested_in_indexes: CHAIN, nest(64, 1020, "x");
}

/// `inner` wrapped `levels` times as `a[inner].b.b…`, with `links` of `.b`.
fn nest(levels: usize, links: usize, inner: &str) -> String {
    let mut s = inner.to_string();
    for _ in 0..levels {
        s = format!("a[{s}]{}", ".b".repeat(links));
    }
    s
}

// Scripts that parsed quickly but stalled when run: the compiler holds a register for each level
// of the tree, and scans them all for each one it takes.
fail! {
    chained_assignments: NESTED, format!("{}a", "a=".repeat(131_000));
    tagged_templates: CHAIN, format!("a{}", "``".repeat(131_000));
    additions: CHAIN, format!("{}a", "a+".repeat(131_000));
    exponents: NESTED, format!("{}a", "a**".repeat(87_000));
    commas: CHAIN, format!("{}a", "a,".repeat(131_000));
}

// Every other way a script recurses or builds a tall tree. Nothing in front of the parser caught
// these either: a bracket count sees none of the first group.
fail! {
    nested_unary_operators: NESTED, format!("{}a", "!".repeat(131_000));
    nested_typeof: NESTED, format!("{}a", "typeof ".repeat(37_000));
    nested_new: NESTED, format!("{}a", "new ".repeat(65_000));
    nested_conditionals: NESTED, format!("{}a", "a?a:".repeat(65_000));
    nested_labels: NESTED, format!("{}a", "a:".repeat(131_000));
    nested_ifs: NESTED, format!("{}a", "if(a)".repeat(52_000));
    nested_whiles: NESTED, format!("{}a", "while(a)".repeat(32_000));
    nested_withs: NESTED, format!("{}a", "with(a)".repeat(37_000));
    nested_function_declarations: NESTED, "function a(){".repeat(20_000);
    nested_class_declarations: NESTED, "class A{m(){".repeat(21_000);
    nested_static_blocks: NESTED, "class A{static{".repeat(17_000);
    nested_class_heritage: NESTED, "class A extends ".repeat(16_000);
    nested_array_binding_patterns: NESTED, format!("let {}a", "[".repeat(200_000));
    nested_rest_binding_patterns: NESTED, format!("let {}a", "[...".repeat(65_000));
    nested_object_binding_patterns: NESTED, format!("let {}a", "{a:".repeat(87_000));
    nested_parameter_patterns: NESTED, format!("function f({}a", "[".repeat(200_000));
    nested_catch_patterns: NESTED, format!("try{{}}catch({}a", "[".repeat(200_000));
    nested_for_of_patterns: NESTED, format!("for(let {}a", "[".repeat(200_000));
    or_chains: CHAIN, format!("{}a", "a||".repeat(87_000));
    and_chains: CHAIN, format!("{}a", "a&&".repeat(87_000));
    coalesce_chains: CHAIN, format!("{}a", "a??".repeat(87_000));
    or_chains_of_and_chains: CHAIN, format!("{}a", "a||a&&".repeat(43_000));
    and_chain_after_an_or: CHAIN, format!("a||{}a", "a&&".repeat(87_000));
    comparison_chains: CHAIN, format!("{}a", "a<".repeat(131_000));
    instanceof_chains: CHAIN, format!("{}a", "a instanceof ".repeat(20_000));
    bitwise_chains: CHAIN, format!("{}a", "a|".repeat(131_000));
    multiplication_chains: CHAIN, format!("{}a", "a*".repeat(131_000));
    call_chains: CHAIN, format!("a{}", "()".repeat(131_000));
    member_chains_after_a_call: CHAIN, format!("a(){}", ".a".repeat(131_000));
    tagged_templates_after_a_call: CHAIN, format!("a(){}", "``".repeat(131_000));
    else_if_ladders: CHAIN, format!("{}a", "if(a);else ".repeat(23_000));
    ladders_nested_in_ladders: CHAIN, ladder_in_ladder();
    template_substitutions: CHAIN, format!("`{}`", "${a}".repeat(65_000));
    tagged_template_substitutions: CHAIN, format!("a`{}`", "${a}".repeat(65_000));
    chains_on_chains: CHAIN, chain_on_chain();
}

/// Ladders whose every last `else` holds the next: each stays under the limit on its own, and
/// the tree is as tall as all of them together.
fn ladder_in_ladder() -> String {
    let ladder = "if(a);else ".repeat(2000);
    format!("{ladder}{{{ladder}{{{ladder}a}}}}")
}

/// Chains whose first operand is the next: each stays under the limit on its own, and the tree
/// is as tall as all of them together.
fn chain_on_chain() -> String {
    let (level, levels) = ("+a".repeat(2000), 10);
    format!("{}a{}", "(".repeat(levels), format!("{level})").repeat(levels))
}

// Cost that doesn't come from nesting: the parser and the compiler find a name by walking a list,
// and one function could declare tens of thousands of them. Each `let` also takes a register, and
// the compiler scans those: names can repeat, block by block, and still cost that.
fail! {
    declared_lets: NAMES, format!("let {}z", numbered("a", ",", 30_000));
    declared_consts_in_a_block: NAMES, format!("{{{}}}", numbered("let a", ";", 24_000));
    declared_vars_in_a_function: NAMES, format!("function f(){{var {}z}}", numbered("a", ",", 30_000));
    declared_vars_in_blocks: NAMES, format!("function f(){{{}}}", (0..20_000).map(|i| format!("{{var a{i}}}")).collect::<String>());
    declared_functions_in_a_function: NAMES, format!("function f(){{{}}}", (0..14_000).map(|i| format!("function a{i}(){{}}")).collect::<String>());
    declared_block_functions: NAMES, format!("function f(){{{}}}", (0..12_000).map(|i| format!("{{function a{i}(){{}}}}")).collect::<String>());
    declared_parameters: NAMES, format!("function f({}z){{}}", numbered("a", ",", 30_000));
    declared_parameters_with_defaults: NAMES, format!("function f({}z){{}}", numbered("a", "=1,", 30_000));
    declared_destructured_names: NAMES, format!("let {{{}z}}=x", numbered("a", ",", 30_000));
    declared_in_a_loop_head: NAMES, format!("for(let {}z;0;);", numbered("a", ",", 30_000));
    declared_in_a_switch: NAMES, format!("switch(x){{case 0:{}}}", numbered("let a", ";", 24_000));
    declared_in_arrow_parameters_read_as_an_object: NAMES, format!("({{{}z}})=>0", numbered("a", ",", 30_000));
    declared_in_arrow_parameters_read_as_an_array: NAMES, format!("([{}z])=>0", numbered("a", ",", 30_000));
    repeated_lets_in_blocks: NAMES, format!("{{let {}}}", ('a'..='z').chain('A'..='Z').map(String::from).collect::<Vec<_>>().join(",")).repeat(2400);
    repeated_destructured_lets_in_blocks: NAMES, format!("{{let [{}]=x}}", ('a'..='z').chain('A'..='Z').map(String::from).collect::<Vec<_>>().join(",")).repeat(2300);
}

/// `prefix0 end prefix1 end …`
fn numbered(prefix: &str, end: &str, n: usize) -> String {
    (0..n).map(|i| format!("{prefix}{i}{end}")).collect()
}

// Leaving a `finally`, the compiler copies every jump still pending, for each `try` it leaves.
fail! {
    jumps_through_many_finally_blocks: NESTED, format!("for(;;){{{}{}{}}}", "try{".repeat(250), "break;".repeat(43_000), "}finally{}".repeat(250));
}

// The compiler compiles a labelled function twice, so nesting them doubles the work per level: this
// is under 1 KiB, and would take months.
fail! {
    labelled_functions_inside_each_other: "labelled function", format!("{}1{}", "a:function b(){".repeat(40), "}".repeat(40));
    labelled_functions_with_many_labels_inside_each_other: "labelled function", format!("{}1{}", "a:b:function c(){".repeat(40), "}".repeat(40));
    labelled_functions_inside_each_other_through_a_function: "labelled function", format!("{}1{}", "a:function b(){function c(){".repeat(40), "}}".repeat(40));
}

// A long regular expression costs the square of its length in the regular expression engine:
// `a|a|…` when it is evaluated, and groups when the parser validates it.
fail! {
    long_regular_expression_alternatives: "regular expression is too long", format!("/{}a/", "a|".repeat(131_000));
    long_regular_expression_groups: "regular expression is too long", format!("/{}/", "(a)".repeat(87_000));
}

/// boa's optimizer, on by default, runs while a script is parsed. It evaluates constants whatever
/// they cost, and copies what is left of a dead `if` at each level of a ladder. A script is safe to
/// parse even if running it is not: these never reach what would be slow.
#[test]
fn nothing_is_evaluated_while_parsing() {
    let o = field_script("function f(){return 3n**499999999n}event.value='ran';".to_string());
    assert_eq!((o.error.as_deref(), o.value.as_str()), (None, "ran"));
    let o = field_script("if(false)1n<<2147483647n;event.value='ran';".to_string());
    assert_eq!((o.error.as_deref(), o.value.as_str()), (None, "ran"));
    let ladder = format!("{}{{{}}}", "if(!1);else ".repeat(4000), "event.value=1;".repeat(15_000));
    let o = field_script(ladder);
    assert_eq!((o.error.as_deref(), o.value.as_str()), (None, "1"));
}

/// The limits read the script as JavaScript, so what a byte scanner got wrong now runs. It refused
/// these for counting the brackets in regular expressions.
#[test]
fn brackets_in_regular_expressions_do_not_nest() {
    let script = format!("var s='a((b';\n{}event.value=s;", "s=s.replace(/\\(/g,'');\n".repeat(70));
    let o = field_script(script);
    assert_eq!((o.error.as_deref(), o.value.as_str()), (None, "ab"));
    let o = field_script(format!("event.value=/[(]/.test('(')+{}", "/[(]/.source.length;".to_string() + &"/[(]/;".repeat(100)));
    assert_eq!((o.error.as_deref(), o.value.as_str()), (None, "4"));
    // … and the brackets in strings and comments, which it did not.
    let o = field_script(format!("// {}\nevent.value='{}'.length;", "(".repeat(1000), "[".repeat(1000)));
    assert_eq!((o.error.as_deref(), o.value.as_str()), (None, "1000"));
}

/// What a form might really do at each limit's edge. A limit that stops these breaks documents.
#[test]
fn scripts_within_the_limits_run() {
    // 80 parentheses: the old limit was 64.
    let o = field_script(format!("event.value={}1{};", "(".repeat(80), ")".repeat(80)));
    assert_eq!((o.error.as_deref(), o.value.as_str()), (None, "1"));
    // Over 3,000 terms in a sum.
    let o = field_script(format!("event.value=1{};", "+1".repeat(3000)));
    assert_eq!((o.error.as_deref(), o.value.as_str()), (None, "3001"));
    // A thousand lines of text put together, and a long list of allowed values.
    let o = field_script(format!("var s=''{};event.value=s.length;", "\n+'line'".repeat(1000)));
    assert_eq!((o.error.as_deref(), o.value.as_str()), (None, "4000"));
    let list: String = (0..800).map(|i| format!("||v=={i}")).collect();
    let o = field_script(format!("var v=799;event.value=(false{list})?'yes':'no';"));
    assert_eq!((o.error.as_deref(), o.value.as_str()), (None, "yes"));
    // A long `else if` ladder, which ends in the branch it should.
    let ladder: String = (0..1500).map(|i| format!("if(v=={i})event.value='b{i}';else ")).collect();
    let o = field_script(format!("var v=1499;{ladder}event.value='none';"));
    assert_eq!((o.error.as_deref(), o.value.as_str()), (None, "b1499"));
    // `try` inside `try`, as deep as a form could want.
    let o = field_script(format!("{}event.value=1;{}", "try{".repeat(40), "}catch(e){}".repeat(40)));
    assert_eq!((o.error.as_deref(), o.value.as_str()), (None, "1"));
    // A nested `if`, a nested loop, and a chain of method calls.
    let o = field_script(format!("var n=0;{}n++;event.value=n;", "if(1)".repeat(100)));
    assert_eq!((o.error.as_deref(), o.value.as_str()), (None, "1"));
    let o = field_script(format!("var s='a';event.value=s{}.length;", ".concat('b')".repeat(2000)));
    assert_eq!((o.error.as_deref(), o.value.as_str()), (None, "2001"));
    // 2,000 names in one function, and more than that in the script's functions together.
    let vars: String = (0..1990).map(|i| format!("var a{i}={i};")).collect();
    let o = field_script(format!("function f(){{{vars}return a1989}}function g(){{{vars}return a1}}event.value=f()+g();"));
    assert_eq!((o.error.as_deref(), o.value.as_str()), (None, "1990"));
    // A labelled function at all, with others beside it.
    let o = field_script("a:function f(){return 1}b:function g(){return 2}event.value=f()+g();".to_string());
    assert_eq!((o.error.as_deref(), o.value.as_str()), (None, "3"));
    // An ordinary validation script.
    let o = field_script(
        "function valid(s) {\n\
           var parts = /^(\\d{4})-(\\d{2})-(\\d{2})$/.exec(s);\n\
           if (!parts) return false;\n\
           var m = Number(parts[2]), d = Number(parts[3]);\n\
           if (m < 1 || m > 12) return false; else if (d < 1 || d > 31) return false;\n\
           return [m, d].every(function (x) { return x > 0; });\n\
         }\n\
         event.value = valid('2026-11-04') && !valid('2026-13-04') ? 'ok' : 'bad';"
            .to_string(),
    );
    assert_eq!((o.error.as_deref(), o.value.as_str()), (None, "ok"));
}

/// The limits' own edges: a script is as good as the next up to them, and refused past them.
#[test]
fn the_limits_are_where_they_say() {
    let ok = |s: String| field_script(s).error;
    assert_eq!(ok(format!("{}1{}", "(".repeat(84), ")".repeat(84))), None);
    assert!(ok(format!("{}1{}", "(".repeat(85), ")".repeat(85))).is_some_and(|e| e.contains(NESTED)));
    // What takes more of the stack, or more of the compiler, counts for more: a class body two
    // levels, a `try` five.
    assert_eq!(ok(format!("{}{}", "class A{m(){".repeat(120), "}}".repeat(120))), None);
    assert!(ok(format!("{}{}", "class A{m(){".repeat(130), "}}".repeat(130))).is_some_and(|e| e.contains(NESTED)));
    assert_eq!(ok(format!("{}{}", "try{".repeat(50), "}catch(e){}".repeat(50))), None);
    assert!(ok(format!("{}{}", "try{".repeat(52), "}catch(e){}".repeat(52))).is_some_and(|e| e.contains(NESTED)));
    // A sum's first term is itself two levels high.
    assert_eq!(ok(format!("1{}", "+1".repeat(4094))), None);
    assert!(ok(format!("1{}", "+1".repeat(4095))).is_some_and(|e| e.contains(CHAIN)));
    assert_eq!(ok(format!("let {}z", numbered("a", ",", 2047))), None);
    assert!(ok(format!("let {}y,z", numbered("a", ",", 2047))).is_some_and(|e| e.contains(NAMES)));
    // Different names: declaring one again costs nothing, and generated scripts do it a lot.
    assert_eq!(ok("var a;".repeat(5000)), None);
    assert_eq!(ok("for(var i=0;i<1;i++);".repeat(3000)), None);
    assert_eq!(ok("try{}catch(e){}".repeat(3000)), None);
    assert_eq!(ok(format!("function f({}a){{}}", "a,".repeat(5000))), None);
    // Across a function's blocks, not per block; and a function's names are its own.
    assert!(ok(format!("{{let {}z}}{{let y}}", numbered("a", ",", 2047))).is_some_and(|e| e.contains(NAMES)));
    assert_eq!(ok(format!("function f(){{let {}z}}function g(){{let {}z}}", numbered("a", ",", 2047), numbered("a", ",", 2047))), None);
    assert_eq!(ok(format!("function f({}z){{let {}y}}", numbered("a", ",", 2047), numbered("b", ",", 2047))), None);
    // `let`s, which take a register each, count every time.
    assert_eq!(ok("{let a}".repeat(8192)), None);
    assert!(ok("{let a}".repeat(8193)).is_some_and(|e| e.contains(NAMES)));
    assert_eq!(ok(format!("function f(){{{}}}function g(){{{}}}", "{let a}".repeat(8192), "{let a}".repeat(8192))), None);
    assert_eq!(ok(format!("/{}/", "a".repeat(8192))), None);
    assert!(ok(format!("/{}/", "a".repeat(8193))).is_some_and(|e| e.contains("too long")));
}

/// `||` is parsed in a loop and an `else if` ladder too: both give what they gave when each was
/// parsed by recursion, and refuse what they refused.
#[test]
fn logical_operators_and_ladders_mean_what_they_meant() {
    for (expression, value) in [
        ("0||''||3", "3"),
        ("1||x()", "1"),
        ("0||1&&2", "2"),
        // `&&` binds tighter on the right of `||` too, so these are not `(1||0)&&…`.
        ("1||0&&x()", "1"),
        ("1||0&&0", "1"),
        ("1||0&&0||0", "1"),
        ("0||1&&0||2", "2"),
        ("1&&0||2", "2"),
        ("0&&x()||0&&x()||5", "5"),
        ("0||0&&x()||6", "6"),
        ("1&&2&&3||x()", "3"),
        ("(null??7)||8", "7"),
        ("null??(0||9)", "9"),
        ("(0||null)??10", "10"),
    ] {
        let o = field_script(format!("event.value={expression};"));
        assert_eq!((o.error.as_deref(), o.value.as_str()), (None, value), "{expression}");
    }
    // What is evaluated, and in what order.
    let o = field_script(
        "var seen='';function f(n,v){seen+=n;return v}\
         f(1,0)||f(2,0)&&f(3,1)||f(4,1)||f(5,1);event.value=seen;"
            .to_string(),
    );
    assert_eq!((o.error.as_deref(), o.value.as_str()), (None, "124"));
    for mixed in ["a||b??c", "a??b||c", "a&&b??c", "a??b&&c", "a||b&&c??d", "a??b??c||d"] {
        let o = field_script(format!("var a,b,c,d;{mixed};"));
        assert!(o.error.as_deref().is_some_and(|e| e.contains("SyntaxError")), "{mixed}: {:?}", o.error);
    }
    for (clauses, value) in [
        ("if(1)r='a';else if(1)r='b';else r='c';", "a"),
        ("if(0)r='a';else if(1)r='b';else r='c';", "b"),
        ("if(0)r='a';else if(0)r='b';else r='c';", "c"),
        ("if(0)r='a';else if(0)r='b';", "none"),
        // The `else` belongs to the nearest `if`.
        ("if(1)if(0)r='a';else r='b';else r='c';", "b"),
        ("if(0)if(0)r='a';else r='b';else if(1)r='c';else r='d';", "c"),
        ("if(1){if(0)r='a'}else if(1)r='b';", "none"),
        // A function declaration where an `else` statement goes, as the Annex B grammar allows.
        ("if(0);else if(0);else function g(){}\nr=typeof g;", "function"),
        ("if(0);else if(1)function h(){}\nr=typeof h;", "function"),
    ] {
        let o = field_script(format!("var r='none';{clauses}event.value=r;"));
        assert_eq!((o.error.as_deref(), o.value.as_str()), (None, value), "{clauses}");
    }
    for refused in [
        // An `if` with an escape in it is no keyword.
        "if(0);else \\u0069f(1);",
        // Labelled functions in a clause.
        "if(0);else if(0)a:function f(){}",
        "if(0);else if(0);else a:function f(){}",
        // A clause cut short, and an `else` with no statement.
        "if(0);else if",
        "if(0);else if(1)",
        "if(0);else if(1);else",
    ] {
        let o = field_script(refused.to_string());
        assert!(o.error.as_deref().is_some_and(|e| e.contains("SyntaxError")), "{refused}: {:?}", o.error);
    }
    // In strict code, a function can't be a clause.
    let o = field_script("'use strict';if(0);else if(0);else function g(){}".to_string());
    assert!(o.error.as_deref().is_some_and(|e| e.contains("SyntaxError")), "{:?}", o.error);
}

/// The parser takes a chain's start once now, not at each link: only a private member uses it.
#[test]
fn private_members_after_a_chain_still_parse() {
    let o = field_script("class A{#p=1;m(){return this.a.b.#p}}new A().m();".to_string());
    let error = o.error.unwrap_or_default();
    assert!(!error.contains("SyntaxError"), "{error}");
    let o = field_script("class A{#p=1;m(){return f().a.b.#p}}event.value=typeof A;".to_string());
    assert_eq!((o.error.as_deref(), o.value.as_str()), (None, "function"));
}

/// The stack in which the limits leave room, whatever the build: scripts at the limits take about
/// 3 MiB in the parser in a release build, and the dev profile optimizes dependencies too.
const STACK_BOUND: usize = 16 << 20;

/// Each script parses, compiles and is dropped on a stack far smaller than the script thread's, so
/// a limit that stopped bounding the stack shows here as an abort, and not only at 1 GiB. Not run:
/// nothing here is meant to do anything.
fn fit_a_small_stack(scripts: Vec<String>) {
    let on_thread = std::thread::Builder::new().stack_size(STACK_BOUND).spawn(move || {
        let mut context = Context::default();
        for script in scripts {
            if let Ok(parsed) = Script::parse(Source::from_bytes(&script), None, &mut context) {
                let _ = parsed.codeblock(&mut context);
            }
        }
    });
    assert!(on_thread.is_ok_and(|thread| thread.join().is_ok()), "the engine stopped with an error");
}

/// How many times to repeat something: around each limit's edge in every shape's units, and far
/// past all of them.
const NESTINGS: [usize; 5] = [20, 40, 80, 250, 20_000];

/// Every kind of expression nesting.
#[test]
fn expression_nesting_fits_a_small_stack() {
    let mut scripts = Vec::new();
    for (open, close) in [
        ("(", ")"),
        ("[", "]"),
        ("{a:", "}"),
        ("a[", "]"),
        ("a(", ")"),
        ("new a(", ")"),
        ("a?.[", "]"),
        ("a?.(", ")"),
        ("`${", "}`"),
        ("a`${", "}`"),
        ("{[", "]:1}"),
        ("[...", "]"),
        ("{...", "}"),
        ("function(){return ", "}"),
        ("function(a=", "){}"),
        ("function*(){yield ", "}"),
        ("async function(){await ", "}"),
        ("class{m(){return ", "}}"),
        ("class{[", "]}"),
        ("class{a=", "}"),
        ("class extends ", "{}"),
        ("{m(){return ", "}}"),
        ("{get a(){return ", "}}"),
        ("a=>", ""),
        ("(a)=>", ""),
        ("async a=>", ""),
        ("async(a)=>", ""),
        ("(a=", ")=>0"),
        ("a=", ""),
        ("a+=", ""),
        ("a?a:", ""),
        ("a?(", "):a"),
        ("!", ""),
        ("-", ""),
        ("- ", ""),
        ("~", ""),
        ("void ", ""),
        ("typeof ", ""),
        ("delete ", ""),
        ("++", ""),
        ("new ", ""),
        ("a**", ""),
        ("(a)**", ""),
    ] {
        for n in NESTINGS {
            scripts.push(format!("x={}1{}", open.repeat(n), close.repeat(n)));
        }
    }
    fit_a_small_stack(scripts);
}

/// Every kind of statement, declaration and binding pattern nesting.
#[test]
fn statement_nesting_fits_a_small_stack() {
    let mut scripts = Vec::new();
    for (open, close) in [
        ("{", "}"),
        ("if(a)", ""),
        ("if(a);else{", "}"),
        ("while(a)", ""),
        ("do ", " while(a)"),
        ("for(;;)", ""),
        ("for(a in a)", ""),
        ("for(a of a)", ""),
        ("with(a)", ""),
        ("a:", ""),
        ("try{", "}catch(e){}"),
        ("try{}catch(e){", "}"),
        ("try{}finally{", "}"),
        ("switch(a){case 1:", "}"),
        ("function a(){", "}"),
        ("function*a(){", "}"),
        ("async function a(){", "}"),
        ("class A{m(){", "}}"),
        ("class A{static{", "}}"),
        ("class A{constructor(){", "}}"),
        ("if(a)function b(){", "}"),
        ("a:function b(){", "}"),
    ] {
        for n in NESTINGS {
            scripts.push(format!("{}a;{}", open.repeat(n), close.repeat(n)));
        }
    }
    for (head, open, close) in [
        ("let ", "[", "]"),
        ("let ", "[...", "]"),
        ("let ", "{a:", "}"),
        ("var ", "[", "]"),
        ("var ", "{a:", "}"),
        ("for(let ", "[", "]"),
        ("for(const ", "{a:", "}"),
    ] {
        for n in NESTINGS {
            scripts.push(format!("{head}{}a{}=b;", open.repeat(n), close.repeat(n)));
        }
    }
    for n in NESTINGS {
        scripts.push(format!("function f({}a{}){{}}", "[".repeat(n), "]".repeat(n)));
        scripts.push(format!("try{{}}catch({}a{}){{}}", "[".repeat(n), "]".repeat(n)));
    }
    fit_a_small_stack(scripts);
}

/// Chains are trees as tall as they are long, and every pass after the parser recurses on them.
#[test]
fn chains_fit_a_small_stack() {
    let mut scripts = Vec::new();
    for link in ["+1", ",1", "||1", "&&1", "??1", "<1", "|1", "*1", "==1", "<<1", " in a", ".b", "[0]", "()", "``", "?.b"] {
        for n in [100, 4000, 20_000] {
            let chain = link.repeat(n);
            scripts.push(format!("x=(a{chain})"));
            // What a pattern or an arrow function's cover grammar does with a tall tree.
            scripts.push(format!("(a{chain})=>0"));
            scripts.push(format!("[a{chain}]=b"));
            scripts.push(format!("a{chain}=1"));
            scripts.push(format!("a{chain}++"));
            scripts.push(format!("for(a{chain} of b);"));
        }
    }
    for n in [100, 4000, 20_000] {
        scripts.push(format!("{}a", "if(a);else ".repeat(n)));
    }
    fit_a_small_stack(scripts);
}

/// What the limits leave allowed stays quick when it is deep and wide at once: every level of these
/// walks everything under it, so this is about the worst the limits leave, a second or so here.
#[test]
fn deep_and_wide_scripts_stay_quick() {
    const FILL: usize = 256 * 1024;
    let most = |unit: &str, around: usize| unit.repeat(FILL.saturating_sub(around) / unit.len());
    let compiled = |script: String| {
        assert!(script.len() <= FILL, "{} bytes", script.len());
        in_time(move || {
            let mut context = Context::default();
            match Script::parse(Source::from_bytes(&script), None, &mut context) {
                Ok(parsed) => parsed.codeblock(&mut context).map(|_| ()).map_err(|e| e.to_string()),
                Err(e) => Err(e.to_string()),
            }
        })
    };
    assert_eq!(compiled(format!("{}[{}a]", "a=>".repeat(200), most("a,", 700))), Ok(()));
    assert_eq!(compiled(format!("{}{{{}}}", "for(;;)".repeat(250), most("a;", 1800))), Ok(()));
    assert_eq!(compiled(format!("{}{{{}}}", "with(a)".repeat(250), most("a;", 1800))), Ok(()));
    assert_eq!(compiled(format!("{}{}{}{}", "{".repeat(250), "var a;".repeat(2000), most("a;", 12_600), "}".repeat(250))), Ok(()));
    assert_eq!(compiled(format!("x={}[{}a]", "a+".repeat(4090), most("a,", 8200))), Ok(()));
    assert_eq!(compiled(format!("x=[{}a]{}", most("a,", 8200), "+a".repeat(4090))), Ok(()));
    assert_eq!(compiled(format!("{}{{{}}}", "if(0);else ".repeat(4090), most("a;", 45_000))), Ok(()));
    assert_eq!(compiled(format!("x={}[{}a]{}", "(".repeat(60), most("a,", 200), ")".repeat(60))), Ok(()));
    let functions: String = (0..12).map(|f| format!("function f{f}(){{var {}z}}f{f}();", numbered("a", ",", 2047))).collect();
    assert_eq!(compiled(functions), Ok(()));
}
