//! ES module graph resolution.
//!
//! This module implements a [`ModuleLoader`] on top of Boa's native module
//! support. Modules are resolved against the referring module URL (or the page
//! URL for entry points and classic scripts) and parsed into `boa_engine::Module`
//! instances that are cached by their resolved URL, so static imports,
//! re-exports (`export ... from`, `export * from`, `export * as ns from`) and
//! dynamic `import()` all share one evaluation per module.
//!
//! Supported specifiers:
//! - relative: `./foo.js`, `../lib/foo.js`
//! - absolute path: `/assets/foo.js`
//! - absolute URLs: `http://`, `https://` and `data:` URLs
//! - bare specifiers mapped by a `<script type="importmap">` entry, including
//!   trailing-slash prefix mappings
//!
//! Unmapped bare specifiers are rejected. `file:` and other schemes are
//! rejected.

use std::cell::RefCell;
use std::collections::HashMap;
use std::future::Future;
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;

use base64::Engine as _;
use boa_engine::builtins::promise::PromiseState;
use boa_engine::module::{ModuleLoader, ModuleRequest, Referrer};
use boa_engine::{Context, JsError, JsNativeError, JsObject, JsResult, JsString, Module, Source};
use url::Url;

use crate::engine::dom::Document;
use crate::networking::HttpClient;

use super::{fetch, js_string_value, value_to_display};

/// Bare specifier entries from the document's `<script type="importmap">`.
#[derive(Default)]
pub(crate) struct ImportMap {
    imports: HashMap<String, Url>,
    prefixes: Vec<(String, Url)>,
}

impl ImportMap {
    /// Read the first import map in the document. Invalid JSON or a missing
    /// `imports` object yields an empty map instead of an error.
    pub(crate) fn from_document(document: &Document, base_url: &Url) -> Self {
        for id in document.descendants(document.document_node()) {
            if document.tag_name(id) != Some("script") {
                continue;
            }
            let is_import_map = document
                .get_attribute(id, "type")
                .is_some_and(|value| value.trim().eq_ignore_ascii_case("importmap"));
            if is_import_map {
                return Self::parse(&document.text_content(id), base_url);
            }
        }
        Self::default()
    }

    fn parse(json: &str, base_url: &Url) -> Self {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(json) else {
            return Self::default();
        };
        let Some(imports) = value.get("imports").and_then(|imports| imports.as_object()) else {
            return Self::default();
        };
        let mut map = Self::default();
        for (specifier, target) in imports {
            let Some(target) = target.as_str() else {
                continue;
            };
            let target = Url::parse(target).or_else(|_| base_url.join(target)).ok();
            let Some(target) = target else {
                continue;
            };
            if specifier.ends_with('/') {
                map.prefixes.push((specifier.clone(), target));
            } else {
                map.imports.insert(specifier.clone(), target);
            }
        }
        map.prefixes
            .sort_by_key(|entry| std::cmp::Reverse(entry.0.len()));
        map
    }

    fn resolve(&self, specifier: &str) -> Option<Url> {
        if let Some(target) = self.imports.get(specifier) {
            return Some(target.clone());
        }
        for (prefix, target) in &self.prefixes {
            if let Some(remainder) = specifier.strip_prefix(prefix.as_str()) {
                let mut spliced = target.as_str().to_owned();
                spliced.push_str(remainder);
                return Url::parse(&spliced).ok();
            }
        }
        None
    }
}

/// Resolves and caches modules for one JavaScript runtime.
pub(crate) struct ModuleRegistry {
    base_url: Url,
    client: Option<Arc<HttpClient>>,
    import_map: ImportMap,
    cache: RefCell<HashMap<String, Module>>,
}

impl ModuleRegistry {
    pub(crate) fn new(
        base_url: Url,
        client: Option<Arc<HttpClient>>,
        import_map: ImportMap,
    ) -> Rc<Self> {
        Rc::new(Self {
            base_url,
            client,
            import_map,
            cache: RefCell::new(HashMap::new()),
        })
    }

    /// Resolve a specifier against the referrer URL (or the page URL when the
    /// referrer has no address, e.g. classic scripts and entry points).
    fn resolve(&self, referrer: Option<&Path>, specifier: &str) -> JsResult<Url> {
        let specifier = specifier.trim();
        if specifier.is_empty() {
            return Err(type_error("empty module specifier"));
        }
        if let Ok(url) = Url::parse(specifier) {
            return supported_module_url(url, specifier);
        }
        let relative = specifier.starts_with('/')
            || specifier.starts_with("./")
            || specifier.starts_with("../")
            || specifier == "."
            || specifier == "..";
        if !relative {
            let mapped = self.import_map.resolve(specifier).ok_or_else(|| {
                type_error(format!(
                    "cannot resolve bare module specifier `{specifier}`"
                ))
            })?;
            return supported_module_url(mapped, specifier);
        }
        let base = referrer
            .and_then(|path| Url::parse(&path.to_string_lossy()).ok())
            .unwrap_or_else(|| self.base_url.clone());
        base.join(specifier)
            .or_else(|_| self.base_url.join(specifier))
            .map_err(|error| type_error(format!("cannot resolve `{specifier}`: {error}")))
    }

    /// Parse `code` as a module whose address is `url`.
    fn parse(&self, url: &Url, code: &str, context: &mut Context) -> JsResult<Module> {
        let path = Path::new(url.as_str());
        Module::parse(Source::from_bytes(code).with_path(path), None, context).map_err(|error| {
            JsNativeError::syntax()
                .with_message(format!("could not parse module `{url}`: {error}"))
                .into()
        })
    }

    /// Read the source text for a module URL.
    fn fetch_source(&self, url: &Url) -> JsResult<String> {
        fetch_module_source(url, self.client.clone())
    }

    /// Load a module (or return the cached instance for its resolved URL).
    fn load(
        &self,
        referrer: Option<&Path>,
        specifier: &str,
        context: &mut Context,
    ) -> JsResult<Module> {
        let url = self.resolve(referrer, specifier)?;
        let key = url.as_str().to_owned();
        if let Some(module) = self.cache.borrow().get(&key) {
            return Ok(module.clone());
        }
        let code = self.fetch_source(&url)?;
        let module = self.parse(&url, &code, context)?;
        self.cache.borrow_mut().insert(key, module.clone());
        Ok(module)
    }
}

impl ModuleLoader for ModuleRegistry {
    fn load_imported_module(
        self: Rc<Self>,
        referrer: Referrer,
        request: ModuleRequest,
        context: &RefCell<&mut Context>,
    ) -> impl Future<Output = JsResult<Module>> {
        let specifier = request.specifier().to_std_string_escaped();
        let result = {
            let mut context = context.borrow_mut();
            self.load(referrer.path(), &specifier, &mut context)
        };
        std::future::ready(result)
    }

    fn init_import_meta(
        self: Rc<Self>,
        import_meta: &JsObject,
        module: &Module,
        context: &mut Context,
    ) {
        // `import.meta.url` is the module's resolved address. Inline module
        // scripts are parsed with the document URL as their path.
        let url = module
            .path()
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.base_url.to_string());
        let _ = import_meta.create_data_property_or_throw(
            JsString::from("url"),
            js_string_value(&url),
            context,
        );
    }
}

/// Evaluate an inline `<script type="module">`, resolving imports against the
/// document URL (which is also exposed as `import.meta.url`).
pub(crate) fn evaluate_inline_module(code: &str, context: &mut Context) -> Option<String> {
    let Some(registry) = context.downcast_module_loader::<ModuleRegistry>() else {
        return evaluate_standalone(code, context);
    };
    let url = registry.base_url.clone();
    match registry.parse(&url, code, context) {
        Ok(module) => evaluate_module(&module, context),
        Err(error) => Some(error.to_string()),
    }
}

/// Evaluate an external `<script type="module" src=...>` from already fetched
/// source, resolving relative imports against the script URL.
pub(crate) fn evaluate_external_module(
    url: &Url,
    code: &str,
    context: &mut Context,
) -> Option<String> {
    let Some(registry) = context.downcast_module_loader::<ModuleRegistry>() else {
        return evaluate_standalone(code, context);
    };
    evaluate_with_source(&registry, url, code, context)
}

/// Fetch and evaluate a module by its resolved URL (used by module workers).
pub(crate) fn evaluate_module_url(url: &Url, context: &mut Context) -> Option<String> {
    let Some(registry) = context.downcast_module_loader::<ModuleRegistry>() else {
        return Some("module loader is not available".to_owned());
    };
    let code = match registry.fetch_source(url) {
        Ok(code) => code,
        Err(error) => return Some(error.to_string()),
    };
    evaluate_with_source(&registry, url, &code, context)
}

fn evaluate_with_source(
    registry: &ModuleRegistry,
    url: &Url,
    code: &str,
    context: &mut Context,
) -> Option<String> {
    // Cache before evaluating so that cycles and later imports reuse the
    // instance instead of parsing the module again.
    match registry.parse(url, code, context) {
        Ok(module) => {
            registry
                .cache
                .borrow_mut()
                .insert(url.as_str().to_owned(), module.clone());
            evaluate_module(&module, context)
        }
        Err(error) => Some(error.to_string()),
    }
}

fn evaluate_standalone(code: &str, context: &mut Context) -> Option<String> {
    match Module::parse(Source::from_bytes(code), None, context) {
        Ok(module) => evaluate_module(&module, context),
        Err(error) => Some(error.to_string()),
    }
}

fn evaluate_module(module: &Module, context: &mut Context) -> Option<String> {
    let promise = module.load_link_evaluate(context);
    let _ = context.run_jobs();
    // Boa supports top-level await: awaits settled by microtasks resolve while
    // `run_jobs` drains the queue. Awaits waiting on host timers stay pending
    // here and are completed by the page event loop after scripts finish.
    match promise.state() {
        PromiseState::Rejected(value) => Some(value_to_display(&value, context)),
        PromiseState::Fulfilled(_) | PromiseState::Pending => None,
    }
}

fn type_error(message: impl Into<String>) -> JsError {
    JsNativeError::typ().with_message(message.into()).into()
}

fn supported_module_url(url: Url, specifier: &str) -> JsResult<Url> {
    match url.scheme() {
        "http" | "https" | "data" => Ok(url),
        scheme => Err(type_error(format!(
            "unsupported module specifier scheme `{scheme}`: `{specifier}`"
        ))),
    }
}

/// Fetch module source for a supported URL scheme.
pub(crate) fn fetch_module_source(url: &Url, client: Option<Arc<HttpClient>>) -> JsResult<String> {
    match url.scheme() {
        "data" => decode_data_url(url),
        "http" | "https" => {
            let Some(client) = client else {
                return Err(type_error(format!(
                    "cannot load module `{url}`: network access is unavailable"
                )));
            };
            fetch::fetch_text_sync(url.clone(), Some(client))
                .map_err(|error| type_error(format!("failed to load module `{url}`: {error}")))
        }
        scheme => Err(type_error(format!(
            "unsupported module scheme `{scheme}` for `{url}`"
        ))),
    }
}

/// Decode a `data:` URL module body, honoring the `;base64` marker and
/// percent-encoded payloads.
fn decode_data_url(url: &Url) -> JsResult<String> {
    let raw = url.as_str();
    let raw = raw.split_once('#').map_or(raw, |(head, _)| head);
    let Some(rest) = raw.strip_prefix("data:") else {
        return Err(type_error(format!("invalid data URL module `{raw}`")));
    };
    let Some((meta, payload)) = rest.split_once(',') else {
        return Err(type_error(format!("invalid data URL module `{raw}`")));
    };
    let is_base64 = meta
        .rsplit(';')
        .next()
        .is_some_and(|part| part.eq_ignore_ascii_case("base64"));
    let bytes = if is_base64 {
        let trimmed: String = payload
            .chars()
            .filter(|c| !c.is_ascii_whitespace())
            .collect();
        base64::engine::general_purpose::STANDARD
            .decode(&trimmed)
            .or_else(|_| base64::engine::general_purpose::STANDARD_NO_PAD.decode(&trimmed))
            .map_err(|error| type_error(format!("invalid base64 data URL `{raw}`: {error}")))?
    } else {
        percent_decode(payload)
    };
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn percent_decode(input: &str) -> Vec<u8> {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%'
            && index + 2 < bytes.len()
            && let (Some(high), Some(low)) =
                (hex_digit(bytes[index + 1]), hex_digit(bytes[index + 2]))
        {
            out.push(high * 16 + low);
            index += 3;
            continue;
        }
        out.push(bytes[index]);
        index += 1;
    }
    out
}

fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}
