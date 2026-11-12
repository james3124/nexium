//! Phase 3 observer/event gap coverage: `characterData` and `attributeFilter`
//! mutations, `IntersectionObserver`, `ResizeObserver`, passive listeners,
//! `Event.isTrusted` and `Blob.arrayBuffer()`.

use nexium::engine::css::Viewport;
use nexium::{Browser, Config, PageSession};
use serde_json::json;

fn browser() -> Browser {
    Browser::new(Config::default())
}

fn session_for(html: &str) -> PageSession {
    let page = browser()
        .load_html("https://js.test/", html)
        .expect("page loads");
    PageSession::new(
        page,
        Viewport {
            width: 800.0,
            height: 600.0,
        },
    )
}

#[test]
fn mutation_observer_reports_character_data_with_old_value() {
    let mut session = session_for(
        r##"<div id="t">hello</div><script>
  var seen = [];
  var target = document.getElementById("t");
  var observer = new MutationObserver(function (records) {
    for (var i = 0; i < records.length; i++) {
      var record = records[i];
      if (record.type === "characterData") {
        seen.push(record.oldValue + ">" + record.target.textContent);
      }
    }
  });
  observer.observe(target, {
    childList: true,
    characterData: true,
    characterDataOldValue: true,
    subtree: true,
  });
  target.textContent = "world";
</script>"##,
    );
    let seen = session.evaluate("seen").expect("evaluate");
    assert_eq!(seen, json!(["hello>world"]));
}

#[test]
fn mutation_observer_character_data_without_old_value_flag_omits_it() {
    let mut session = session_for(
        r##"<div id="t">one</div><script>
  var rows = [];
  var target = document.getElementById("t");
  var observer = new MutationObserver(function (records) {
    for (var i = 0; i < records.length; i++) {
      if (records[i].type === "characterData") {
        rows.push(String(records[i].oldValue));
      }
    }
  });
  observer.observe(target, { characterData: true, subtree: true });
  target.textContent = "two";
</script>"##,
    );
    let rows = session.evaluate("rows").expect("evaluate");
    assert_eq!(rows, json!(["undefined"]));
}

#[test]
fn mutation_observer_attribute_filter_limits_records() {
    let mut session = session_for(
        r##"<div id="t"></div><script>
  var names = [];
  var target = document.getElementById("t");
  var observer = new MutationObserver(function (records) {
    for (var i = 0; i < records.length; i++) {
      names.push(records[i].attributeName);
    }
  });
  observer.observe(target, { attributes: true, attributeFilter: ["data-keep"] });
  target.setAttribute("data-skip", "1");
  target.setAttribute("data-keep", "2");
  target.setAttribute("title", "three");
  target.className = "late";
</script>"##,
    );
    let names = session.evaluate("names").expect("evaluate");
    assert_eq!(names, json!(["data-keep"]));
}

#[test]
fn intersection_observer_delivers_initial_entry() {
    let mut session = session_for(
        r##"<div id="box" style="width:200px;height:100px"></div><script>
  var result = null;
  var box = document.getElementById("box");
  var observer = new IntersectionObserver(function (entries, self) {
    var entry = entries[0];
    result = {
      count: entries.length,
      isIntersecting: entry.isIntersecting,
      ratio: entry.intersectionRatio,
      width: entry.boundingClientRect.width,
      height: entry.boundingClientRect.height,
      rootWidth: entry.rootBounds.width,
      rootHeight: entry.rootBounds.height,
      sameTarget: entry.target === box,
      sameObserver: self === observer,
      pending: self.takeRecords().length
    };
  }, { root: null, rootMargin: "0px", threshold: 0.5 });
  observer.observe(box);
</script>"##,
    );
    let result = session.evaluate("result").expect("evaluate");
    assert_eq!(result["count"], json!(1.0));
    assert_eq!(result["isIntersecting"], json!(true));
    assert_eq!(result["ratio"], json!(1.0));
    assert_eq!(result["width"], json!(200.0));
    assert_eq!(result["height"], json!(100.0));
    assert_eq!(result["rootWidth"], json!(1280.0));
    assert_eq!(result["rootHeight"], json!(720.0));
    assert_eq!(result["sameTarget"], json!(true));
    assert_eq!(result["sameObserver"], json!(true));
    assert_eq!(result["pending"], json!(0.0));
}

#[test]
fn intersection_observer_honours_root_margin_and_unobserve() {
    let mut session = session_for(
        r##"<div id="spacer" style="height:900px"></div>
<div id="box" style="position:absolute;top:900px;left:0;width:200px;height:100px"></div>
<script>
  var hits = [];
  var box = document.getElementById("box");
  var observer = new IntersectionObserver(function (entries) {
    hits.push(entries[0].isIntersecting);
  }, { threshold: [0, 0.25, 1] });
  observer.observe(box);
  observer.observe(box);
</script>"##,
    );
    let hits = session.evaluate("hits").expect("evaluate");
    assert_eq!(hits, json!([false]), "box below the 600px viewport");

    session
        .evaluate(
            r##"var hits2 = [];
var observer2 = new IntersectionObserver(function (entries) {
  hits2.push(entries[0].isIntersecting);
}, { rootMargin: "0px 0px 200px 0px" });
observer2.observe(document.getElementById("box"));"##,
        )
        .expect("evaluate");
    let after_margin = session.evaluate("hits2").expect("evaluate");
    assert_eq!(after_margin, json!([true]), "rootMargin expands the root");

    session
        .evaluate(
            r##"observer2.unobserve(document.getElementById("box"));
document.getElementById("box").style.top = "1300px";"##,
        )
        .expect("evaluate");
    let after_unobserve = session
        .evaluate("observer2.takeRecords().length")
        .expect("evaluate");
    assert_eq!(after_unobserve, json!(0.0));
}

#[test]
fn resize_observer_delivers_initial_and_changed_sizes() {
    let mut session = session_for(
        r##"<div id="box" style="width:200px;height:100px"></div><script>
  var sizes = [];
  var box = document.getElementById("box");
  var observer = new ResizeObserver(function (entries) {
    for (var i = 0; i < entries.length; i++) {
      sizes.push(entries[i].contentRect.width + "x" + entries[i].contentRect.height);
    }
  });
  observer.observe(box);
</script>"##,
    );
    let sizes = session.evaluate("sizes").expect("evaluate");
    assert_eq!(sizes, json!(["200x100"]));

    session
        .evaluate("box.style.width = '320px';")
        .expect("evaluate");
    let sizes = session.evaluate("sizes").expect("evaluate");
    assert_eq!(sizes, json!(["200x100", "320x100"]));

    session
        .evaluate(
            r##"observer.disconnect();
box.style.height = "50px";
sizes.length"##,
        )
        .expect("evaluate");
    let sizes = session.evaluate("sizes").expect("evaluate");
    assert_eq!(sizes, json!(["200x100", "320x100"]));
}

#[test]
fn passive_listener_ignores_prevent_default() {
    let mut session = session_for(
        r##"<div id="passive">a</div><div id="active">b</div><script>
  var passiveSeen = [];
  var activeSeen = [];
  var passiveTarget = document.getElementById("passive");
  passiveTarget.addEventListener("click", function (event) {
    event.preventDefault();
    passiveSeen.push(event.defaultPrevented);
  }, { passive: true });
  passiveTarget.addEventListener("click", function (event) {
    passiveSeen.push(event.defaultPrevented);
  });
  passiveTarget.click();

  var activeTarget = document.getElementById("active");
  activeTarget.addEventListener("click", function (event) {
    event.preventDefault();
    activeSeen.push(event.defaultPrevented);
  });
  activeTarget.click();
</script>"##,
    );
    let passive = session.evaluate("passiveSeen").expect("evaluate");
    assert_eq!(passive, json!([false, false]));
    let active = session.evaluate("activeSeen").expect("evaluate");
    assert_eq!(active, json!([true]));
}

#[test]
fn event_is_trusted_is_false_for_script_events_and_read_only() {
    let mut session = session_for(
        r##"<div id="t">x</div><script>
  var trusted = [];
  var target = document.getElementById("t");
  target.addEventListener("click", function (event) {
    trusted.push(event.isTrusted);
    event.isTrusted = true;
    trusted.push(event.isTrusted);
  });
  target.click();
</script>"##,
    );
    let trusted = session.evaluate("trusted").expect("evaluate");
    assert_eq!(trusted, json!([false, false]));
}

#[test]
fn blob_array_buffer_round_trips() {
    let mut session = session_for(
        r##"<script>
  new Blob(["hi", "!"], { type: "text/plain" }).arrayBuffer().then(function (buffer) {
    var view = new Uint8Array(buffer);
    var round = "";
    for (var i = 0; i < view.length; i++) {
      round += String.fromCharCode(view[i]);
    }
    window.__arrayBuffer = {
      byteLength: buffer.byteLength,
      isArrayBuffer: buffer instanceof ArrayBuffer,
      text: round
    };
  });
</script>"##,
    );
    let result = session.evaluate("__arrayBuffer").expect("evaluate");
    assert_eq!(result["byteLength"], json!(3.0));
    assert_eq!(result["isArrayBuffer"], json!(true));
    assert_eq!(result["text"], json!("hi!"));
}
