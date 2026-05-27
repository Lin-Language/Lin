use std::rc::Rc;
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use indexmap::IndexMap;

use lin_lex::Lexer;
use lin_parse::ast::*;
use lin_parse::Parser;
use crate::value::*;
use crate::env::Env;

enum TailResult {
    Return(Value),
    TailCall(Vec<Value>),
}

pub struct Interpreter {
    pub global_env: Env,
    pub output: Vec<String>,
    pub exit_code: Option<i32>,
    module_cache: HashMap<String, HashMap<String, Value>>,
    stdlib_sources: HashMap<String, &'static str>,
    base_path: Option<std::path::PathBuf>,
    source: String,
}

impl Interpreter {
    pub fn new() -> Self {
        let mut interp = Self {
            global_env: Env::new(),
            output: Vec::new(),
            exit_code: None,
            module_cache: HashMap::new(),
            stdlib_sources: HashMap::new(),
            base_path: None,
            source: String::new(),
        };
        interp.register_intrinsics();
        interp.register_stdlib_sources();
        interp.preload_stdlib();
        interp
    }

    pub fn run_file(&mut self, path: &std::path::Path) -> Result<Value, String> {
        let source = std::fs::read_to_string(path)
            .map_err(|e| format!("Cannot read {}: {}", path.display(), e))?;
        self.base_path = path.parent().map(|p| p.to_path_buf());
        self.run(&source)
    }

    fn register_stdlib_sources(&mut self) {
        self.stdlib_sources.insert("std/io".to_string(), include_str!("../../../stdlib/io.lin"));
        self.stdlib_sources.insert("std/string".to_string(), include_str!("../../../stdlib/string.lin"));
        self.stdlib_sources.insert("std/number".to_string(), include_str!("../../../stdlib/number.lin"));
        self.stdlib_sources.insert("std/array".to_string(), include_str!("../../../stdlib/array.lin"));
        self.stdlib_sources.insert("std/fs".to_string(), include_str!("../../../stdlib/fs.lin"));
        self.stdlib_sources.insert("std/http".to_string(), include_str!("../../../stdlib/http.lin"));
        self.stdlib_sources.insert("std/test".to_string(), include_str!("../../../stdlib/test.lin"));
        self.stdlib_sources.insert("std/object".to_string(), include_str!("../../../stdlib/object.lin"));
        self.stdlib_sources.insert("std/template".to_string(), include_str!("../../../stdlib/template.lin"));
        self.stdlib_sources.insert("std/async".to_string(), include_str!("../../../stdlib/async.lin"));
    }

    fn preload_stdlib(&mut self) {
        for module in &["std/io", "std/string", "std/array", "std/object", "std/async"] {
            let exports = self.load_module(module).unwrap_or_default();
            for (name, value) in &exports {
                self.global_env.define(name.clone(), value.clone());
            }
        }
    }

    fn register_intrinsics(&mut self) {
        self.define_native("lin_print", 1, |args| {
            Ok(args[0].clone())
        });

        self.define_native("lin_length", 1, |args| {
            match &args[0] {
                Value::String(s) => Ok(Value::Int(s.chars().count() as i64)),
                Value::Array(a) => Ok(Value::Int(a.borrow().len() as i64)),
                Value::Object(o) => Ok(Value::Int(o.borrow().len() as i64)),
                _ => Err(format!("length: unsupported type {}", args[0].type_name())),
            }
        });

        self.define_native("lin_to_string", 1, |args| {
            Ok(Value::String(Rc::new(args[0].to_display_string())))
        });

        self.define_native("__stringSlice", 3, |args| {
            match (&args[0], &args[1], &args[2]) {
                (Value::String(s), Value::Int(start), Value::Int(end)) => {
                    let chars: Vec<char> = s.chars().collect();
                    let start = (*start).max(0) as usize;
                    let end = (*end).min(chars.len() as i64) as usize;
                    if start >= end || start >= chars.len() {
                        Ok(Value::String(Rc::new(String::new())))
                    } else {
                        Ok(Value::String(Rc::new(chars[start..end].iter().collect())))
                    }
                }
                _ => Err("__stringSlice: expected (String, Int, Int)".to_string()),
            }
        });

        self.define_native("__stringIndexOf", 2, |args| {
            match (&args[0], &args[1]) {
                (Value::String(haystack), Value::String(needle)) => {
                    match haystack.find(needle.as_str()) {
                        Some(byte_pos) => {
                            let char_pos = haystack[..byte_pos].chars().count();
                            Ok(Value::Int(char_pos as i64))
                        }
                        None => Ok(Value::Int(-1)),
                    }
                }
                _ => Err("__stringIndexOf: expected (String, String)".to_string()),
            }
        });

        self.define_native("__stringToUpper", 1, |args| {
            match &args[0] {
                Value::String(s) => Ok(Value::String(Rc::new(s.to_uppercase()))),
                _ => Err("__stringToUpper: expected String".to_string()),
            }
        });

        self.define_native("__stringToLower", 1, |args| {
            match &args[0] {
                Value::String(s) => Ok(Value::String(Rc::new(s.to_lowercase()))),
                _ => Err("__stringToLower: expected String".to_string()),
            }
        });

        self.define_native("__stringTrim", 1, |args| {
            match &args[0] {
                Value::String(s) => Ok(Value::String(Rc::new(s.trim().to_string()))),
                _ => Err("__stringTrim: expected String".to_string()),
            }
        });

        self.define_native("__stringLength", 1, |args| {
            match &args[0] {
                Value::String(s) => Ok(Value::Int(s.chars().count() as i64)),
                _ => Err("__stringLength: expected String".to_string()),
            }
        });

        self.define_native("__stringContains", 2, |args| {
            match (&args[0], &args[1]) {
                (Value::String(haystack), Value::String(needle)) => {
                    Ok(Value::Bool(haystack.contains(needle.as_str())))
                }
                _ => Err("__stringContains: expected (String, String)".to_string()),
            }
        });

        self.define_native("__stringStartsWith", 2, |args| {
            match (&args[0], &args[1]) {
                (Value::String(s), Value::String(prefix)) => {
                    Ok(Value::Bool(s.starts_with(prefix.as_str())))
                }
                _ => Err("__stringStartsWith: expected (String, String)".to_string()),
            }
        });

        self.define_native("__stringEndsWith", 2, |args| {
            match (&args[0], &args[1]) {
                (Value::String(s), Value::String(suffix)) => {
                    Ok(Value::Bool(s.ends_with(suffix.as_str())))
                }
                _ => Err("__stringEndsWith: expected (String, String)".to_string()),
            }
        });

        self.define_native("__stringSplit", 2, |args| {
            match (&args[0], &args[1]) {
                (Value::String(s), Value::String(delim)) => {
                    let parts: Vec<Value> = s.split(delim.as_str())
                        .map(|p| Value::String(Rc::new(p.to_string())))
                        .collect();
                    Ok(Value::Array(Rc::new(RefCell::new(parts))))
                }
                _ => Err("__stringSplit: expected (String, String)".to_string()),
            }
        });

        self.define_native("__stringJoin", 2, |args| {
            match (&args[0], &args[1]) {
                (Value::Array(arr), Value::String(sep)) => {
                    let parts: Vec<String> = arr.borrow().iter()
                        .map(|v| v.to_display_string())
                        .collect();
                    Ok(Value::String(Rc::new(parts.join(sep.as_str()))))
                }
                _ => Err("__stringJoin: expected (Array, String)".to_string()),
            }
        });

        self.define_native("__stringReplace", 3, |args| {
            match (&args[0], &args[1], &args[2]) {
                (Value::String(s), Value::String(pattern), Value::String(replacement)) => {
                    Ok(Value::String(Rc::new(s.replace(pattern.as_str(), replacement.as_str()))))
                }
                _ => Err("__stringReplace: expected (String, String, String)".to_string()),
            }
        });

        self.define_native("__stringRepeat", 2, |args| {
            match (&args[0], &args[1]) {
                (Value::String(s), Value::Int(n)) => {
                    Ok(Value::String(Rc::new(s.repeat(*n as usize))))
                }
                _ => Err("__stringRepeat: expected (String, Int)".to_string()),
            }
        });

        self.define_native("__parseInt32", 1, |args| {
            match &args[0] {
                Value::String(s) => {
                    match s.trim().parse::<i64>() {
                        Ok(v) => Ok(Value::Int(v)),
                        Err(_) => Ok(Value::Null),
                    }
                }
                _ => Err("__parseInt32: expected String".to_string()),
            }
        });

        self.define_native("__parseFloat64", 1, |args| {
            match &args[0] {
                Value::String(s) => {
                    match s.trim().parse::<f64>() {
                        Ok(v) => Ok(Value::Float(v)),
                        Err(_) => Ok(Value::Null),
                    }
                }
                _ => Err("__parseFloat64: expected String".to_string()),
            }
        });

        self.define_native("__isInt32", 1, |args| {
            match &args[0] {
                Value::String(s) => Ok(Value::Bool(s.trim().parse::<i64>().is_ok())),
                Value::Int(_) => Ok(Value::Bool(true)),
                _ => Ok(Value::Bool(false)),
            }
        });

        self.define_native("__toInt32", 1, |args| {
            match &args[0] {
                Value::Int(v) => Ok(Value::Int(*v)),
                Value::Float(v) => Ok(Value::Int(*v as i64)),
                Value::String(s) => {
                    match s.trim().parse::<i64>() {
                        Ok(v) => Ok(Value::Int(v)),
                        Err(_) => Err("__toInt32: cannot convert".to_string()),
                    }
                }
                _ => Err("__toInt32: unsupported type".to_string()),
            }
        });

        self.define_native("lin_push", 2, |args| {
            match &args[0] {
                Value::Array(arr) => {
                    arr.borrow_mut().push(args[1].clone());
                    Ok(args[0].clone())
                }
                _ => Err("push: expected Array as first argument".to_string()),
            }
        });

        self.define_native("concat", 2, |args| {
            match (&args[0], &args[1]) {
                (Value::Array(a), Value::Array(b)) => {
                    let mut result = a.borrow().clone();
                    result.extend(b.borrow().iter().cloned());
                    Ok(Value::Array(Rc::new(RefCell::new(result))))
                }
                _ => Err("concat: expected two Arrays".to_string()),
            }
        });

        self.define_native("lin_keys", 1, |args| {
            match &args[0] {
                Value::Object(obj) => {
                    let ks: Vec<Value> = obj.borrow().keys()
                        .map(|k| Value::String(Rc::new(k.clone())))
                        .collect();
                    Ok(Value::Array(Rc::new(RefCell::new(ks))))
                }
                _ => Err("keys: expected Object".to_string()),
            }
        });

        self.define_native("values", 1, |args| {
            match &args[0] {
                Value::Object(obj) => {
                    let vs: Vec<Value> = obj.borrow().values().cloned().collect();
                    Ok(Value::Array(Rc::new(RefCell::new(vs))))
                }
                _ => Err("values: expected Object".to_string()),
            }
        });

        self.define_native("entries", 1, |args| {
            match &args[0] {
                Value::Object(obj) => {
                    let es: Vec<Value> = obj.borrow().iter()
                        .map(|(k, v)| {
                            let pair = vec![
                                Value::String(Rc::new(k.clone())),
                                v.clone(),
                            ];
                            Value::Array(Rc::new(RefCell::new(pair)))
                        })
                        .collect();
                    Ok(Value::Array(Rc::new(RefCell::new(es))))
                }
                _ => Err("entries: expected Object".to_string()),
            }
        });

        // Placeholder natives for functions that need interpreter access
        // (actual logic is handled specially in call_value)
        self.define_native("lin_for", 2, |_args| Ok(Value::Null));
        self.define_native("lin_iter", 4, |_args| Ok(Value::Null));
        self.define_native("toFloat64", 1, |args| {
            match &args[0] {
                Value::Int(v) => Ok(Value::Float(*v as f64)),
                _ => Err("toFloat64: expected Int".to_string()),
            }
        });
        self.define_native("isInt32", 1, |args| {
            match &args[0] {
                Value::String(s) => Ok(Value::Bool(s.trim().parse::<i64>().is_ok())),
                Value::Int(_) => Ok(Value::Bool(true)),
                _ => Ok(Value::Bool(false)),
            }
        });
        self.define_native("toInt32", 1, |args| {
            match &args[0] {
                Value::String(s) => match s.trim().parse::<i64>() {
                    Ok(v) => Ok(Value::Int(v)),
                    Err(_) => Err("toInt32: cannot convert".to_string()),
                },
                Value::Float(v) => Ok(Value::Int(*v as i64)),
                Value::Int(v) => Ok(Value::Int(*v)),
                _ => Err("toInt32: unsupported".to_string()),
            }
        });

        self.define_native("__toFloat64", 1, |args| {
            match &args[0] {
                Value::Int(v) => Ok(Value::Float(*v as f64)),
                Value::Float(v) => Ok(Value::Float(*v)),
                Value::String(s) => {
                    match s.trim().parse::<f64>() {
                        Ok(v) => Ok(Value::Float(v)),
                        Err(_) => Err("__toFloat64: cannot convert".to_string()),
                    }
                }
                _ => Err("__toFloat64: unsupported type".to_string()),
            }
        });

        // Concurrency intrinsics — the actual implementation is in call_value dispatch above.
        // We register them as 1-arg stubs so they are callable; the real dispatch intercepts them.
        self.define_native("lin_async", 1, |_| Ok(Value::Null));
        self.define_native("lin_await", 1, |_| Ok(Value::Null));
        self.define_native("lin_parallel", 1, |_| Ok(Value::Null));
        self.define_native("lin_race", 1, |_| Ok(Value::Null));
        self.define_native("lin_timeout", 2, |_| Ok(Value::Null));
        self.define_native("lin_retry", 2, |_| Ok(Value::Null));
        self.define_native("lin_thread_pool", 1, |_| Ok(Value::Null));
        self.define_native("lin_worker", 2, |_| Ok(Value::Null));

        // IO intrinsics
        self.define_native("__ioReadLine", 0, |_| {
            use std::io::BufRead;
            let stdin = std::io::stdin();
            let mut line = String::new();
            match stdin.lock().read_line(&mut line) {
                Ok(0) => Ok(Value::Null), // EOF
                Ok(_) => {
                    if line.ends_with('\n') { line.pop(); }
                    if line.ends_with('\r') { line.pop(); }
                    Ok(Value::String(Rc::new(line)))
                }
                Err(e) => Err(format!("__ioReadLine: {}", e)),
            }
        });

        self.define_native("__ioReadAll", 0, |_| {
            use std::io::Read;
            let mut content = String::new();
            std::io::stdin().read_to_string(&mut content)
                .map_err(|e| format!("__ioReadAll: {}", e))?;
            Ok(Value::String(Rc::new(content)))
        });

        // Filesystem intrinsics
        self.define_native("__fsReadFile", 1, |args| {
            match &args[0] {
                Value::String(path) => {
                    match std::fs::read_to_string(path.as_str()) {
                        Ok(content) => Ok(Value::String(Rc::new(content))),
                        Err(e) => {
                            let mut map = indexmap::IndexMap::new();
                            map.insert("type".to_string(), Value::String(Rc::new("error".to_string())));
                            map.insert("message".to_string(), Value::String(Rc::new(e.to_string())));
                            Ok(Value::Object(Rc::new(RefCell::new(map))))
                        }
                    }
                }
                _ => Err("__fsReadFile: expected String path".to_string()),
            }
        });

        self.define_native("__fsWriteFile", 2, |args| {
            match (&args[0], &args[1]) {
                (Value::String(path), Value::String(content)) => {
                    match std::fs::write(path.as_str(), content.as_str()) {
                        Ok(_) => Ok(Value::Null),
                        Err(e) => {
                            let mut map = indexmap::IndexMap::new();
                            map.insert("type".to_string(), Value::String(Rc::new("error".to_string())));
                            map.insert("message".to_string(), Value::String(Rc::new(e.to_string())));
                            Ok(Value::Object(Rc::new(RefCell::new(map))))
                        }
                    }
                }
                _ => Err("__fsWriteFile: expected (String, String)".to_string()),
            }
        });

        self.define_native("__fsAppendFile", 2, |args| {
            use std::io::Write;
            match (&args[0], &args[1]) {
                (Value::String(path), Value::String(content)) => {
                    use std::fs::OpenOptions;
                    match OpenOptions::new().append(true).create(true).open(path.as_str()) {
                        Ok(mut file) => {
                            match file.write_all(content.as_bytes()) {
                                Ok(_) => Ok(Value::Null),
                                Err(e) => {
                                    let mut map = indexmap::IndexMap::new();
                                    map.insert("type".to_string(), Value::String(Rc::new("error".to_string())));
                                    map.insert("message".to_string(), Value::String(Rc::new(e.to_string())));
                                    Ok(Value::Object(Rc::new(RefCell::new(map))))
                                }
                            }
                        }
                        Err(e) => {
                            let mut map = indexmap::IndexMap::new();
                            map.insert("type".to_string(), Value::String(Rc::new("error".to_string())));
                            map.insert("message".to_string(), Value::String(Rc::new(e.to_string())));
                            Ok(Value::Object(Rc::new(RefCell::new(map))))
                        }
                    }
                }
                _ => Err("__fsAppendFile: expected (String, String)".to_string()),
            }
        });

        self.define_native("__fsExists", 1, |args| {
            match &args[0] {
                Value::String(path) => Ok(Value::Bool(std::path::Path::new(path.as_str()).exists())),
                _ => Err("__fsExists: expected String path".to_string()),
            }
        });

        self.define_native("__fsReadJson", 1, |args| {
            match &args[0] {
                Value::String(path) => {
                    match std::fs::read_to_string(path.as_str()) {
                        Err(e) => {
                            let mut map = indexmap::IndexMap::new();
                            map.insert("type".to_string(), Value::String(Rc::new("error".to_string())));
                            map.insert("message".to_string(), Value::String(Rc::new(e.to_string())));
                            Ok(Value::Object(Rc::new(RefCell::new(map))))
                        }
                        Ok(content) => {
                            match parse_json_to_value(&content) {
                                Ok(v) => Ok(v),
                                Err(e) => {
                                    let mut map = indexmap::IndexMap::new();
                                    map.insert("type".to_string(), Value::String(Rc::new("error".to_string())));
                                    map.insert("message".to_string(), Value::String(Rc::new(e)));
                                    Ok(Value::Object(Rc::new(RefCell::new(map))))
                                }
                            }
                        }
                    }
                }
                _ => Err("__fsReadJson: expected String path".to_string()),
            }
        });

        self.define_native("__fsWriteJson", 2, |args| {
            match &args[0] {
                Value::String(path) => {
                    let json_str = value_to_json_string(&args[1]);
                    match std::fs::write(path.as_str(), json_str) {
                        Ok(_) => Ok(Value::Null),
                        Err(e) => {
                            let mut map = indexmap::IndexMap::new();
                            map.insert("type".to_string(), Value::String(Rc::new("error".to_string())));
                            map.insert("message".to_string(), Value::String(Rc::new(e.to_string())));
                            Ok(Value::Object(Rc::new(RefCell::new(map))))
                        }
                    }
                }
                _ => Err("__fsWriteJson: expected (String, Json)".to_string()),
            }
        });

        self.define_native("__parseJson", 1, |args| {
            match &args[0] {
                Value::String(s) => {
                    match parse_json_to_value(s.as_str()) {
                        Ok(v) => Ok(v),
                        Err(e) => {
                            let mut map = indexmap::IndexMap::new();
                            map.insert("type".to_string(), Value::String(Rc::new("error".to_string())));
                            map.insert("message".to_string(), Value::String(Rc::new(e)));
                            Ok(Value::Object(Rc::new(RefCell::new(map))))
                        }
                    }
                }
                _ => Err("__parseJson: expected String".to_string()),
            }
        });

        // __exit is dispatched specially in call_value to capture exit_code on self
        self.define_native("__exit", 1, |_| Ok(Value::Null));

        // __ioLines and __fsReadLines need interpreter dispatch (they return lazy iterators)
        self.define_native("__ioLines", 0, |_| Ok(Value::Null));
        self.define_native("__fsReadLines", 1, |_| Ok(Value::Null));

        // Server intrinsics — dispatched via call_value
        self.define_native("__serverServe", 2, |_| Ok(Value::Null));
        self.define_native("__serverServeWithPool", 3, |_| Ok(Value::Null));
        self.define_native("__serverPathMatch", 2, |args| {
            match (&args[0], &args[1]) {
                (Value::String(pattern), Value::String(path)) => {
                    let pat_parts: Vec<&str> = pattern.split('/').collect();
                    let path_parts: Vec<&str> = path.split('/').collect();
                    if pat_parts.len() != path_parts.len() {
                        return Ok(Value::Null);
                    }
                    let mut captures = indexmap::IndexMap::new();
                    for (pp, pv) in pat_parts.iter().zip(path_parts.iter()) {
                        if let Some(name) = pp.strip_prefix(':') {
                            captures.insert(name.to_string(), Value::String(Rc::new(pv.to_string())));
                        } else if pp != pv {
                            return Ok(Value::Null);
                        }
                    }
                    Ok(Value::Object(Rc::new(RefCell::new(captures))))
                }
                _ => Err("__serverPathMatch: expected (String, String)".to_string()),
            }
        });

        // HTTP intrinsics
        self.define_native("__httpFetch", 1, |args| {
            match &args[0] {
                Value::String(url) => {
                    match ureq::get(url.as_str()).call() {
                        Ok(response) => {
                            let status = response.status() as i64;
                            let headers_map = indexmap::IndexMap::new();
                            let body = response.into_string()
                                .unwrap_or_default();
                            let mut map = indexmap::IndexMap::new();
                            map.insert("status".to_string(), Value::Int(status));
                            map.insert("headers".to_string(), Value::Object(Rc::new(RefCell::new(headers_map))));
                            map.insert("body".to_string(), Value::String(Rc::new(body)));
                            Ok(Value::Object(Rc::new(RefCell::new(map))))
                        }
                        Err(ureq::Error::Status(code, response)) => {
                            let body = response.into_string().unwrap_or_default();
                            let mut map = indexmap::IndexMap::new();
                            map.insert("status".to_string(), Value::Int(code as i64));
                            map.insert("headers".to_string(), Value::Object(Rc::new(RefCell::new(indexmap::IndexMap::new()))));
                            map.insert("body".to_string(), Value::String(Rc::new(body)));
                            Ok(Value::Object(Rc::new(RefCell::new(map))))
                        }
                        Err(e) => {
                            let mut map = indexmap::IndexMap::new();
                            map.insert("type".to_string(), Value::String(Rc::new("error".to_string())));
                            map.insert("message".to_string(), Value::String(Rc::new(e.to_string())));
                            Ok(Value::Object(Rc::new(RefCell::new(map))))
                        }
                    }
                }
                _ => Err("__httpFetch: expected String url".to_string()),
            }
        });

        self.define_native("__httpFetchWith", 2, |args| {
            match &args[0] {
                Value::String(url) => {
                    let opts = &args[1];
                    let method = match opts {
                        Value::Object(o) => {
                            o.borrow().get("method")
                                .and_then(|v| if let Value::String(s) = v { Some(s.as_str().to_uppercase()) } else { None })
                                .unwrap_or_else(|| "GET".to_string())
                        }
                        _ => "GET".to_string(),
                    };
                    let body_str = match opts {
                        Value::Object(o) => {
                            o.borrow().get("body")
                                .and_then(|v| if let Value::String(s) = v { Some(s.as_ref().clone()) } else { None })
                        }
                        _ => None,
                    };
                    let req = ureq::request(method.as_str(), url.as_str());
                    // Add headers from opts["headers"] if present
                    let req = if let Value::Object(o) = opts {
                        if let Some(Value::Object(hdrs)) = o.borrow().get("headers").cloned() {
                            let mut r = req;
                            for (k, v) in hdrs.borrow().iter() {
                                if let Value::String(vs) = v {
                                    r = r.set(k.as_str(), vs.as_str());
                                }
                            }
                            r
                        } else {
                            req
                        }
                    } else {
                        req
                    };
                    let result = if let Some(body) = body_str {
                        req.send_string(body.as_str())
                    } else {
                        req.call()
                    };
                    match result {
                        Ok(response) => {
                            let status = response.status() as i64;
                            let body = response.into_string().unwrap_or_default();
                            let mut map = indexmap::IndexMap::new();
                            map.insert("status".to_string(), Value::Int(status));
                            map.insert("headers".to_string(), Value::Object(Rc::new(RefCell::new(indexmap::IndexMap::new()))));
                            map.insert("body".to_string(), Value::String(Rc::new(body)));
                            Ok(Value::Object(Rc::new(RefCell::new(map))))
                        }
                        Err(ureq::Error::Status(code, response)) => {
                            let body = response.into_string().unwrap_or_default();
                            let mut map = indexmap::IndexMap::new();
                            map.insert("status".to_string(), Value::Int(code as i64));
                            map.insert("headers".to_string(), Value::Object(Rc::new(RefCell::new(indexmap::IndexMap::new()))));
                            map.insert("body".to_string(), Value::String(Rc::new(body)));
                            Ok(Value::Object(Rc::new(RefCell::new(map))))
                        }
                        Err(e) => {
                            let mut map = indexmap::IndexMap::new();
                            map.insert("type".to_string(), Value::String(Rc::new("error".to_string())));
                            map.insert("message".to_string(), Value::String(Rc::new(e.to_string())));
                            Ok(Value::Object(Rc::new(RefCell::new(map))))
                        }
                    }
                }
                _ => Err("__httpFetchWith: expected String url".to_string()),
            }
        });
    }

    fn error_at(&self, span: lin_common::Span, msg: &str) -> String {
        if self.source.is_empty() || span.start == 0 && span.end == 0 {
            return msg.to_string();
        }
        let (line, col) = span.line_col(&self.source);
        format!("[line {}:{}] {}", line, col, msg)
    }

    fn define_native(&mut self, name: &str, arity: usize, func: NativeFn) {
        self.global_env.define(
            name.to_string(),
            Value::NativeFunction(Rc::new(NativeFunction {
                name: name.to_string(),
                arity,
                func,
            })),
        );
    }

    pub fn run(&mut self, source: &str) -> Result<Value, String> {
        self.source = source.to_string();
        let mut lexer = Lexer::new(source, 0);
        let tokens = lexer.tokenize();
        let mut parser = Parser::new(tokens);
        let module = parser.parse_module();
        self.eval_module(&module)
    }

    fn eval_module(&mut self, module: &Module) -> Result<Value, String> {
        let stmts = module.statements.clone();

        // Pre-scan: register top-level function bindings as mutable cells
        // so that forward references between functions work.
        let mut forward_declared: Vec<String> = Vec::new();
        for stmt in &stmts {
            if let Stmt::Val { pattern: Pattern::Ident(name, _), value, .. } = stmt {
                if matches!(value, Expr::Function { .. }) {
                    self.global_env.define_mut(name.clone(), Value::Null);
                    forward_declared.push(name.clone());
                }
            }
        }

        let mut last = Value::Null;
        for stmt in &stmts {
            last = self.eval_top_stmt_with_forwards(stmt, &forward_declared)?;
        }
        Ok(last)
    }

    fn eval_top_stmt_with_forwards(&mut self, stmt: &Stmt, forward_declared: &[String]) -> Result<Value, String> {
        match stmt {
            Stmt::Val { pattern: Pattern::Ident(name, _), value, .. } if forward_declared.contains(name) => {
                let val = self.eval_expr_in_global(value)?;
                // Give the function its name (for TCO self-call detection)
                let named_val = match val {
                    Value::Function(f) if f.name.is_none() => {
                        let mut named = (*f).clone();
                        named.name = Some(name.clone());
                        Value::Function(Rc::new(named))
                    }
                    _ => val,
                };
                // Update the mutable cell that was pre-registered
                self.global_env.set(name, named_val.clone());
                Ok(named_val)
            }
            _ => self.eval_top_stmt(stmt),
        }
    }

    fn eval_top_stmt(&mut self, stmt: &Stmt) -> Result<Value, String> {
        match stmt {
            Stmt::Val { pattern, value, .. } => {
                let val = self.eval_expr_in_global(value)?;
                self.bind_pattern_in_global(pattern, &val)?;
                Ok(val)
            }
            Stmt::Var { name, value, .. } => {
                let val = self.eval_expr_in_global(value)?;
                self.global_env.define_mut(name.clone(), val.clone());
                Ok(val)
            }
            Stmt::TypeDecl { .. } => Ok(Value::Null),
            Stmt::Import { bindings, path, .. } => {
                self.eval_import(bindings, path)
            }
            Stmt::ForeignImport { bindings, .. } => {
                // Register each foreign binding as a stub function that errors at call time.
                // Real FFI calls require the LLVM compiler pipeline.
                fn foreign_stub(_: &[Value]) -> Result<Value, String> {
                    Err("Foreign functions are not available in the interpreter; use `lin build` to compile".to_string())
                }
                for binding in bindings {
                    // Derive arity from the declared function type so call-site arity checks pass.
                    let arity = if let lin_parse::ast::TypeExpr::Function(params, _, _) = &binding.type_ann {
                        params.len()
                    } else { 0 };
                    self.global_env.define(
                        binding.name.clone(),
                        Value::NativeFunction(Rc::new(NativeFunction {
                            name: binding.name.clone(),
                            arity,
                            func: foreign_stub,
                        })),
                    );
                }
                Ok(Value::Null)
            }
            Stmt::Expr(expr) => self.eval_expr_in_global(expr),
        }
    }

    fn eval_expr_in_global(&mut self, expr: &Expr) -> Result<Value, String> {
        let mut env = self.global_env.clone();
        let result = self.eval_expr_in_env(expr, &mut env)?;
        self.global_env = env;
        Ok(result)
    }

    fn bind_pattern_in_global(&mut self, pattern: &Pattern, value: &Value) -> Result<(), String> {
        let mut env = self.global_env.clone();
        self.bind_pattern_in_env(pattern, value, &mut env)?;
        self.global_env = env;
        Ok(())
    }

    fn eval_stmt_in_env(&mut self, stmt: &Stmt, env: &mut Env) -> Result<Value, String> {
        match stmt {
            Stmt::Val { pattern, value, .. } => {
                let val = self.eval_expr_in_env(value, env)?;
                self.bind_pattern_in_env(pattern, &val, env)?;
                Ok(val)
            }
            Stmt::Var { name, value, .. } => {
                let val = self.eval_expr_in_env(value, env)?;
                env.define_mut(name.clone(), val.clone());
                Ok(val)
            }
            Stmt::TypeDecl { .. } => Ok(Value::Null),
            Stmt::ForeignImport { .. } => Ok(Value::Null), // stubs registered in global scope
            Stmt::Import { bindings, path, .. } => {
                let exports = self.load_module(path)?;
                for binding in bindings {
                    let name = binding.alias.as_ref().unwrap_or(&binding.name);
                    if let Some(val) = exports.get(&binding.name) {
                        env.define(name.clone(), val.clone());
                    }
                }
                Ok(Value::Null)
            }
            Stmt::Expr(expr) => self.eval_expr_in_env(expr, env),
        }
    }

    fn eval_import(&mut self, bindings: &[ImportBinding], path: &str) -> Result<Value, String> {
        let exports = self.load_module(path)?;
        for binding in bindings {
            let name = binding.alias.as_ref().unwrap_or(&binding.name);
            if let Some(val) = exports.get(&binding.name) {
                self.global_env.define(name.clone(), val.clone());
            }
        }
        Ok(Value::Null)
    }

    fn load_module(&mut self, path: &str) -> Result<HashMap<String, Value>, String> {
        if let Some(cached) = self.module_cache.get(path) {
            return Ok(cached.clone());
        }

        let source = if let Some(src) = self.stdlib_sources.get(path) {
            src.to_string()
        } else if let Some(base) = &self.base_path {
            let file_path = base.join(format!("{}.lin", path));
            std::fs::read_to_string(&file_path)
                .map_err(|_| format!("Module not found: {} (tried {})", path, file_path.display()))?
        } else {
            return Err(format!("Module not found: {}", path));
        };

        // Mark as loading to prevent cycles
        self.module_cache.insert(path.to_string(), HashMap::new());

        let mut lexer = Lexer::new(&source, 0);
        let tokens = lexer.tokenize();
        let mut parser = Parser::new(tokens);
        let module = parser.parse_module();

        let mut module_env = Env::new();
        for name in self.global_env.keys() {
            if let Some(val) = self.global_env.get(&name) {
                module_env.define(name, val);
            }
        }

        let mut exports = HashMap::new();

        // Pre-scan: forward-declare function bindings in module scope
        let mut mod_forward: Vec<String> = Vec::new();
        for stmt in &module.statements {
            if let Stmt::Val { pattern: Pattern::Ident(name, _), value, .. } = stmt {
                if matches!(value, Expr::Function { .. }) {
                    module_env.define_mut(name.clone(), Value::Null);
                    mod_forward.push(name.clone());
                }
            }
        }

        for stmt in &module.statements {
            match stmt {
                Stmt::Import { bindings, path: imp_path, .. } => {
                    let imp_exports = self.load_module(imp_path)?;
                    for binding in bindings {
                        let name = binding.alias.as_ref().unwrap_or(&binding.name);
                        if let Some(val) = imp_exports.get(&binding.name) {
                            module_env.define(name.clone(), val.clone());
                        }
                    }
                }
                Stmt::Val { pattern: Pattern::Ident(name, _), value, exported, .. } if mod_forward.contains(name) => {
                    let val = self.eval_expr_in_env(value, &mut module_env)?;
                    let named_val = match val {
                        Value::Function(f) if f.name.is_none() => {
                            let mut named = (*f).clone();
                            named.name = Some(name.clone());
                            Value::Function(Rc::new(named))
                        }
                        _ => val,
                    };
                    module_env.set(name, named_val.clone());
                    if *exported {
                        exports.insert(name.clone(), named_val);
                    }
                }
                Stmt::Val { pattern, value, exported, .. } => {
                    let val = self.eval_expr_in_env(value, &mut module_env)?;
                    self.bind_pattern_in_env(pattern, &val, &mut module_env)?;
                    if *exported {
                        if let Pattern::Ident(name, _) = pattern {
                            exports.insert(name.clone(), val);
                        }
                    }
                }
                Stmt::Var { name, value, exported, .. } => {
                    let val = self.eval_expr_in_env(value, &mut module_env)?;
                    module_env.define_mut(name.clone(), val.clone());
                    if *exported {
                        exports.insert(name.clone(), val);
                    }
                }
                Stmt::TypeDecl { .. } => {}
                Stmt::ForeignImport { bindings, .. } => {
                    fn foreign_stub(_: &[Value]) -> Result<Value, String> {
                        Err("Foreign functions are not available in the interpreter; use `lin build` to compile".to_string())
                    }
                    for binding in bindings {
                        let arity = if let lin_parse::ast::TypeExpr::Function(params, _, _) = &binding.type_ann {
                            params.len()
                        } else { 0 };
                        module_env.define(binding.name.clone(), Value::NativeFunction(Rc::new(NativeFunction {
                            name: binding.name.clone(),
                            arity,
                            func: foreign_stub,
                        })));
                    }
                }
                Stmt::Expr(expr) => {
                    self.eval_expr_in_env(expr, &mut module_env)?;
                }
            }
        }

        self.module_cache.insert(path.to_string(), exports.clone());
        Ok(exports)
    }

    pub fn eval_expr_in_env(&mut self, expr: &Expr, env: &mut Env) -> Result<Value, String> {
        match expr {
            Expr::IntLit(v, _) => Ok(Value::Int(*v)),
            Expr::FloatLit(v, _) => Ok(Value::Float(*v)),
            Expr::StringLit(s, _) => Ok(Value::String(Rc::new(s.clone()))),
            Expr::BoolLit(b, _) => Ok(Value::Bool(*b)),
            Expr::NullLit(_) => Ok(Value::Null),

            Expr::Ident(name, span) => {
                env.get(name)
                    .or_else(|| self.global_env.get(name))
                    .ok_or_else(|| self.error_at(*span, &format!("Undefined variable: {}", name)))
            }

            Expr::StringInterp(parts, _) => {
                let mut result = String::new();
                for part in parts {
                    match part {
                        StringPart::Literal(s) => result.push_str(s),
                        StringPart::Expr(e) => {
                            let val = self.eval_expr_in_env(e, env)?;
                            result.push_str(&val.to_display_string());
                        }
                    }
                }
                Ok(Value::String(Rc::new(result)))
            }

            Expr::BinaryOp { left, op, right, .. } => {
                // Short-circuit for && and ||
                if *op == BinOp::And {
                    let l = self.eval_expr_in_env(left, env)?;
                    if !l.is_truthy() {
                        return Ok(Value::Bool(false));
                    }
                    let r = self.eval_expr_in_env(right, env)?;
                    return Ok(Value::Bool(r.is_truthy()));
                }
                if *op == BinOp::Or {
                    let l = self.eval_expr_in_env(left, env)?;
                    if l.is_truthy() {
                        return Ok(Value::Bool(true));
                    }
                    let r = self.eval_expr_in_env(right, env)?;
                    return Ok(Value::Bool(r.is_truthy()));
                }

                let l = self.eval_expr_in_env(left, env)?;
                let r = self.eval_expr_in_env(right, env)?;
                self.eval_binary_op(&l, op, &r)
            }

            Expr::Call { func, args, span } => {
                let func_val = self.eval_expr_in_env(func, env)?;
                let mut arg_vals = Vec::new();
                for arg in args {
                    arg_vals.push(self.eval_expr_in_env(arg, env)?);
                }
                self.call_value(&func_val, arg_vals, env)
                    .map_err(|e| if e.starts_with('[') { e } else { self.error_at(*span, &e) })
            }

            Expr::DotCall { receiver, method, args, span } => {
                let recv = self.eval_expr_in_env(receiver, env)?;

                // Method dispatch on concurrency primitives
                match (&recv, method.as_str()) {
                    (Value::ThreadPool(pool), "async") => {
                        let extra_args: Vec<Value> = if let Some(a) = args {
                            a.iter().map(|e| self.eval_expr_in_env(e, env)).collect::<Result<_, _>>()?
                        } else { vec![] };
                        return self.pool_async(&pool.clone(), &extra_args)
                            .map_err(|e| self.error_at(*span, &e));
                    }
                    (Value::Worker(w), "message") => {
                        let extra_args: Vec<Value> = if let Some(a) = args {
                            a.iter().map(|e| self.eval_expr_in_env(e, env)).collect::<Result<_, _>>()?
                        } else { vec![] };
                        if extra_args.len() != 1 {
                            return Err(self.error_at(*span, "worker.message expects 1 argument"));
                        }
                        return self.worker_message(&w.clone(), &extra_args[0])
                            .map_err(|e| self.error_at(*span, &e));
                    }
                    (Value::Worker(w), "request") => {
                        let extra_args: Vec<Value> = if let Some(a) = args {
                            a.iter().map(|e| self.eval_expr_in_env(e, env)).collect::<Result<_, _>>()?
                        } else { vec![] };
                        if extra_args.len() != 1 {
                            return Err(self.error_at(*span, "worker.request expects 1 argument"));
                        }
                        return self.worker_request(&w.clone(), &extra_args[0])
                            .map_err(|e| self.error_at(*span, &e));
                    }
                    (Value::Worker(w), "close") => {
                        return self.worker_close(&w.clone())
                            .map_err(|e| self.error_at(*span, &e));
                    }
                    (Value::Promise(p), "map") => {
                        let extra_args: Vec<Value> = if let Some(a) = args {
                            a.iter().map(|e| self.eval_expr_in_env(e, env)).collect::<Result<_, _>>()?
                        } else { vec![] };
                        if extra_args.len() != 1 {
                            return Err(self.error_at(*span, "promise.map expects 1 argument"));
                        }
                        return self.promise_map(&p.clone(), &extra_args[0])
                            .map_err(|e| self.error_at(*span, &e));
                    }
                    (Value::ThreadPool(pool), "serve") => {
                        let extra_args: Vec<Value> = if let Some(a) = args {
                            a.iter().map(|e| self.eval_expr_in_env(e, env)).collect::<Result<_, _>>()?
                        } else { vec![] };
                        if extra_args.len() != 2 {
                            return Err(self.error_at(*span, "pool.serve expects (port, handler)"));
                        }
                        let port = match &extra_args[0] {
                            Value::Int(p) => *p as u16,
                            _ => return Err(self.error_at(*span, "pool.serve: port must be Int")),
                        };
                        let handler = extra_args[1].clone();
                        let pool_clone = pool.clone();
                        return self.builtin_server_serve_with_pool(port, &Value::ThreadPool(pool_clone), &handler)
                            .map_err(|e| self.error_at(*span, &e));
                    }
                    _ => {}
                }

                // Special case: handle TupleArgs receiver
                let first_args = match &recv {
                    _ if matches!(receiver.as_ref(), Expr::TupleArgs(_, _)) => {
                        if let Expr::TupleArgs(exprs, _) = receiver.as_ref() {
                            let mut vals = Vec::new();
                            for e in exprs {
                                vals.push(self.eval_expr_in_env(e, env)?);
                            }
                            vals
                        } else {
                            vec![recv]
                        }
                    }
                    _ => vec![recv],
                };

                // Look up the method
                let func_val = env.get(method)
                    .or_else(|| self.global_env.get(method))
                    .ok_or_else(|| self.error_at(*span, &format!("Undefined function: {}", method)))?;

                let mut all_args = first_args;
                if let Some(call_args) = args {
                    for arg in call_args {
                        all_args.push(self.eval_expr_in_env(arg, env)?);
                    }
                }

                self.call_value(&func_val, all_args, env)
                    .map_err(|e| if e.starts_with('[') { e } else { self.error_at(*span, &e) })
            }

            Expr::Index { object, key, span } => {
                let obj = self.eval_expr_in_env(object, env)?;
                let k = self.eval_expr_in_env(key, env)?;
                self.eval_index(&obj, &k)
                    .map_err(|e| if e.starts_with('[') { e } else { self.error_at(*span, &e) })
            }

            Expr::If { condition, then_branch, else_branch, .. } => {
                let cond = self.eval_expr_in_env(condition, env)?;
                if cond.is_truthy() {
                    self.eval_expr_in_env(then_branch, env)
                } else {
                    self.eval_expr_in_env(else_branch, env)
                }
            }

            Expr::Match { scrutinee, arms, .. } => {
                let val = self.eval_expr_in_env(scrutinee, env)?;
                self.eval_match(&val, arms, env)
            }

            Expr::Block(stmts, final_expr, _) => {
                let mut block_env = Env::child(env);
                for stmt in stmts {
                    self.eval_stmt_in_env(stmt, &mut block_env)?;
                }
                self.eval_expr_in_env(final_expr, &mut block_env)
            }

            Expr::Function { params, body, .. } => {
                let func = Function {
                    name: None,
                    params: params.clone(),
                    body: *body.clone(),
                    closure: env.clone(),
                    arity: params.len(),
                };
                Ok(Value::Function(Rc::new(func)))
            }

            Expr::Object(fields, _) => {
                let mut map = IndexMap::new();
                for field in fields {
                    match field {
                        ObjectField::Pair(key_expr, val_expr) => {
                            let key = match self.eval_expr_in_env(key_expr, env)? {
                                Value::String(s) => (*s).clone(),
                                other => other.to_display_string(),
                            };
                            let val = self.eval_expr_in_env(val_expr, env)?;
                            map.insert(key, val);
                        }
                        ObjectField::Spread(expr) => {
                            match self.eval_expr_in_env(expr, env)? {
                                Value::Object(src) => {
                                    for (k, v) in src.borrow().iter() {
                                        map.insert(k.clone(), v.clone());
                                    }
                                }
                                other => return Err(format!(
                                    "Object spread: expected Object, got {}",
                                    other.type_name(),
                                )),
                            }
                        }
                    }
                }
                Ok(Value::Object(Rc::new(RefCell::new(map))))
            }

            Expr::Array(elements, _) => {
                let mut arr = Vec::new();
                for elem in elements {
                    arr.push(self.eval_expr_in_env(elem, env)?);
                }
                Ok(Value::Array(Rc::new(RefCell::new(arr))))
            }

            Expr::Assign { target, value, .. } => {
                let val = self.eval_expr_in_env(value, env)?;
                if env.set(target, val.clone()) {
                    Ok(val)
                } else if self.global_env.set(target, val.clone()) {
                    Ok(val)
                } else {
                    Err(format!("Cannot assign to '{}': not a mutable binding", target))
                }
            }

            Expr::Is { expr, pattern, .. } => {
                let val = self.eval_expr_in_env(expr, env)?;
                Ok(Value::Bool(self.check_is(&val, pattern)))
            }

            Expr::Has { expr, pattern, .. } => {
                let val = self.eval_expr_in_env(expr, env)?;
                Ok(Value::Bool(self.check_has(&val, pattern)))
            }

            Expr::TupleArgs(exprs, _) => {
                // Evaluate as array when used as expression
                let mut vals = Vec::new();
                for e in exprs {
                    vals.push(self.eval_expr_in_env(e, env)?);
                }
                Ok(Value::Array(Rc::new(RefCell::new(vals))))
            }
        }
    }

    fn eval_binary_op(&self, left: &Value, op: &BinOp, right: &Value) -> Result<Value, String> {
        match op {
            BinOp::Add => self.eval_add(left, right),
            BinOp::Sub => self.eval_sub(left, right),
            BinOp::Mul => self.eval_mul(left, right),
            BinOp::Div => self.eval_div(left, right),
            BinOp::Mod => self.eval_mod(left, right),
            BinOp::Eq => Ok(Value::Bool(left.deep_eq(right))),
            BinOp::NotEq => Ok(Value::Bool(!left.deep_eq(right))),
            BinOp::Lt => self.eval_cmp(left, right, |a, b| a < b, |a, b| a < b),
            BinOp::LtEq => self.eval_cmp(left, right, |a, b| a <= b, |a, b| a <= b),
            BinOp::Gt => self.eval_cmp(left, right, |a, b| a > b, |a, b| a > b),
            BinOp::GtEq => self.eval_cmp(left, right, |a, b| a >= b, |a, b| a >= b),
            BinOp::And | BinOp::Or => unreachable!("handled above"),
        }
    }

    fn eval_add(&self, left: &Value, right: &Value) -> Result<Value, String> {
        match (left, right) {
            (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a + b)),
            (Value::Float(a), Value::Float(b)) => Ok(Value::Float(a + b)),
            (Value::Int(a), Value::Float(b)) => Ok(Value::Float(*a as f64 + b)),
            (Value::Float(a), Value::Int(b)) => Ok(Value::Float(a + *b as f64)),
            (Value::String(_), Value::String(_)) => {
                Err("String concatenation with + is not supported; use interpolation: \"${a}${b}\"".to_string())
            }
            _ => Err(format!("Cannot add {} and {}", left.type_name(), right.type_name())),
        }
    }

    fn eval_sub(&self, left: &Value, right: &Value) -> Result<Value, String> {
        match (left, right) {
            (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a - b)),
            (Value::Float(a), Value::Float(b)) => Ok(Value::Float(a - b)),
            (Value::Int(a), Value::Float(b)) => Ok(Value::Float(*a as f64 - b)),
            (Value::Float(a), Value::Int(b)) => Ok(Value::Float(a - *b as f64)),
            _ => Err(format!("Cannot subtract {} and {}", left.type_name(), right.type_name())),
        }
    }

    fn eval_mul(&self, left: &Value, right: &Value) -> Result<Value, String> {
        match (left, right) {
            (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a * b)),
            (Value::Float(a), Value::Float(b)) => Ok(Value::Float(a * b)),
            (Value::Int(a), Value::Float(b)) => Ok(Value::Float(*a as f64 * b)),
            (Value::Float(a), Value::Int(b)) => Ok(Value::Float(a * *b as f64)),
            _ => Err(format!("Cannot multiply {} and {}", left.type_name(), right.type_name())),
        }
    }

    fn eval_div(&self, left: &Value, right: &Value) -> Result<Value, String> {
        match (left, right) {
            (Value::Int(a), Value::Int(b)) => {
                if *b == 0 {
                    Err("Runtime error: integer division by zero".to_string())
                } else {
                    Ok(Value::Int(a / b))
                }
            }
            (Value::Float(a), Value::Float(b)) => Ok(Value::Float(a / b)),
            (Value::Int(a), Value::Float(b)) => Ok(Value::Float(*a as f64 / b)),
            (Value::Float(a), Value::Int(b)) => Ok(Value::Float(a / *b as f64)),
            _ => Err(format!("Cannot divide {} and {}", left.type_name(), right.type_name())),
        }
    }

    fn eval_mod(&self, left: &Value, right: &Value) -> Result<Value, String> {
        match (left, right) {
            (Value::Int(a), Value::Int(b)) => {
                if *b == 0 {
                    Err("Runtime error: integer modulo by zero".to_string())
                } else {
                    Ok(Value::Int(a % b))
                }
            }
            (Value::Float(a), Value::Float(b)) => Ok(Value::Float(a % b)),
            (Value::Int(a), Value::Float(b)) => Ok(Value::Float(*a as f64 % b)),
            (Value::Float(a), Value::Int(b)) => Ok(Value::Float(a % *b as f64)),
            _ => Err(format!("Cannot modulo {} and {}", left.type_name(), right.type_name())),
        }
    }

    fn eval_cmp(&self, left: &Value, right: &Value, int_cmp: fn(i64, i64) -> bool, float_cmp: fn(f64, f64) -> bool) -> Result<Value, String> {
        match (left, right) {
            (Value::Int(a), Value::Int(b)) => Ok(Value::Bool(int_cmp(*a, *b))),
            (Value::Float(a), Value::Float(b)) => Ok(Value::Bool(float_cmp(*a, *b))),
            (Value::Int(a), Value::Float(b)) => Ok(Value::Bool(float_cmp(*a as f64, *b))),
            (Value::Float(a), Value::Int(b)) => Ok(Value::Bool(float_cmp(*a, *b as f64))),
            (Value::String(a), Value::String(b)) => Ok(Value::Bool(int_cmp(a.cmp(b) as i64, 0))),
            _ => Err(format!("Cannot compare {} and {}", left.type_name(), right.type_name())),
        }
    }

    fn eval_index(&self, obj: &Value, key: &Value) -> Result<Value, String> {
        match obj {
            Value::Null => Ok(Value::Null),
            Value::Object(map) => {
                let map = map.borrow();
                match key {
                    Value::String(k) => Ok(map.get(k.as_str()).cloned().unwrap_or(Value::Null)),
                    _ => Ok(map.get(&key.to_display_string()).cloned().unwrap_or(Value::Null)),
                }
            }
            Value::Array(arr) => {
                match key {
                    Value::Int(i) => {
                        let arr = arr.borrow();
                        let idx = if *i < 0 { arr.len() as i64 + i } else { *i } as usize;
                        if idx >= arr.len() {
                            Err(format!("Runtime error: array index {} out of bounds (length {})", i, arr.len()))
                        } else {
                            Ok(arr[idx].clone())
                        }
                    }
                    _ => Err(format!("Array index must be an integer, got {}", key.type_name())),
                }
            }
            Value::String(s) => {
                match key {
                    Value::Int(i) => {
                        let chars: Vec<char> = s.chars().collect();
                        let idx = if *i < 0 { chars.len() as i64 + i } else { *i } as usize;
                        if idx >= chars.len() {
                            Ok(Value::Null)
                        } else {
                            Ok(Value::String(Rc::new(chars[idx].to_string())))
                        }
                    }
                    _ => Ok(Value::Null),
                }
            }
            _ => Ok(Value::Null),
        }
    }

    fn call_value(&mut self, func: &Value, args: Vec<Value>, _caller_env: &mut Env) -> Result<Value, String> {
        match func {
            Value::Function(f) => {
                let arity = f.arity;
                if args.len() < arity {
                    // Partial application
                    Ok(Value::Partial(Rc::new(PartialApp {
                        func: func.clone(),
                        applied: args,
                    })))
                } else if args.len() == arity {
                    self.call_function(f, args)
                } else {
                    Err(format!("Too many arguments: expected {}, got {}", arity, args.len()))
                }
            }
            Value::Partial(p) => {
                let mut all_args = p.applied.clone();
                all_args.extend(args);
                self.call_value(&p.func, all_args, _caller_env)
            }
            Value::NativeFunction(nf) => {
                let name = nf.name.clone();
                let arity = nf.arity;

                if args.len() < arity {
                    return Ok(Value::Partial(Rc::new(PartialApp {
                        func: func.clone(),
                        applied: args,
                    })));
                }

                // Handle special built-ins that need interpreter access
                match name.as_str() {
                    "lin_print" => {
                        let output = args[0].to_display_string();
                        self.output.push(output.clone());
                        println!("{}", output);
                        return Ok(args[0].clone());
                    }
                    "__exit" => {
                        let code = match &args[0] {
                            Value::Int(n) => *n as i32,
                            _ => 1,
                        };
                        self.exit_code = Some(code);
                        return Err(format!("__exit:{}", code));
                    }
                    "lin_for" => {
                        return self.builtin_for(&args[0], &args[1]);
                    }
                    "lin_iter" => {
                        return self.builtin_iter(&args[0], &args[1], &args[2], &args[3]);
                    }
                    "lin_async" => {
                        return self.builtin_async(&args[0]);
                    }
                    "lin_await" => {
                        return self.builtin_await(&args[0]);
                    }
                    "lin_parallel" => {
                        return self.builtin_parallel(&args[0]);
                    }
                    "lin_race" => {
                        return self.builtin_race(&args[0]);
                    }
                    "lin_timeout" => {
                        return self.builtin_timeout(&args[0], &args[1]);
                    }
                    "lin_retry" => {
                        return self.builtin_retry(&args[0], &args[1]);
                    }
                    "lin_thread_pool" => {
                        return self.builtin_thread_pool(&args[0]);
                    }
                    "lin_worker" => {
                        return self.builtin_worker(&args[0], &args[1]);
                    }
                    "__ioLines" => {
                        return self.builtin_io_lines();
                    }
                    "__fsReadLines" => {
                        let path = match &args[0] {
                            Value::String(s) => s.as_ref().clone(),
                            _ => return Err("__fsReadLines: expected String path".to_string()),
                        };
                        return self.builtin_fs_read_lines(&path);
                    }
                    "__serverServe" => {
                        let port = match &args[0] {
                            Value::Int(p) => *p as u16,
                            _ => return Err("__serverServe: port must be Int".to_string()),
                        };
                        let handler = args[1].clone();
                        return self.builtin_server_serve(port, &handler);
                    }
                    "__serverServeWithPool" => {
                        let port = match &args[0] {
                            Value::Int(p) => *p as u16,
                            _ => return Err("__serverServeWithPool: port must be Int".to_string()),
                        };
                        let pool = args[1].clone();
                        let handler = args[2].clone();
                        return self.builtin_server_serve_with_pool(port, &pool, &handler);
                    }
                    _ => {}
                }

                if args.len() == arity {
                    let result = (nf.func)(&args)?;
                    Ok(result)
                } else {
                    Err(format!("Too many arguments to {}: expected {}, got {}", name, arity, args.len()))
                }
            }
            Value::Iterator(iter_val) => {
                // Calling an iterator with a callback is `for`
                if args.len() == 1 {
                    self.eval_for_iterator(iter_val, &args[0])
                } else {
                    Err("Iterator call expects 1 argument (callback)".to_string())
                }
            }
            Value::Promise(_) | Value::ThreadPool(_) | Value::Worker(_) => {
                Err(format!("Cannot call {} directly; use await/message/request methods", func.type_name()))
            }
            _ => Err(format!("Cannot call value of type {}", func.type_name())),
        }
    }

    fn call_function(&mut self, func: &Rc<Function>, args: Vec<Value>) -> Result<Value, String> {
        let mut current_args = args;

        loop {
            let mut call_env = Env::child(&func.closure);

            for (i, param) in func.params.iter().enumerate() {
                let val = current_args.get(i).cloned().unwrap_or(Value::Null);
                self.bind_pattern_in_env(&param.pattern, &val, &mut call_env)?;
            }

            if let Some(name) = &func.name {
                call_env.define(name.clone(), Value::Function(func.clone()));
            }

            match self.eval_tail_expr(&func.body, &mut call_env, func.name.as_deref())? {
                TailResult::Return(val) => return Ok(val),
                TailResult::TailCall(new_args) => {
                    current_args = new_args;
                }
            }
        }
    }

    fn eval_tail_expr(&mut self, expr: &Expr, env: &mut Env, self_name: Option<&str>) -> Result<TailResult, String> {
        match expr {
            Expr::Call { func, args, .. } => {
                if let Some(name) = self_name {
                    if let Expr::Ident(fn_name, _) = func.as_ref() {
                        if fn_name == name {
                            let mut arg_vals = Vec::new();
                            for arg in args {
                                arg_vals.push(self.eval_expr_in_env(arg, env)?);
                            }
                            return Ok(TailResult::TailCall(arg_vals));
                        }
                    }
                }
                let val = self.eval_expr_in_env(expr, env)?;
                Ok(TailResult::Return(val))
            }
            Expr::If { condition, then_branch, else_branch, .. } => {
                let cond = self.eval_expr_in_env(condition, env)?;
                if cond.is_truthy() {
                    self.eval_tail_expr(then_branch, env, self_name)
                } else {
                    self.eval_tail_expr(else_branch, env, self_name)
                }
            }
            Expr::Block(stmts, final_expr, _) => {
                let mut block_env = Env::child(env);
                for stmt in stmts {
                    self.eval_stmt_in_env(stmt, &mut block_env)?;
                }
                self.eval_tail_expr(final_expr, &mut block_env, self_name)
            }
            Expr::Match { scrutinee, arms, .. } => {
                let val = self.eval_expr_in_env(scrutinee, env)?;
                for arm in arms {
                    let mut arm_env = Env::child(env);
                    let matched = match &arm.pattern {
                        MatchPattern::Is(pattern) => self.match_is(&val, pattern, &mut arm_env),
                        MatchPattern::Has(pattern) => self.match_has(&val, pattern, &mut arm_env),
                        MatchPattern::Else => true,
                    };
                    if matched {
                        if let Some(guard) = &arm.guard {
                            let guard_val = self.eval_expr_in_env(guard, &mut arm_env)?;
                            if !guard_val.is_truthy() {
                                continue;
                            }
                        }
                        return self.eval_tail_expr(&arm.body, &mut arm_env, self_name);
                    }
                }
                Err("Runtime error: non-exhaustive match (no arm matched)".to_string())
            }
            _ => {
                let val = self.eval_expr_in_env(expr, env)?;
                Ok(TailResult::Return(val))
            }
        }
    }

    fn eval_for_iterator(&mut self, iter_val: &Rc<RefCell<IteratorValue>>, callback: &Value) -> Result<Value, String> {
        let iter = iter_val.borrow_mut();

        // Initialize state
        let mut state = self.call_value(&iter.init, vec![], &mut Env::new())?;

        loop {
            // Check continuation
            let cont = self.call_value(&iter.cont, vec![state.clone()], &mut Env::new())?;
            if !cont.is_truthy() {
                break;
            }

            // Get current value
            let current = self.call_value(&iter.current, vec![state.clone()], &mut Env::new())?;

            // Call the callback
            self.call_value(callback, vec![current], &mut Env::new())?;

            // Advance state
            state = self.call_value(&iter.next, vec![state], &mut Env::new())?;
        }

        Ok(Value::Null)
    }

    fn builtin_for(&mut self, iterable: &Value, callback: &Value) -> Result<Value, String> {
        match iterable {
            Value::Array(arr) => {
                let items: Vec<Value> = arr.borrow().clone();
                for item in items {
                    self.call_value(callback, vec![item], &mut Env::new())?;
                }
                Ok(Value::Null)
            }
            Value::Iterator(iter_val) => {
                self.eval_for_iterator(iter_val, callback)
            }
            _ => Err(format!("for: expected Array or Iterator, got {}", iterable.type_name())),
        }
    }

    fn builtin_iter(&mut self, init: &Value, cont: &Value, next: &Value, current: &Value) -> Result<Value, String> {
        let iter_val = IteratorValue {
            init: init.clone(),
            cont: cont.clone(),
            next: next.clone(),
            current: current.clone(),
            state: None,
            started: false,
        };
        Ok(Value::Iterator(Rc::new(RefCell::new(iter_val))))
    }

    fn eval_match(&mut self, val: &Value, arms: &[MatchArm], env: &mut Env) -> Result<Value, String> {
        for arm in arms {
            let mut arm_env = Env::child(env);
            let matched = match &arm.pattern {
                MatchPattern::Is(pattern) => self.match_is(val, pattern, &mut arm_env),
                MatchPattern::Has(pattern) => self.match_has(val, pattern, &mut arm_env),
                MatchPattern::Else => true,
            };

            if matched {
                if let Some(guard) = &arm.guard {
                    let guard_val = self.eval_expr_in_env(guard, &mut arm_env)?;
                    if !guard_val.is_truthy() {
                        continue;
                    }
                }
                return self.eval_expr_in_env(&arm.body, &mut arm_env);
            }
        }

        Err("Runtime error: non-exhaustive match (no arm matched)".to_string())
    }

    fn match_is(&self, val: &Value, pattern: &Pattern, env: &mut Env) -> bool {
        match pattern {
            Pattern::TypeName(ref name, _) => {
                match name.as_str() {
                    "Null" => matches!(val, Value::Null),
                    "Boolean" => matches!(val, Value::Bool(_)),
                    "Int32" | "Int64" | "Int" => matches!(val, Value::Int(_)),
                    "Float32" | "Float64" | "Float" => matches!(val, Value::Float(_)),
                    "String" => matches!(val, Value::String(_)),
                    "Array" => matches!(val, Value::Array(_)),
                    "Object" => matches!(val, Value::Object(_)),
                    "Function" => matches!(val, Value::Function(_) | Value::NativeFunction(_) | Value::Partial(_)),
                    "Iterator" => matches!(val, Value::Iterator(_)),
                    _ => false,
                }
            }
            Pattern::Literal(expr) => {
                match expr.as_ref() {
                    Expr::StringLit(s, _) => {
                        if let Value::String(vs) = val {
                            vs.as_str() == s
                        } else {
                            false
                        }
                    }
                    Expr::IntLit(i, _) => {
                        if let Value::Int(vi) = val {
                            *vi == *i
                        } else {
                            false
                        }
                    }
                    Expr::FloatLit(f, _) => {
                        if let Value::Float(vf) = val {
                            *vf == *f
                        } else {
                            false
                        }
                    }
                    Expr::BoolLit(b, _) => {
                        if let Value::Bool(vb) = val {
                            *vb == *b
                        } else {
                            false
                        }
                    }
                    _ => false,
                }
            }
            Pattern::Object(fields, _, _) => {
                if let Value::Object(obj) = val {
                    let obj = obj.borrow();
                    // is requires exact match: exactly these fields
                    if obj.len() != fields.len() {
                        return false;
                    }
                    for field in fields {
                        let key = field.key.as_ref().unwrap();
                        if let Some(v) = obj.get(key) {
                            env.define(key.clone(), v.clone());
                        } else {
                            return false;
                        }
                    }
                    true
                } else {
                    false
                }
            }
            Pattern::Array(elements, _, _) => {
                if let Value::Array(arr) = val {
                    let arr = arr.borrow();
                    if arr.len() != elements.len() {
                        return false;
                    }
                    for (i, pat) in elements.iter().enumerate() {
                        if let Pattern::Ident(name, _) = pat {
                            env.define(name.clone(), arr[i].clone());
                        }
                    }
                    true
                } else {
                    false
                }
            }
            _ => false,
        }
    }

    fn match_has(&self, val: &Value, pattern: &Pattern, env: &mut Env) -> bool {
        match pattern {
            Pattern::TypeName(_name, _) => {
                self.match_is(val, pattern, env)
            }
            Pattern::Object(fields, rest, _) => {
                if let Value::Object(obj) = val {
                    let obj = obj.borrow();
                    for field in fields {
                        let key = field.key.as_ref().unwrap();
                        if let Some(v) = obj.get(key) {
                            // Check value pattern (for tagged unions like "type": "success")
                            if let Some(value_pat) = &field.value_pattern {
                                match value_pat {
                                    Expr::StringLit(expected, _) => {
                                        if let Value::String(actual) = v {
                                            if actual.as_str() != expected {
                                                return false;
                                            }
                                        } else {
                                            return false;
                                        }
                                    }
                                    _ => {}
                                }
                            }
                            // Bind the field name
                            if let Pattern::Ident(name, _) = &field.pattern {
                                env.define(name.clone(), v.clone());
                            }
                        } else {
                            return false;
                        }
                    }
                    // Bind rest if present
                    if let Some(rest_name) = rest {
                        let mut rest_obj = IndexMap::new();
                        let field_keys: Vec<&String> = fields.iter().filter_map(|f| f.key.as_ref()).collect();
                        for (k, v) in obj.iter() {
                            if !field_keys.contains(&k) {
                                rest_obj.insert(k.clone(), v.clone());
                            }
                        }
                        env.define(rest_name.clone(), Value::Object(Rc::new(RefCell::new(rest_obj))));
                    }
                    true
                } else {
                    false
                }
            }
            Pattern::Array(elements, rest, _) => {
                if let Value::Array(arr) = val {
                    let arr = arr.borrow();
                    if arr.len() < elements.len() {
                        return false;
                    }
                    for (i, pat) in elements.iter().enumerate() {
                        if let Pattern::Ident(name, _) = pat {
                            env.define(name.clone(), arr[i].clone());
                        }
                    }
                    if let Some(rest_name) = rest {
                        let rest_arr: Vec<Value> = arr[elements.len()..].to_vec();
                        env.define(rest_name.clone(), Value::Array(Rc::new(RefCell::new(rest_arr))));
                    }
                    true
                } else {
                    false
                }
            }
            Pattern::Literal(_) => self.match_is(val, pattern, env),
            _ => false,
        }
    }

    fn check_is(&self, val: &Value, pattern: &Pattern) -> bool {
        let mut dummy_env = Env::new();
        self.match_is(val, pattern, &mut dummy_env)
    }

    fn check_has(&self, val: &Value, pattern: &Pattern) -> bool {
        let mut dummy_env = Env::new();
        self.match_has(val, pattern, &mut dummy_env)
    }

    fn bind_pattern_in_env(&mut self, pattern: &Pattern, value: &Value, env: &mut Env) -> Result<(), String> {
        match pattern {
            Pattern::Ident(name, _) => {
                // Check if the value is a function and give it a name for recursion
                let val = match value {
                    Value::Function(f) if f.name.is_none() => {
                        let mut named = (**f).clone();
                        named.name = Some(name.clone());
                        Value::Function(Rc::new(named))
                    }
                    _ => value.clone(),
                };
                env.define(name.clone(), val);
                Ok(())
            }
            Pattern::Object(fields, rest, _) => {
                if let Value::Object(obj) = value {
                    let obj = obj.borrow();
                    for field in fields {
                        let key = field.key.as_ref().unwrap();
                        let val = obj.get(key).cloned().unwrap_or(Value::Null);
                        self.bind_pattern_in_env(&field.pattern, &val, env)?;
                    }
                    if let Some(rest_name) = rest {
                        let mut rest_obj = IndexMap::new();
                        let field_keys: Vec<&String> = fields.iter().filter_map(|f| f.key.as_ref()).collect();
                        for (k, v) in obj.iter() {
                            if !field_keys.contains(&k) {
                                rest_obj.insert(k.clone(), v.clone());
                            }
                        }
                        env.define(rest_name.clone(), Value::Object(Rc::new(RefCell::new(rest_obj))));
                    }
                } else {
                    for field in fields {
                        if let Pattern::Ident(name, _) = &field.pattern {
                            env.define(name.clone(), Value::Null);
                        }
                    }
                }
                Ok(())
            }
            Pattern::Array(elements, rest, _) => {
                if let Value::Array(arr) = value {
                    let arr = arr.borrow();
                    for (i, pat) in elements.iter().enumerate() {
                        let val = arr.get(i).cloned().unwrap_or(Value::Null);
                        self.bind_pattern_in_env(pat, &val, env)?;
                    }
                    if let Some(rest_name) = rest {
                        let rest_arr: Vec<Value> = arr.get(elements.len()..).unwrap_or(&[]).to_vec();
                        env.define(rest_name.clone(), Value::Array(Rc::new(RefCell::new(rest_arr))));
                    }
                }
                Ok(())
            }
            Pattern::Wildcard(_) => Ok(()),
            Pattern::TypeName(_, _) | Pattern::Literal(_) => Ok(()),
        }
    }

    // ---- Concurrency built-ins ----

    /// Spawn one OS thread to evaluate a zero-argument closure.
    /// Returns a Promise that resolves to the result (or an error object).
    fn builtin_async(&mut self, thunk: &Value) -> Result<Value, String> {
        // Overloaded form: async([thunk1, thunk2, ...]) => Promise[]
        if let Value::Array(arr) = thunk {
            let funcs: Vec<Value> = arr.borrow().clone();
            let mut promises = Vec::new();
            for f in funcs {
                promises.push(self.builtin_async(&f)?);
            }
            return Ok(Value::Array(Rc::new(RefCell::new(promises))));
        }
        let func = match thunk {
            Value::Function(f) => f.clone(),
            Value::NativeFunction(_) | Value::Partial(_) => {
                return Err("async: argument must be a zero-argument function literal".to_string());
            }
            _ => return Err(format!("async: expected function, got {}", thunk.type_name())),
        };
        if func.arity != 0 {
            return Err(format!("async: thunk must take 0 arguments, got {}", func.arity));
        }
        let promise = Arc::new(Mutex::new(PromiseState::Pending));
        let promise_clone = promise.clone();
        let sendable = SendFunction::new(func);
        std::thread::spawn(move || {
            let func = unsafe { sendable.into_rc() };
            let mut interp = Interpreter::new();
            let result = interp.call_function(&func, vec![]);
            let state = match result {
                Ok(val) => match val.to_json_value() {
                    Ok(jv) => PromiseState::Resolved(jv),
                    Err(e) => PromiseState::Failed(format!("async: result not JSON-compatible: {}", e)),
                },
                Err(e) => PromiseState::Failed(e),
            };
            *promise_clone.lock().unwrap() = state;
        });
        Ok(Value::Promise(promise))
    }

    /// Block until a Promise resolves; return its value or an error object.
    fn builtin_await(&mut self, promise_val: &Value) -> Result<Value, String> {
        match promise_val {
            Value::Promise(p) => {
                // Spin-wait (simple; for a production impl, use condvar)
                loop {
                    let state = p.lock().unwrap();
                    match &*state {
                        PromiseState::Pending => {
                            drop(state);
                            std::thread::yield_now();
                        }
                        PromiseState::Resolved(jv) => return Ok(jv.clone().to_value()),
                        PromiseState::Failed(msg) => {
                            let mut obj = IndexMap::new();
                            obj.insert("type".to_string(), Value::String(Rc::new("error".to_string())));
                            obj.insert("message".to_string(), Value::String(Rc::new(msg.clone())));
                            return Ok(Value::Object(Rc::new(RefCell::new(obj))));
                        }
                    }
                }
            }
            Value::Array(arr) => {
                // await on an array of promises
                let promises: Vec<Value> = arr.borrow().clone();
                let mut results = Vec::new();
                for p in promises {
                    results.push(self.builtin_await(&p)?);
                }
                Ok(Value::Array(Rc::new(RefCell::new(results))))
            }
            _ => Err(format!("await: expected Promise or Promise[], got {}", promise_val.type_name())),
        }
    }

    /// Run an array of thunks in parallel; return array of results in input order.
    fn builtin_parallel(&mut self, thunks: &Value) -> Result<Value, String> {
        match thunks {
            Value::Array(arr) => {
                let funcs: Vec<Value> = arr.borrow().clone();
                // spawn all
                let mut promises = Vec::new();
                for f in funcs {
                    promises.push(self.builtin_async(&f)?);
                }
                // await all
                let mut results = Vec::new();
                for p in promises {
                    results.push(self.builtin_await(&p)?);
                }
                Ok(Value::Array(Rc::new(RefCell::new(results))))
            }
            _ => Err(format!("parallel: expected Array of thunks, got {}", thunks.type_name())),
        }
    }

    /// Resolve with the first promise to complete.
    fn builtin_race(&mut self, promises: &Value) -> Result<Value, String> {
        match promises {
            Value::Array(arr) => {
                let ps: Vec<Value> = arr.borrow().clone();
                loop {
                    for p in &ps {
                        if let Value::Promise(arc) = p {
                            let state = arc.lock().unwrap();
                            match &*state {
                                PromiseState::Resolved(jv) => return Ok(jv.clone().to_value()),
                                PromiseState::Failed(msg) => {
                                    let mut obj = IndexMap::new();
                                    obj.insert("type".to_string(), Value::String(Rc::new("error".to_string())));
                                    obj.insert("message".to_string(), Value::String(Rc::new(msg.clone())));
                                    return Ok(Value::Object(Rc::new(RefCell::new(obj))));
                                }
                                PromiseState::Pending => {}
                            }
                        }
                    }
                    std::thread::yield_now();
                }
            }
            _ => Err(format!("race: expected Array of promises, got {}", promises.type_name())),
        }
    }

    /// Resolve promise or return null if it doesn't complete within `ms` milliseconds.
    fn builtin_timeout(&mut self, promise_val: &Value, ms_val: &Value) -> Result<Value, String> {
        let ms = match ms_val {
            Value::Int(n) => *n as u64,
            _ => return Err("timeout: second argument must be Int32".to_string()),
        };
        let p = match promise_val {
            Value::Promise(p) => p.clone(),
            _ => return Err(format!("timeout: expected Promise, got {}", promise_val.type_name())),
        };
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(ms);
        loop {
            let state = p.lock().unwrap();
            match &*state {
                PromiseState::Resolved(jv) => return Ok(jv.clone().to_value()),
                PromiseState::Failed(msg) => {
                    let mut obj = IndexMap::new();
                    obj.insert("type".to_string(), Value::String(Rc::new("error".to_string())));
                    obj.insert("message".to_string(), Value::String(Rc::new(msg.clone())));
                    return Ok(Value::Object(Rc::new(RefCell::new(obj))));
                }
                PromiseState::Pending => {
                    drop(state);
                    if std::time::Instant::now() >= deadline {
                        return Ok(Value::Null);
                    }
                    std::thread::yield_now();
                }
            }
        }
    }

    /// Retry a thunk up to n times; return first non-error result.
    fn builtin_retry(&mut self, thunk: &Value, n_val: &Value) -> Result<Value, String> {
        let n = match n_val {
            Value::Int(n) => *n,
            _ => return Err("retry: second argument must be Int32".to_string()),
        };
        let mut last_err = Value::Null;
        for _ in 0..n {
            let p = self.builtin_async(thunk)?;
            let result = self.builtin_await(&p)?;
            match &result {
                Value::Object(o) => {
                    let o = o.borrow();
                    if o.get("type").map(|v| v.to_display_string()) == Some("error".to_string()) {
                        last_err = result.clone();
                        continue;
                    }
                    return Ok(Value::Promise({
                        let resolved = Arc::new(Mutex::new(PromiseState::Resolved(
                            result.to_json_value().unwrap_or(JsonValue::Null)
                        )));
                        resolved
                    }));
                }
                _ => {
                    return Ok(Value::Promise({
                        let resolved = Arc::new(Mutex::new(PromiseState::Resolved(
                            result.to_json_value().unwrap_or(JsonValue::Null)
                        )));
                        resolved
                    }));
                }
            }
        }
        // All attempts failed — wrap last error in a promise
        let p = Arc::new(Mutex::new(PromiseState::Failed(
            last_err.to_display_string()
        )));
        Ok(Value::Promise(p))
    }

    /// Create a thread pool of n worker threads.
    fn builtin_thread_pool(&mut self, n_val: &Value) -> Result<Value, String> {
        let n = match n_val {
            Value::Int(n) => *n as usize,
            _ => return Err("threadPool: argument must be Int32".to_string()),
        };
        let (tx, rx) = std::sync::mpsc::channel::<Box<dyn FnOnce() + Send + 'static>>();
        let rx = Arc::new(Mutex::new(rx));
        for _ in 0..n {
            let rx = rx.clone();
            std::thread::spawn(move || loop {
                let task = {
                    let lock = rx.lock().unwrap();
                    lock.recv()
                };
                match task {
                    Ok(f) => f(),
                    Err(_) => break, // channel closed
                }
            });
        }
        Ok(Value::ThreadPool(Arc::new(ThreadPoolState { sender: tx })))
    }

    /// Dispatch a thunk to a thread pool.
    fn pool_async(&mut self, pool: &Arc<ThreadPoolState>, args: &[Value]) -> Result<Value, String> {
        let thunk = args.first().ok_or("pool.async: requires 1 argument")?;
        let func = match thunk {
            Value::Function(f) => f.clone(),
            _ => return Err(format!("pool.async: expected function, got {}", thunk.type_name())),
        };
        if func.arity != 0 {
            return Err("pool.async: thunk must take 0 arguments".to_string());
        }
        let promise = Arc::new(Mutex::new(PromiseState::Pending));
        let promise_clone = promise.clone();
        let sendable = SendFunction::new(func);
        pool.sender.send(Box::new(move || {
            let func = unsafe { sendable.into_rc() };
            let mut interp = Interpreter::new();
            let result = interp.call_function(&func, vec![]);
            let state = match result {
                Ok(val) => match val.to_json_value() {
                    Ok(jv) => PromiseState::Resolved(jv),
                    Err(e) => PromiseState::Failed(e),
                },
                Err(e) => PromiseState::Failed(e),
            };
            *promise_clone.lock().unwrap() = state;
        })).map_err(|_| "pool.async: thread pool is shut down".to_string())?;
        Ok(Value::Promise(promise))
    }

    /// Create a stateful worker thread.
    fn builtin_worker(&mut self, on_msg: &Value, on_shutdown: &Value) -> Result<Value, String> {
        let handler = match on_msg {
            Value::Function(f) => f.clone(),
            _ => return Err(format!("worker: onMessage must be a function, got {}", on_msg.type_name())),
        };
        let shutdown_fn = match on_shutdown {
            Value::Function(f) => f.clone(),
            Value::NativeFunction(_) => {
                // Accept native functions for onShutdown
                let _f = on_shutdown.clone();
                // Create a synthetic empty function for shutdown
                handler.clone() // placeholder; we handle this below
            }
            Value::Null => handler.clone(), // no-op shutdown
            _ => return Err(format!("worker: onShutdown must be a function, got {}", on_shutdown.type_name())),
        };
        let (tx, rx) = std::sync::mpsc::sync_channel::<WorkerMsg>(64);
        let sendable_handler = SendFunction::new(handler);
        let sendable_shutdown = SendFunction::new(shutdown_fn);
        std::thread::spawn(move || {
            let handler = unsafe { sendable_handler.into_rc() };
            let shutdown_fn = unsafe { sendable_shutdown.into_rc() };
            let mut interp = Interpreter::new();
            loop {
                match rx.recv() {
                    Ok(WorkerMsg::Message(msg, reply_tx)) => {
                        let msg_val = msg.to_value();
                        let result = interp.call_function(&handler, vec![msg_val]);
                        if let Some(tx) = reply_tx {
                            let reply = match result {
                                Ok(v) => v.to_json_value().unwrap_or(JsonValue::Null),
                                Err(e) => JsonValue::Error(e),
                            };
                            let _ = tx.send(reply);
                        }
                    }
                    Ok(WorkerMsg::Shutdown) | Err(_) => {
                        let _ = interp.call_function(&shutdown_fn, vec![]);
                        break;
                    }
                }
            }
        });
        Ok(Value::Worker(Arc::new(WorkerState {
            sender: tx,
            closed: std::sync::atomic::AtomicBool::new(false),
        })))
    }

    /// Send a message to a worker (fire and forget).
    fn worker_message(&mut self, worker: &Arc<WorkerState>, msg: &Value) -> Result<Value, String> {
        if worker.closed.load(std::sync::atomic::Ordering::SeqCst) {
            return Err("worker.message: worker is closed".to_string());
        }
        let jv = msg.to_json_value().map_err(|e| format!("worker.message: {}", e))?;
        worker.sender.send(WorkerMsg::Message(jv, None))
            .map_err(|_| "worker.message: worker thread is gone".to_string())?;
        Ok(Value::Null)
    }

    /// Send a message to a worker and wait for a reply.
    fn worker_request(&mut self, worker: &Arc<WorkerState>, msg: &Value) -> Result<Value, String> {
        if worker.closed.load(std::sync::atomic::Ordering::SeqCst) {
            return Err("worker.request: worker is closed".to_string());
        }
        let jv = msg.to_json_value().map_err(|e| format!("worker.request: {}", e))?;
        let (reply_tx, reply_rx) = std::sync::mpsc::sync_channel(1);
        worker.sender.send(WorkerMsg::Message(jv, Some(reply_tx)))
            .map_err(|_| "worker.request: worker thread is gone".to_string())?;
        let reply = reply_rx.recv()
            .map_err(|_| "worker.request: worker thread disconnected".to_string())?;
        Ok(reply.to_value())
    }

    /// Close a worker (drain + shutdown).
    fn worker_close(&mut self, worker: &Arc<WorkerState>) -> Result<Value, String> {
        worker.closed.store(true, std::sync::atomic::Ordering::SeqCst);
        let _ = worker.sender.send(WorkerMsg::Shutdown);
        Ok(Value::Null)
    }

    /// Transform a promise's resolved value without blocking.
    fn promise_map(&mut self, promise: &Arc<Mutex<PromiseState>>, f: &Value) -> Result<Value, String> {
        let state = promise.lock().unwrap();
        match &*state {
            PromiseState::Resolved(jv) => {
                let val = jv.clone().to_value();
                drop(state);
                let result = self.call_value(f, vec![val], &mut Env::new())?;
                let new_jv = result.to_json_value().unwrap_or(JsonValue::Null);
                Ok(Value::Promise(Arc::new(Mutex::new(PromiseState::Resolved(new_jv)))))
            }
            PromiseState::Failed(msg) => {
                Ok(Value::Promise(Arc::new(Mutex::new(PromiseState::Failed(msg.clone())))))
            }
            PromiseState::Pending => {
                // For now, map on a pending promise is not supported in interpreter
                Err("promise.map on a Pending promise is not supported in the interpreter".to_string())
            }
        }
    }

    /// Start a single-threaded HTTP server that blocks forever.
    fn builtin_server_serve(&mut self, port: u16, handler: &Value) -> Result<Value, String> {
        let server = tiny_http::Server::http(format!("0.0.0.0:{}", port))
            .map_err(|e| format!("__serverServe: cannot bind port {}: {}", port, e))?;
        loop {
            match server.recv() {
                Ok(request) => {
                    let req_val = tiny_http_request_to_value(&request);
                    let response_val = self.call_value(handler, vec![req_val], &mut Env::new())
                        .unwrap_or_else(|e| {
                            let mut map = indexmap::IndexMap::new();
                            map.insert("status".to_string(), Value::Int(500));
                            map.insert("headers".to_string(), Value::Object(Rc::new(RefCell::new(indexmap::IndexMap::new()))));
                            map.insert("body".to_string(), Value::String(Rc::new(e)));
                            Value::Object(Rc::new(RefCell::new(map)))
                        });
                    let _ = send_http_response(request, &response_val);
                }
                Err(e) => return Err(format!("__serverServe: {}", e)),
            }
        }
    }

    /// Start a multi-threaded HTTP server using a thread pool; blocks forever.
    fn builtin_server_serve_with_pool(&mut self, port: u16, pool: &Value, handler: &Value) -> Result<Value, String> {
        let pool_arc = match pool {
            Value::ThreadPool(p) => p.clone(),
            _ => return Err("__serverServeWithPool: second argument must be a ThreadPool".to_string()),
        };
        let handler_func = match handler {
            Value::Function(f) => f.clone(),
            _ => return Err("__serverServeWithPool: handler must be a function".to_string()),
        };
        let server = tiny_http::Server::http(format!("0.0.0.0:{}", port))
            .map_err(|e| format!("__serverServeWithPool: cannot bind port {}: {}", port, e))?;
        for request in server.incoming_requests() {
            let req_val = tiny_http_request_to_value(&request);
            let jv = match req_val.to_json_value() {
                Ok(v) => v,
                Err(_) => JsonValue::Null,
            };
            let sendable = SendFunction::new(handler_func.clone());
            pool_arc.sender.send(Box::new(move || {
                let func = unsafe { sendable.into_rc() };
                let req = jv.to_value();
                let mut interp = Interpreter::new();
                let response_val = interp.call_function(&func, vec![req])
                    .unwrap_or_else(|e| {
                        let mut map = indexmap::IndexMap::new();
                        map.insert("status".to_string(), Value::Int(500));
                        map.insert("headers".to_string(), Value::Object(Rc::new(RefCell::new(indexmap::IndexMap::new()))));
                        map.insert("body".to_string(), Value::String(Rc::new(e)));
                        Value::Object(Rc::new(RefCell::new(map)))
                    });
                let _ = send_http_response(request, &response_val);
            })).map_err(|_| "pool.serve: thread pool is shut down")?;
        }
        Ok(Value::Null)
    }

    /// Read all stdin lines eagerly, return as Array<String>.
    pub fn builtin_io_lines(&mut self) -> Result<Value, String> {
        use std::io::BufRead;
        let stdin = std::io::stdin();
        let mut lines = Vec::new();
        for line in stdin.lock().lines() {
            match line {
                Ok(l) => lines.push(Value::String(Rc::new(l))),
                Err(e) => return Err(format!("__ioLines: {}", e)),
            }
        }
        Ok(Value::Array(Rc::new(RefCell::new(lines))))
    }

    /// Create an iterator over lines of a file.
    pub fn builtin_fs_read_lines(&mut self, path: &str) -> Result<Value, String> {
        use std::io::BufRead;
        match std::fs::File::open(path) {
            Err(e) => {
                let mut map = indexmap::IndexMap::new();
                map.insert("type".to_string(), Value::String(Rc::new("error".to_string())));
                map.insert("message".to_string(), Value::String(Rc::new(e.to_string())));
                Ok(Value::Object(Rc::new(RefCell::new(map))))
            }
            Ok(file) => {
                use std::io::BufReader;
                let reader = BufReader::new(file);
                let lines: Vec<Value> = reader.lines()
                    .map(|l| Value::String(Rc::new(l.unwrap_or_default())))
                    .collect();
                Ok(Value::Array(Rc::new(RefCell::new(lines))))
            }
        }
    }
}

/// Parse a JSON string into a Lin Value.
fn parse_json_to_value(s: &str) -> Result<Value, String> {
    let parsed: serde_json::Value = serde_json::from_str(s)
        .map_err(|e| format!("JSON parse error: {}", e))?;
    Ok(serde_json_to_lin_value(&parsed))
}

fn serde_json_to_lin_value(v: &serde_json::Value) -> Value {
    match v {
        serde_json::Value::Null => Value::Null,
        serde_json::Value::Bool(b) => Value::Bool(*b),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Value::Int(i)
            } else if let Some(f) = n.as_f64() {
                Value::Float(f)
            } else {
                Value::Null
            }
        }
        serde_json::Value::String(s) => Value::String(Rc::new(s.clone())),
        serde_json::Value::Array(arr) => {
            let items: Vec<Value> = arr.iter().map(serde_json_to_lin_value).collect();
            Value::Array(Rc::new(RefCell::new(items)))
        }
        serde_json::Value::Object(obj) => {
            let mut map = indexmap::IndexMap::new();
            for (k, v) in obj {
                map.insert(k.clone(), serde_json_to_lin_value(v));
            }
            Value::Object(Rc::new(RefCell::new(map)))
        }
    }
}

/// Serialize a Lin Value to a compact JSON string.
fn value_to_json_string(v: &Value) -> String {
    match v {
        Value::Null => "null".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Int(i) => i.to_string(),
        Value::Float(f) => {
            if f.is_nan() { "null".to_string() }
            else if f.is_infinite() { "null".to_string() }
            else { format!("{}", f) }
        }
        Value::String(s) => {
            serde_json::to_string(s.as_str()).unwrap_or_else(|_| format!("\"{}\"", s))
        }
        Value::Array(arr) => {
            let items: Vec<String> = arr.borrow().iter().map(value_to_json_string).collect();
            format!("[{}]", items.join(","))
        }
        Value::Object(obj) => {
            let fields: Vec<String> = obj.borrow().iter()
                .map(|(k, v)| format!("{}:{}", serde_json::to_string(k).unwrap_or_default(), value_to_json_string(v)))
                .collect();
            format!("{{{}}}", fields.join(","))
        }
        _ => "null".to_string(),
    }
}

/// Convert a tiny_http Request into a Lin Value with HttpRequest shape.
fn tiny_http_request_to_value(req: &tiny_http::Request) -> Value {
    let method = req.method().to_string();
    let url = req.url().to_string();
    let (path, query) = if let Some(idx) = url.find('?') {
        (url[..idx].to_string(), url[idx+1..].to_string())
    } else {
        (url.clone(), String::new())
    };

    let mut headers_map = indexmap::IndexMap::new();
    for header in req.headers() {
        headers_map.insert(
            header.field.to_string().to_lowercase(),
            Value::String(Rc::new(header.value.to_string())),
        );
    }

    let mut map = indexmap::IndexMap::new();
    map.insert("method".to_string(), Value::String(Rc::new(method)));
    map.insert("path".to_string(), Value::String(Rc::new(path)));
    map.insert("query".to_string(), Value::String(Rc::new(query)));
    map.insert("headers".to_string(), Value::Object(Rc::new(RefCell::new(headers_map))));
    map.insert("body".to_string(), Value::String(Rc::new(String::new())));
    Value::Object(Rc::new(RefCell::new(map)))
}

/// Write a Lin HttpResponse Value back to a tiny_http Request.
fn send_http_response(request: tiny_http::Request, response_val: &Value) -> Result<(), String> {
    let status = match response_val {
        Value::Object(o) => {
            match o.borrow().get("status") {
                Some(Value::Int(s)) => *s as u16,
                _ => 200,
            }
        }
        _ => 200,
    };
    let body = match response_val {
        Value::Object(o) => {
            match o.borrow().get("body").cloned() {
                Some(Value::String(s)) => s.as_ref().clone(),
                _ => String::new(),
            }
        }
        _ => String::new(),
    };
    let content_type = match response_val {
        Value::Object(o) => {
            let borrow = o.borrow();
            if let Some(Value::Object(hdrs)) = borrow.get("headers") {
                let hdrs_borrow = hdrs.borrow();
                let ct = hdrs_borrow.get("content-type")
                    .or_else(|| hdrs_borrow.get("Content-Type"));
                ct.and_then(|v| if let Value::String(s) = v { Some(s.as_ref().clone()) } else { None })
            } else {
                None
            }
        }
        _ => None,
    }.unwrap_or_else(|| "text/plain; charset=utf-8".to_string());

    let mut http_response = tiny_http::Response::from_string(body);
    http_response = http_response.with_status_code(status);
    http_response = http_response.with_header(
        tiny_http::Header::from_bytes("Content-Type", content_type.as_str()).unwrap()
    );
    request.respond(http_response).map_err(|e| e.to_string())
}
