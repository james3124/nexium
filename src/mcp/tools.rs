//! MCP tool definitions and dispatch into the shared API service.

use std::sync::Arc;

use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value, json};

use crate::api::service::{
    ApiError, ApiResult, ApiService, AuthContext, ClickOptions, CreateOptions, EvaluateOptions,
    HoverOptions, PressOptions, ScreenshotOptions, ScrollOptions, SessionOptions, SnapshotOptions,
    TabsCreateOptions, TabsTargetOptions, TypeOptions, VisionActOptions, VisionAnalyzeOptions,
    VisionClickOptions, VisionCompareOptions, VisionFindOptions, VisionOptions, VisionTextOptions,
    WaitOptions,
};

#[derive(Deserialize)]
struct NavigateArgs {
    url: String,
}

#[derive(Deserialize)]
struct CreateArgs {
    #[serde(flatten)]
    options: CreateOptions,
}

pub async fn call(
    service: &Arc<ApiService>,
    auth: &AuthContext,
    name: &str,
    arguments: Value,
) -> ApiResult<Value> {
    let session = session_arg(&arguments);
    match name {
        "browser_create" => {
            let args: CreateArgs = parse(&arguments)?;
            service.create(auth, args.options).await
        }
        "browser_close" => {
            let id = match session {
                Some(id) => id.to_owned(),
                None => service.resolve(auth, None).await?.id.clone(),
            };
            service.close(auth, &id).await
        }
        "browser_navigate" => {
            let args: NavigateArgs = parse(&arguments)?;
            service.navigate(auth, session, &args.url).await
        }
        "browser_click" => {
            let args: ClickOptions = parse(&arguments)?;
            service.click(auth, session, args).await
        }
        "browser_type" => {
            let args: TypeOptions = parse(&arguments)?;
            service.type_text(auth, session, args).await
        }
        "browser_press" => {
            let args: PressOptions = parse(&arguments)?;
            service.press(auth, session, args).await
        }
        "browser_scroll" => {
            let args: ScrollOptions = parse(&arguments)?;
            service.scroll(auth, session, args).await
        }
        "browser_hover" => {
            let args: HoverOptions = parse(&arguments)?;
            service.hover(auth, session, args).await
        }
        "browser_screenshot" => {
            let args: ScreenshotOptions = parse(&arguments)?;
            service.screenshot(auth, session, args).await
        }
        "browser_snapshot" => {
            let args: SnapshotOptions = parse(&arguments)?;
            service.snapshot(auth, session, args).await
        }
        "browser_extract" => service.extract(auth, session).await,
        "browser_wait" => {
            let args: WaitOptions = parse(&arguments)?;
            service.wait(auth, session, args).await
        }
        "browser_tabs" => {
            let _: TabsCreateOptions = parse(&arguments)?;
            service.tabs(auth, session).await
        }
        "browser_switch_tab" => {
            let args: TabsTargetOptions = parse(&arguments)?;
            service.tabs_switch(auth, session, args).await
        }
        "browser_session_save" => {
            let args: SessionOptions = parse(&arguments)?;
            service.session_save(auth, session, args).await
        }
        "browser_session_restore" => {
            let args: SessionOptions = parse(&arguments)?;
            service.session_restore(auth, session, args).await
        }
        "browser_session_list_persisted" => service.persisted_sessions(),
        "browser_session_restore_persisted" => {
            service.restore_persisted_sessions(Some(&auth.agent)).await
        }
        "browser_evaluate" => {
            let args: EvaluateOptions = parse(&arguments)?;
            service.evaluate(auth, session, args).await
        }
        "vision_snapshot" | "snapshot" => {
            let args: VisionOptions = parse(&arguments)?;
            service.vision_snapshot(auth, session, args).await
        }
        "vision_analyze" => {
            let args: VisionAnalyzeOptions = parse(&arguments)?;
            service.vision_analyze(auth, session, args).await
        }
        "vision_extract_text" => {
            let args: VisionTextOptions = parse(&arguments)?;
            service.vision_extract_text(auth, session, args).await
        }
        "vision_find_element" => {
            let args: VisionFindOptions = parse(&arguments)?;
            service.vision_find_element(auth, session, args).await
        }
        "vision_click_target" => {
            let args: VisionClickOptions = parse(&arguments)?;
            service.vision_click_target(auth, session, args).await
        }
        "vision_act" => {
            let args: VisionActOptions = parse(&arguments)?;
            service.vision_act(auth, session, args).await
        }
        "vision_compare" => {
            let args: VisionCompareOptions = parse(&arguments)?;
            service.vision_compare(auth, session, args).await
        }
        "browser_network" => service.network(auth, session).await,
        "browser_status" => service.status(auth, session).await,
        other => Err(ApiError::not_found(format!("unknown tool: {other}"))),
    }
}

fn parse<T: DeserializeOwned>(arguments: &Value) -> ApiResult<T> {
    serde_json::from_value(arguments.clone())
        .map_err(|error| ApiError::bad_request(format!("invalid arguments: {error}")))
}

fn session_arg(arguments: &Value) -> Option<&str> {
    arguments
        .get("session_id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
}

fn tool(name: &str, description: &str, properties: Map<String, Value>) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": {
            "type": "object",
            "properties": properties,
            "additionalProperties": false,
        }
    })
}

fn required(mut tool: Value, names: &[&str]) -> Value {
    if let Some(schema) = tool.get_mut("inputSchema").and_then(Value::as_object_mut) {
        schema.insert("required".to_owned(), json!(names));
    }
    tool
}

fn session_property(properties: &mut Map<String, Value>) {
    properties.insert(
        "session_id".to_owned(),
        json!({
            "type": "string",
            "description": "Browser session id. Omit to use this agent's default session."
        }),
    );
}

fn point_properties(properties: &mut Map<String, Value>) {
    properties.insert(
        "x".to_owned(),
        json!({ "type": "number", "description": "Viewport x coordinate." }),
    );
    properties.insert(
        "y".to_owned(),
        json!({ "type": "number", "description": "Viewport y coordinate." }),
    );
}

fn selector_property(properties: &mut Map<String, Value>) {
    properties.insert(
        "selector".to_owned(),
        json!({
            "type": "string",
            "description": "CSS selector; overrides coordinates and is scrolled into view."
        }),
    );
}

fn region_property(properties: &mut Map<String, Value>) {
    properties.insert(
        "region".to_owned(),
        json!({
            "type": "object",
            "description": "Viewport-space region; only elements or text intersecting it are considered.",
            "properties": {
                "x": { "type": "number" },
                "y": { "type": "number" },
                "width": { "type": "number" },
                "height": { "type": "number" },
                "scale": { "type": "number", "description": "Output scale factor (default 1)." }
            },
            "required": ["width", "height"]
        }),
    );
}

pub fn tool_definitions() -> Vec<Value> {
    let mut tools = Vec::new();

    let mut properties = Map::new();
    properties.insert(
        "url".to_owned(),
        json!({
            "type": "string",
            "description": "Initial URL to load. Must be a concrete, valid absolute URL; omit to create a blank session. Never pass a malformed or placeholder URL."
        }),
    );
    properties.insert(
        "profile".to_owned(),
        json!({ "type": "string", "description": "Persistent profile name." }),
    );
    properties.insert(
        "viewport".to_owned(),
        json!({
            "type": "object",
            "properties": {
                "width": { "type": "integer", "minimum": 1 },
                "height": { "type": "integer", "minimum": 1 }
            },
            "required": ["width", "height"]
        }),
    );
    properties.insert(
        "user_agent".to_owned(),
        json!({ "type": "string", "description": "User-Agent override." }),
    );
    tools.push(tool(
        "browser_create",
        "Create an isolated browser session. Returns a session_id used by other browser tools.",
        properties,
    ));

    let mut properties = Map::new();
    session_property(&mut properties);
    tools.push(tool(
        "browser_close",
        "Close a browser session and release its tabs, storage handles and memory.",
        properties,
    ));

    let mut properties = Map::new();
    session_property(&mut properties);
    properties.insert(
        "url".to_owned(),
        json!({
            "type": "string",
            "description": "Absolute http(s) URL to load. Must be concrete and valid; never pass a malformed, placeholder, or relative value."
        }),
    );
    tools.push(required(
        tool(
            "browser_navigate",
            "Navigate the active tab to a concrete, absolute http(s) URL and return page metadata. Call only when a complete, valid URL was provided; if the URL is missing, malformed, or a placeholder, do not call this tool—explain the problem and ask for a valid URL instead.",
            properties,
        ),
        &["url"],
    ));

    let mut properties = Map::new();
    session_property(&mut properties);
    point_properties(&mut properties);
    selector_property(&mut properties);
    properties.insert(
        "button".to_owned(),
        json!({ "type": "string", "enum": ["left", "middle", "right"] }),
    );
    tools.push(tool(
        "browser_click",
        "Click the element at viewport coordinates or matching a CSS selector.",
        properties,
    ));

    let mut properties = Map::new();
    session_property(&mut properties);
    properties.insert(
        "text".to_owned(),
        json!({ "type": "string", "description": "Text to type into the focused input." }),
    );
    selector_property(&mut properties);
    tools.push(required(
        tool(
            "browser_type",
            "Type text into the focused input/textarea, optionally focusing an element first.",
            properties,
        ),
        &["text"],
    ));

    let mut properties = Map::new();
    session_property(&mut properties);
    properties.insert(
        "key".to_owned(),
        json!({ "type": "string", "description": "Key name, e.g. Enter, Backspace, ArrowDown, a." }),
    );
    tools.push(required(
        tool(
            "browser_press",
            "Press a keyboard key on the focused element.",
            properties,
        ),
        &["key"],
    ));

    let mut properties = Map::new();
    session_property(&mut properties);
    properties.insert(
        "dx".to_owned(),
        json!({ "type": "number", "description": "Horizontal scroll delta." }),
    );
    properties.insert(
        "dy".to_owned(),
        json!({ "type": "number", "description": "Vertical scroll delta." }),
    );
    selector_property(&mut properties);
    tools.push(tool(
        "browser_scroll",
        "Scroll the viewport or scroll a selector into view.",
        properties,
    ));

    let mut properties = Map::new();
    session_property(&mut properties);
    point_properties(&mut properties);
    selector_property(&mut properties);
    tools.push(tool(
        "browser_hover",
        "Move the virtual mouse over coordinates or a CSS selector.",
        properties,
    ));

    let mut properties = Map::new();
    session_property(&mut properties);
    properties.insert(
        "format".to_owned(),
        json!({ "type": "string", "enum": ["png", "jpeg", "jpg", "webp"] }),
    );
    properties.insert(
        "png_compression".to_owned(),
        json!({ "type": "string", "enum": ["fast", "default", "best", "uncompressed"] }),
    );
    properties.insert(
        "png_filter".to_owned(),
        json!({ "type": "string", "enum": ["none", "sub", "up", "avg", "paeth", "adaptive"] }),
    );
    properties.insert(
        "quality".to_owned(),
        json!({
            "type": "integer", "minimum": 1, "maximum": 100,
            "description": "JPEG quality (default 90); WebP output is lossless."
        }),
    );
    properties.insert(
        "full_page".to_owned(),
        json!({ "type": "boolean", "description": "Capture the full document instead of the viewport." }),
    );
    properties.insert(
        "element".to_owned(),
        json!({ "type": "string", "description": "CSS selector for an element crop." }),
    );
    properties.insert(
        "clip".to_owned(),
        json!({
            "type": "object",
            "description": "Crop region in CSS pixels, relative to clip_origin.",
            "properties": {
                "x": { "type": "number" },
                "y": { "type": "number" },
                "width": { "type": "number" },
                "height": { "type": "number" },
                "scale": { "type": "number", "description": "Output scale factor (default 1)." }
            },
            "required": ["width", "height"]
        }),
    );
    properties.insert(
        "clip_origin".to_owned(),
        json!({
            "type": "string", "enum": ["document", "viewport"],
            "description": "Coordinate space for clip (default document)."
        }),
    );
    properties.insert(
        "save".to_owned(),
        json!({
            "type": "boolean",
            "description": "Persist the encoded image to the artifacts directory and return its path/URL (default false)."
        }),
    );
    properties.insert(
        "artifact_name".to_owned(),
        json!({
            "type": "string",
            "description": "Optional safe file name (no path separators) for the persisted artifact; implies save."
        }),
    );
    tools.push(tool(
        "browser_screenshot",
        "Capture a PNG/JPEG/WebP screenshot and return base64 image data; optionally persist it to disk.",
        properties,
    ));

    let mut properties = Map::new();
    session_property(&mut properties);
    properties.insert(
        "include_accessibility".to_owned(),
        json!({ "type": "boolean", "description": "Include the accessibility tree (default true)." }),
    );
    tools.push(tool(
        "browser_snapshot",
        "Return a structured DOM snapshot, optionally with the accessibility tree.",
        properties,
    ));

    let mut properties = Map::new();
    session_property(&mut properties);
    tools.push(tool(
        "browser_extract",
        "Extract page text, headings, links, form fields and buttons.",
        properties,
    ));

    let mut properties = Map::new();
    session_property(&mut properties);
    properties.insert(
        "ms".to_owned(),
        json!({ "type": "integer", "minimum": 0, "maximum": 30000 }),
    );
    properties.insert(
        "selector".to_owned(),
        json!({ "type": "string", "description": "Wait until this selector exists." }),
    );
    properties.insert(
        "timeout_ms".to_owned(),
        json!({ "type": "integer", "minimum": 0, "maximum": 30000 }),
    );
    tools.push(tool(
        "browser_wait",
        "Wait for a duration or until a CSS selector appears.",
        properties,
    ));

    let mut properties = Map::new();
    session_property(&mut properties);
    tools.push(tool(
        "browser_tabs",
        "List tabs with ids, URLs, titles and active state.",
        properties,
    ));

    let mut properties = Map::new();
    session_property(&mut properties);
    properties.insert(
        "tab_id".to_owned(),
        json!({ "type": "integer", "minimum": 1, "description": "Tab id from browser_tabs." }),
    );
    tools.push(tool(
        "browser_switch_tab",
        "Switch the active tab.",
        properties,
    ));

    let mut properties = Map::new();
    session_property(&mut properties);
    properties.insert(
        "name".to_owned(),
        json!({ "type": "string", "description": "Session snapshot name." }),
    );
    tools.push(tool(
        "browser_session_save",
        "Save tabs, history and session storage to disk.",
        properties,
    ));

    let mut properties = Map::new();
    session_property(&mut properties);
    properties.insert(
        "name".to_owned(),
        json!({ "type": "string", "description": "Session snapshot name." }),
    );
    tools.push(tool(
        "browser_session_restore",
        "Restore a previously saved session snapshot.",
        properties,
    ));

    tools.push(tool(
        "browser_session_list_persisted",
        "List API sessions persisted to disk (available after a process restart).",
        Map::new(),
    ));
    tools.push(tool(
        "browser_session_restore_persisted",
        "Rebuild this agent's persisted sessions into the live registry after a restart.",
        Map::new(),
    ));

    let mut properties = Map::new();
    session_property(&mut properties);
    properties.insert(
        "script".to_owned(),
        json!({
            "type": "string",
            "description": "JavaScript expression to evaluate in the page. Required and must not be empty."
        }),
    );
    tools.push(required(
        tool(
            "browser_evaluate",
            "Evaluate JavaScript in the live page runtime and return a JSON value.",
            properties,
        ),
        &["script"],
    ));

    let mut properties = Map::new();
    session_property(&mut properties);
    tools.push(tool(
        "browser_network",
        "Return recorded network requests with status, type and timing.",
        properties,
    ));

    let mut properties = Map::new();
    session_property(&mut properties);
    tools.push(tool(
        "browser_status",
        "Return session status, page metadata, tabs and scroll state.",
        properties,
    ));

    let mut properties = Map::new();
    session_property(&mut properties);
    properties.insert(
        "full_page".to_owned(),
        json!({ "type": "boolean", "description": "Render the whole document instead of the viewport (default false)." }),
    );
    properties.insert(
        "format".to_owned(),
        json!({ "type": "string", "enum": ["png", "jpeg", "jpg", "webp"] }),
    );
    properties.insert(
        "quality".to_owned(),
        json!({
            "type": "integer", "minimum": 1, "maximum": 100,
            "description": "JPEG quality (default 90); ignored for PNG/WebP."
        }),
    );
    properties.insert(
        "include_visual".to_owned(),
        json!({ "type": "boolean", "description": "Include layout-grounded elements and text runs (default true)." }),
    );
    tools.push(tool(
        "vision_snapshot",
        "Capture a screenshot plus a layout-grounded visual index (elements, text runs, scroll and viewport).",
        properties,
    ));

    let mut properties = Map::new();
    session_property(&mut properties);
    properties.insert(
        "full_page".to_owned(),
        json!({ "type": "boolean", "description": "Analyze the whole document instead of the viewport (default false)." }),
    );
    region_property(&mut properties);
    tools.push(tool(
        "vision_analyze",
        "Analyze the current frame: elements, text runs, clickable regions, viewport and a PNG image.",
        properties,
    ));

    let mut properties = Map::new();
    session_property(&mut properties);
    properties.insert(
        "query".to_owned(),
        json!({ "type": "string", "description": "Optional case-insensitive phrase to locate on screen." }),
    );
    region_property(&mut properties);
    tools.push(tool(
        "vision_extract_text",
        "Extract painted text in reading order with boxes, optionally locating a phrase.",
        properties,
    ));

    let mut properties = Map::new();
    session_property(&mut properties);
    properties.insert(
        "description".to_owned(),
        json!({ "type": "string", "description": "Natural-language description of the element to locate (required, must not be empty)." }),
    );
    properties.insert(
        "top_k".to_owned(),
        json!({ "type": "integer", "minimum": 1, "description": "Maximum number of ranked matches to return." }),
    );
    region_property(&mut properties);
    tools.push(required(
        tool(
            "vision_find_element",
            "Rank visual elements by a natural-language description and explain each match with signals.",
            properties,
        ),
        &["description"],
    ));

    let mut properties = Map::new();
    session_property(&mut properties);
    properties.insert(
        "description".to_owned(),
        json!({ "type": "string", "description": "Natural-language description of the element to click (required, must not be empty)." }),
    );
    properties.insert(
        "execute".to_owned(),
        json!({ "type": "boolean", "description": "Actually dispatch the click at the matched coordinates (default false)." }),
    );
    properties.insert(
        "verify".to_owned(),
        json!({ "type": "boolean", "description": "Capture before/after and report visual and text differences (default false)." }),
    );
    properties.insert(
        "button".to_owned(),
        json!({ "type": "string", "enum": ["left", "middle", "right"], "description": "Mouse button used when execute is true." }),
    );
    tools.push(required(
        tool(
            "vision_click_target",
            "Resolve a description to viewport coordinates, optionally click it and verify the result.",
            properties,
        ),
        &["description"],
    ));

    let mut properties = Map::new();
    session_property(&mut properties);
    properties.insert(
        "description".to_owned(),
        json!({ "type": "string", "description": "Natural-language description of the element to act on (required, must not be empty)." }),
    );
    properties.insert(
        "action".to_owned(),
        json!({ "type": "string", "enum": ["click", "hover", "scroll_into_view"], "description": "Action performed at the located element (default click)." }),
    );
    properties.insert(
        "verify".to_owned(),
        json!({ "type": "boolean", "description": "Capture before/after and report visual and text changes (default true)." }),
    );
    properties.insert(
        "threshold".to_owned(),
        json!({ "type": "integer", "minimum": 0, "maximum": 255, "description": "Per-channel pixel delta considered a change." }),
    );
    properties.insert(
        "min_pixels".to_owned(),
        json!({ "type": "integer", "minimum": 0, "description": "Minimum changed pixels before a region is reported." }),
    );
    properties.insert(
        "locator".to_owned(),
        json!({ "type": "string", "enum": ["heuristic", "default"], "description": "Element locator strategy (default heuristic)." }),
    );
    properties.insert(
        "max_attempts".to_owned(),
        json!({ "type": "integer", "minimum": 1, "description": "Maximum locate/auto-scroll attempts (default 2)." }),
    );
    tools.push(required(
        tool(
            "vision_act",
            "Locate a described element, refuse ambiguous matches, scroll it into view, act and verify the result.",
            properties,
        ),
        &["description"],
    ));

    let mut properties = Map::new();
    session_property(&mut properties);
    properties.insert(
        "threshold".to_owned(),
        json!({ "type": "integer", "minimum": 0, "maximum": 255, "description": "Per-channel pixel delta considered a change." }),
    );
    properties.insert(
        "min_pixels".to_owned(),
        json!({ "type": "integer", "minimum": 0, "description": "Minimum changed pixels before a region is reported." }),
    );
    properties.insert(
        "save_artifacts".to_owned(),
        json!({ "type": "boolean", "description": "Persist both captures as PNG artifacts and return their paths/URLs." }),
    );
    tools.push(tool(
        "vision_compare",
        "Compare two captures around a caller-driven action: pixel ratio, changed regions and text diff.",
        properties,
    ));

    tools
}
