//! Minimal user-agent stylesheet approximating browser defaults.

use std::sync::OnceLock;

use super::parser::{Stylesheet, parse_stylesheet};

pub const UA_CSS: &str = r#"
html { display: block; margin: 0; padding: 0; }
body { display: block; margin: 8px; font-family: sans-serif; }

head, title, meta, link, style, script, base, template, noscript, datalist { display: none; }
[hidden] { display: none; }
input[type="hidden"] { display: none; }

div, p, section, article, header, footer, main, nav, aside, address, blockquote,
figure, figcaption, form, fieldset, dl, dt, dd, ul, ol, menu, dir, center,
details, summary, hgroup, hr, pre, legend, iframe, video, canvas, object,
embed, svg {
    display: block;
}

p { margin-top: 1em; margin-bottom: 1em; }
h1 { display: block; font-size: 2em; margin: 0.67em 0; font-weight: bold; }
h2 { display: block; font-size: 1.5em; margin: 0.83em 0; font-weight: bold; }
h3 { display: block; font-size: 1.17em; margin: 1em 0; font-weight: bold; }
h4 { display: block; font-size: 1em; margin: 1.33em 0; font-weight: bold; }
h5 { display: block; font-size: 0.83em; margin: 1.67em 0; font-weight: bold; }
h6 { display: block; font-size: 0.67em; margin: 2.33em 0; font-weight: bold; }

ul, ol { display: block; margin: 1em 0; padding-left: 40px; }
ul { list-style-type: disc; }
ol { list-style-type: decimal; }
li { display: list-item; }
blockquote { margin: 1em 40px; }
dl { margin: 1em 0; }
dt { font-weight: bold; }
dd { margin-left: 40px; }

pre, code, kbd, samp, tt { font-family: monospace; }
pre { display: block; margin: 1em 0; white-space: pre; }

a { color: #0000ee; text-decoration: underline; }
b, strong { font-weight: bold; }
i, em, cite, var, dfn { font-style: italic; }
small { font-size: 0.83em; }
big { font-size: 1.17em; }
u { text-decoration: underline; }
s, strike, del { text-decoration: line-through; }
center { text-align: center; }

img { display: inline-block; }
input, button, select, textarea { display: inline-block; padding: 1px 3px; border: 1px solid #767676; background-color: #ffffff; font-size: 13px; }
textarea { white-space: pre-wrap; }
button { padding: 2px 6px; }
hr { display: block; height: 0; border: 0; border-top: 1px solid #cccccc; margin: 0.5em 0; }

table { display: table; border-spacing: 2px; }
tr { display: table-row; }
thead, tbody, tfoot { display: table-row-group; }
td, th { display: table-cell; padding: 1px; }
caption { display: block; }
colgroup, col { display: none; }
th { font-weight: bold; }
fieldset { display: block; margin: 0 2px; padding: 0.35em 0.75em 0.625em; border: 1px solid #cccccc; }
legend { padding: 0 2px; }

/* MathML: text-oriented fallback rendering (no full math layout). */
math, mrow, mfrac, msqrt, mroot, mtable, mtr, mtd, mstyle, merror,
mpadded, mphantom, menclose, semantics { display: block; }
math { margin: 0.2em 0; font-family: serif; }
mi, mo, mn, ms, mtext { display: inline; font-family: serif; }
mo { padding: 0 0.15em; }
mfrac, msqrt, mroot, msub, msup, msubsup, munder, mover, munderover {
    display: inline-block;
}
annotation, annotation-xml { display: none; }
"#;

pub fn user_agent_stylesheet() -> &'static Stylesheet {
    static UA: OnceLock<Stylesheet> = OnceLock::new();
    UA.get_or_init(|| parse_stylesheet(UA_CSS))
}
