use std::collections::HashMap;

use serde_json::Value;
use wasmtime::{Caller, Engine, Instance, Linker, Memory, Module, Store, TypedFunc};

use jsr_client::protocol::Message;

#[derive(Debug, thiserror::Error)]
pub enum OpaError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("wasmtime error: {0}")]
    Wasmtime(#[from] wasmtime::Error),
    #[error("opa policy not loaded")]
    NotLoaded,
    #[error("opa wasm missing export: {0}")]
    MissingExport(&'static str),
    #[error("opa eval failed: {0}")]
    Eval(String),
    #[error("opa result format invalid")]
    InvalidResult,
}

pub struct OpaResolver {
    policy_loaded: bool,
    policy_version: u64,
    opa_load_status: u8,
    logged_missing: bool,
    entrypoint: Option<String>,
    engine: Engine,
    module: Option<Module>,
    wasm: Option<OpaWasm>,
}

#[derive(Default)]
struct OpaRuntimeState {
    last_error: Option<String>,
    builtin_names: Vec<String>,
}

struct OpaWasm {
    store: Store<OpaRuntimeState>,
    instance: Instance,
    memory: Memory,
    entrypoints: HashMap<String, i32>,
    opa_malloc: TypedFunc<i32, i32>,
    opa_json_parse: TypedFunc<(i32, i32), i32>,
    opa_eval_ctx_new: TypedFunc<(), i32>,
    opa_eval_ctx_set_input: TypedFunc<(i32, i32), ()>,
    opa_eval_ctx_set_data: Option<TypedFunc<(i32, i32), ()>>,
    opa_eval_ctx_set_entrypoint: Option<TypedFunc<(i32, i32), ()>>,
    opa_eval_ctx_get_result: TypedFunc<i32, i32>,
    opa_eval: TypedFunc<i32, i32>,
    opa_value_dump: Option<TypedFunc<i32, i32>>,
    opa_json_dump: Option<TypedFunc<i32, i32>>,
    data_addr: Option<i32>,
}

impl OpaResolver {
    pub fn new() -> Self {
        Self {
            policy_loaded: false,
            policy_version: 0,
            opa_load_status: OPA_STATUS_ERROR,
            logged_missing: false,
            entrypoint: None,
            engine: Engine::default(),
            module: None,
            wasm: None,
        }
    }

    pub fn resolve_target(&mut self, msg: &Message) -> Result<Option<String>, OpaError> {
        let target = msg.meta.target.as_deref();
        if target.is_none() {
            return Ok(None);
        }
        if !self.policy_loaded && !self.logged_missing {
            self.logged_missing = true;
            tracing::warn!("opa policy not loaded; resolver disabled");
        }
        if !self.policy_loaded {
            return Err(OpaError::NotLoaded);
        }
        let input = serde_json::json!({
            "meta": &msg.meta,
            "routing": {
                "src": &msg.routing.src
            }
        });
        self.eval_target(&input)
    }

    pub fn reload(
        &mut self,
        version: u64,
        entrypoint: Option<String>,
        wasm_bytes: &[u8],
    ) -> Result<(), OpaError> {
        let prev_module = self.module.take();
        let prev_wasm = self.wasm.take();
        let prev_version = self.policy_version;
        let prev_loaded = self.policy_loaded;
        let prev_entrypoint = self.entrypoint.clone();

        self.opa_load_status = OPA_STATUS_LOADING;
        let result = (|| {
            if wasm_bytes.is_empty() {
                return Err(OpaError::NotLoaded);
            }
            let module = Module::from_binary(&self.engine, wasm_bytes)?;
            let wasm = OpaWasm::instantiate(&self.engine, &module)?;
            self.module = Some(module);
            self.wasm = Some(wasm);
            self.policy_loaded = true;
            self.policy_version = version;
            self.entrypoint = entrypoint;
            self.opa_load_status = OPA_STATUS_OK;
            self.logged_missing = false;
            tracing::info!(version = version, "opa policy loaded");
            Ok(())
        })();
        if let Err(err) = result {
            self.module = prev_module;
            self.wasm = prev_wasm;
            self.policy_loaded = prev_loaded;
            self.policy_version = prev_version;
            self.entrypoint = prev_entrypoint;
            self.opa_load_status = OPA_STATUS_ERROR;
            return Err(err);
        }
        Ok(())
    }

    pub fn status(&self) -> (u64, u8) {
        (self.policy_version, self.opa_load_status)
    }

    fn eval_target(&mut self, input: &Value) -> Result<Option<String>, OpaError> {
        let wasm = self.wasm.as_mut().ok_or(OpaError::NotLoaded)?;
        wasm.store.data_mut().last_error = None;

        let input_str = serde_json::to_string(input)?;
        let input_val = wasm.json_parse(&input_str)?;
        let ctx = wasm.opa_eval_ctx_new.call(&mut wasm.store, ())?;

        if let Some(data_addr) = wasm.data_addr {
            if let Some(set_data) = &wasm.opa_eval_ctx_set_data {
                set_data.call(&mut wasm.store, (ctx, data_addr))?;
            }
        }
        if let Some(set_entry) = &wasm.opa_eval_ctx_set_entrypoint {
            let entrypoint_id = self
                .entrypoint
                .as_ref()
                .and_then(|name| wasm.entrypoints.get(name).copied())
                .unwrap_or(0);
            set_entry.call(&mut wasm.store, (ctx, entrypoint_id))?;
        }
        wasm.opa_eval_ctx_set_input
            .call(&mut wasm.store, (ctx, input_val))?;

        let eval_rc = wasm.opa_eval.call(&mut wasm.store, ctx)?;
        if eval_rc != 0 {
            let err = wasm
                .store
                .data()
                .last_error
                .clone()
                .unwrap_or_else(|| format!("eval rc={eval_rc}"));
            return Err(OpaError::Eval(err));
        }

        let result_ptr = wasm.opa_eval_ctx_get_result.call(&mut wasm.store, ctx)?;
        let json = wasm.dump_value(result_ptr)?;
        parse_target_from_result(&json)
    }
}

const OPA_STATUS_OK: u8 = 0;
const OPA_STATUS_ERROR: u8 = 1;
const OPA_STATUS_LOADING: u8 = 2;

impl OpaWasm {
    fn instantiate(engine: &Engine, module: &Module) -> Result<Self, OpaError> {
        let mut store = Store::new(engine, OpaRuntimeState::default());
        let mut linker = Linker::new(engine);
        let mut imported_memory: Option<Memory> = None;

        for import in module.imports() {
            if import.module() == "env" && import.name() == "memory" {
                if let wasmtime::ExternType::Memory(mem_ty) = import.ty() {
                    let memory = Memory::new(&mut store, mem_ty)?;
                    linker.define(&mut store, "env", "memory", memory)?;
                    imported_memory = Some(memory);
                }
                break;
            }
        }

        linker.func_wrap(
            "env",
            "opa_abort",
            move |mut caller: Caller<'_, OpaRuntimeState>, ptr: i32| {
                if let Some(memory) = caller.get_export("memory").and_then(|e| e.into_memory()) {
                    let msg = read_cstr(&memory, &mut caller, ptr);
                    caller.data_mut().last_error = Some(msg);
                } else {
                    caller.data_mut().last_error = Some("opa_abort".to_string());
                }
            },
        )?;
        linker.func_wrap(
            "env",
            "opa_println",
            move |mut caller: Caller<'_, OpaRuntimeState>, ptr: i32| {
                if let Some(memory) = caller.get_export("memory").and_then(|e| e.into_memory()) {
                    let msg = read_cstr(&memory, &mut caller, ptr);
                    tracing::debug!("opa println: {msg}");
                }
            },
        )?;

        for (name, arity) in [
            ("opa_builtin0", 0),
            ("opa_builtin1", 1),
            ("opa_builtin2", 2),
            ("opa_builtin3", 3),
            ("opa_builtin4", 4),
            ("opa_builtin5", 5),
            ("opa_builtin6", 6),
        ] {
            match arity {
                0 => {
                    linker.func_wrap(
                        "env",
                        name,
                        |mut caller: Caller<'_, OpaRuntimeState>, _ctx: i32, builtin: i32| -> i32 {
                            eval_builtin(&mut caller, builtin, &[])
                        },
                    )?;
                }
                1 => {
                    linker.func_wrap(
                        "env",
                        name,
                        |mut caller: Caller<'_, OpaRuntimeState>,
                         _ctx: i32,
                         builtin: i32,
                         a0: i32|
                         -> i32 {
                            eval_builtin(&mut caller, builtin, &[a0])
                        },
                    )?;
                }
                2 => {
                    linker.func_wrap(
                        "env",
                        name,
                        |mut caller: Caller<'_, OpaRuntimeState>,
                         _ctx: i32,
                         builtin: i32,
                         a0: i32,
                         a1: i32|
                         -> i32 {
                            eval_builtin(&mut caller, builtin, &[a0, a1])
                        },
                    )?;
                }
                3 => {
                    linker.func_wrap(
                        "env",
                        name,
                        |mut caller: Caller<'_, OpaRuntimeState>,
                         _ctx: i32,
                         builtin: i32,
                         a0: i32,
                         a1: i32,
                         a2: i32|
                         -> i32 {
                            eval_builtin(&mut caller, builtin, &[a0, a1, a2])
                        },
                    )?;
                }
                4 => {
                    linker.func_wrap(
                        "env",
                        name,
                        |mut caller: Caller<'_, OpaRuntimeState>,
                         _ctx: i32,
                         builtin: i32,
                         a0: i32,
                         a1: i32,
                         a2: i32,
                         a3: i32|
                         -> i32 {
                            eval_builtin(&mut caller, builtin, &[a0, a1, a2, a3])
                        },
                    )?;
                }
                5 => {
                    linker.func_wrap(
                        "env",
                        name,
                        |mut caller: Caller<'_, OpaRuntimeState>,
                         _ctx: i32,
                         builtin: i32,
                         a0: i32,
                         a1: i32,
                         a2: i32,
                         a3: i32,
                         a4: i32|
                         -> i32 {
                            eval_builtin(&mut caller, builtin, &[a0, a1, a2, a3, a4])
                        },
                    )?;
                }
                _ => {
                    linker.func_wrap(
                        "env",
                        name,
                        |mut caller: Caller<'_, OpaRuntimeState>,
                         _ctx: i32,
                         builtin: i32,
                         a0: i32,
                         a1: i32,
                         a2: i32,
                         a3: i32,
                         a4: i32,
                         a5: i32|
                         -> i32 {
                            eval_builtin(&mut caller, builtin, &[a0, a1, a2, a3, a4, a5])
                        },
                    )?;
                }
            }
        }

        let instance = linker.instantiate(&mut store, module)?;
        let memory = instance
            .get_memory(&mut store, "memory")
            .or(imported_memory)
            .ok_or(OpaError::MissingExport("memory"))?;
        if let Some(builtins) = instance
            .get_typed_func::<(), i32>(&mut store, "builtins")
            .ok()
        {
            if let Ok(ptr) = builtins.call(&mut store, ()) {
                let json = read_cstr_store(&memory, &mut store, ptr);
                store.data_mut().builtin_names = parse_builtins(&json);
            }
        } else {
            tracing::warn!("opa wasm missing builtins export; builtin map unavailable");
        }
        let mut entrypoints = HashMap::new();
        if let Some(entrypoints_fn) = instance
            .get_typed_func::<(), i32>(&mut store, "entrypoints")
            .ok()
        {
            if let Ok(ptr) = entrypoints_fn.call(&mut store, ()) {
                let json = read_cstr_store(&memory, &mut store, ptr);
                entrypoints = parse_entrypoints(&json);
            }
        } else {
            tracing::warn!("opa wasm missing entrypoints export; entrypoint map unavailable");
        }
        let opa_malloc = instance
            .get_typed_func::<i32, i32>(&mut store, "opa_malloc")
            .map_err(|_| OpaError::MissingExport("opa_malloc"))?;
        let opa_json_parse = instance
            .get_typed_func::<(i32, i32), i32>(&mut store, "opa_json_parse")
            .map_err(|_| OpaError::MissingExport("opa_json_parse"))?;
        let opa_eval_ctx_new = instance
            .get_typed_func::<(), i32>(&mut store, "opa_eval_ctx_new")
            .map_err(|_| OpaError::MissingExport("opa_eval_ctx_new"))?;
        let opa_eval_ctx_set_input = instance
            .get_typed_func::<(i32, i32), ()>(&mut store, "opa_eval_ctx_set_input")
            .map_err(|_| OpaError::MissingExport("opa_eval_ctx_set_input"))?;
        let opa_eval_ctx_get_result = instance
            .get_typed_func::<i32, i32>(&mut store, "opa_eval_ctx_get_result")
            .map_err(|_| OpaError::MissingExport("opa_eval_ctx_get_result"))?;
        let opa_eval = match instance.get_typed_func::<i32, i32>(&mut store, "opa_eval") {
            Ok(func) => func,
            Err(_) => instance
                .get_typed_func::<i32, i32>(&mut store, "eval")
                .map_err(|_| OpaError::MissingExport("opa_eval/eval"))?,
        };

        let opa_eval_ctx_set_data = instance
            .get_typed_func::<(i32, i32), ()>(&mut store, "opa_eval_ctx_set_data")
            .ok();
        let opa_eval_ctx_set_entrypoint = instance
            .get_typed_func::<(i32, i32), ()>(&mut store, "opa_eval_ctx_set_entrypoint")
            .ok();
        let opa_value_dump = instance
            .get_typed_func::<i32, i32>(&mut store, "opa_value_dump")
            .ok();
        let opa_json_dump = instance
            .get_typed_func::<i32, i32>(&mut store, "opa_json_dump")
            .ok();

        let data_addr = instance
            .get_global(&mut store, "data")
            .and_then(|g| g.get(&mut store).i32());

        Ok(Self {
            store,
            instance,
            memory,
            entrypoints,
            opa_malloc,
            opa_json_parse,
            opa_eval_ctx_new,
            opa_eval_ctx_set_input,
            opa_eval_ctx_set_data,
            opa_eval_ctx_set_entrypoint,
            opa_eval_ctx_get_result,
            opa_eval,
            opa_value_dump,
            opa_json_dump,
            data_addr,
        })
    }

    fn json_parse(&mut self, value: &str) -> Result<i32, OpaError> {
        let ptr = self.opa_malloc.call(&mut self.store, value.len() as i32)?;
        if let Err(err) = self
            .memory
            .write(&mut self.store, ptr as usize, value.as_bytes())
        {
            return Err(OpaError::Eval(err.to_string()));
        }
        let addr = self
            .opa_json_parse
            .call(&mut self.store, (ptr, value.len() as i32))?;
        Ok(addr)
    }

    fn dump_value(&mut self, value_addr: i32) -> Result<String, OpaError> {
        if let Some(dump) = &self.opa_value_dump {
            let ptr = dump.call(&mut self.store, value_addr)?;
            return Ok(read_cstr_store(&self.memory, &mut self.store, ptr));
        }
        if let Some(dump) = &self.opa_json_dump {
            let ptr = dump.call(&mut self.store, value_addr)?;
            return Ok(read_cstr_store(&self.memory, &mut self.store, ptr));
        }
        Err(OpaError::MissingExport("opa_value_dump/opa_json_dump"))
    }
}

fn read_cstr(memory: &Memory, caller: &mut Caller<'_, OpaRuntimeState>, ptr: i32) -> String {
    let data = memory.data(caller);
    let mut end = ptr as usize;
    while end < data.len() && data[end] != 0 {
        end += 1;
    }
    String::from_utf8_lossy(&data[ptr as usize..end]).to_string()
}

fn read_cstr_store(memory: &Memory, store: &mut Store<OpaRuntimeState>, ptr: i32) -> String {
    let data = memory.data(store);
    let mut end = ptr as usize;
    while end < data.len() && data[end] != 0 {
        end += 1;
    }
    String::from_utf8_lossy(&data[ptr as usize..end]).to_string()
}

fn parse_target_from_result(json: &str) -> Result<Option<String>, OpaError> {
    let value: Value = serde_json::from_str(json)?;
    let target = extract_target(&value);
    Ok(target)
}

fn extract_target(value: &Value) -> Option<String> {
    match value {
        Value::Null => None,
        Value::String(s) => Some(s.clone()),
        Value::Array(items) => items.iter().find_map(extract_target),
        Value::Object(map) => {
            if let Some(result) = map.get("result") {
                if let Some(target) = extract_target(result) {
                    return Some(target);
                }
            }
            if let Some(expressions) = map.get("expressions") {
                if let Some(target) = extract_target(expressions) {
                    return Some(target);
                }
            }
            if let Some(value) = map.get("value") {
                if let Some(target) = extract_target(value) {
                    return Some(target);
                }
            }
            if let Some(target) = map.get("target") {
                return extract_target(target);
            }
            None
        }
        _ => None,
    }
}

fn parse_builtins(json: &str) -> Vec<String> {
    let value: Result<Value, _> = serde_json::from_str(json);
    let Ok(value) = value else {
        return Vec::new();
    };
    match value {
        Value::Array(items) => items
            .into_iter()
            .filter_map(|item| match item {
                Value::String(name) => Some(name),
                Value::Object(map) => map
                    .get("name")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn parse_entrypoints(json: &str) -> HashMap<String, i32> {
    let value: Result<Value, _> = serde_json::from_str(json);
    let Ok(value) = value else {
        return HashMap::new();
    };
    match value {
        Value::Object(map) => map
            .into_iter()
            .filter_map(|(key, value)| value.as_i64().map(|id| (key, id as i32)))
            .collect(),
        _ => HashMap::new(),
    }
}

fn eval_builtin(caller: &mut Caller<'_, OpaRuntimeState>, builtin_id: i32, args: &[i32]) -> i32 {
    let mut name = caller
        .data()
        .builtin_names
        .get(builtin_id as usize)
        .cloned()
        .unwrap_or_else(|| format!("builtin#{builtin_id}"));
    name = normalize_builtin_name(&name);
    let values: Result<Vec<Value>, _> = args.iter().map(|addr| dump_value(caller, *addr)).collect();
    let Ok(values) = values else {
        caller.data_mut().last_error = Some(format!("builtin {name} args parse failed"));
        return 0;
    };
    let result = match name.as_str() {
        "regex.match" => match (values.get(0), values.get(1)) {
            (Some(Value::String(pattern)), Some(Value::String(text))) => {
                match regex::Regex::new(pattern) {
                    Ok(re) => Ok(Value::Bool(re.is_match(text))),
                    Err(err) => Err(format!("regex.match error: {err}")),
                }
            }
            _ => Err("regex.match expects (string, string)".to_string()),
        },
        "regex.is_valid" => match values.get(0) {
            Some(Value::String(pattern)) => Ok(Value::Bool(regex::Regex::new(pattern).is_ok())),
            _ => Err("regex.is_valid expects (string)".to_string()),
        },
        "regex.find" => match (values.get(0), values.get(1)) {
            (Some(Value::String(pattern)), Some(Value::String(text))) => {
                match regex::Regex::new(pattern) {
                    Ok(re) => Ok(Value::String(
                        re.find(text)
                            .map(|m| m.as_str().to_string())
                            .unwrap_or_default(),
                    )),
                    Err(err) => Err(format!("regex.find error: {err}")),
                }
            }
            _ => Err("regex.find expects (string,string)".to_string()),
        },
        "regex.find_n" => match (values.get(0), values.get(1), values.get(2)) {
            (Some(Value::String(pattern)), Some(Value::String(text)), Some(Value::Number(n))) => {
                if let Some(n) = n.as_i64() {
                    match regex::Regex::new(pattern) {
                        Ok(re) => {
                            let matches: Vec<Value> = re
                                .find_iter(text)
                                .take(n.max(0) as usize)
                                .map(|m| Value::String(m.as_str().to_string()))
                                .collect();
                            Ok(Value::Array(matches))
                        }
                        Err(err) => Err(format!("regex.find_n error: {err}")),
                    }
                } else {
                    Err("regex.find_n expects integer n".to_string())
                }
            }
            _ => Err("regex.find_n expects (string,string,number)".to_string()),
        },
        "regex.replace" => match (values.get(0), values.get(1), values.get(2)) {
            (
                Some(Value::String(pattern)),
                Some(Value::String(text)),
                Some(Value::String(repl)),
            ) => match regex::Regex::new(pattern) {
                Ok(re) => Ok(Value::String(
                    re.replace_all(text, repl.as_str()).to_string(),
                )),
                Err(err) => Err(format!("regex.replace error: {err}")),
            },
            _ => Err("regex.replace expects (string,string,string)".to_string()),
        },
        "regex.find_all_string_submatch_n" => match (values.get(0), values.get(1), values.get(2)) {
            (Some(Value::String(pattern)), Some(Value::String(text)), Some(Value::Number(n))) => {
                if let Some(n) = n.as_i64() {
                    match regex::Regex::new(pattern) {
                        Ok(re) => {
                            let mut out = Vec::new();
                            for caps in re.captures_iter(text).take(n.max(0) as usize) {
                                let mut row = Vec::new();
                                for idx in 0..caps.len() {
                                    row.push(Value::String(
                                        caps.get(idx).map(|m| m.as_str()).unwrap_or("").to_string(),
                                    ));
                                }
                                out.push(Value::Array(row));
                            }
                            Ok(Value::Array(out))
                        }
                        Err(err) => Err(format!("regex.find_all_string_submatch_n error: {err}")),
                    }
                } else {
                    Err("regex.find_all_string_submatch_n expects integer n".to_string())
                }
            }
            _ => Err("regex.find_all_string_submatch_n expects (string,string,number)".to_string()),
        },
        "regex.split" => match (values.get(0), values.get(1)) {
            (Some(Value::String(pattern)), Some(Value::String(text))) => {
                match regex::Regex::new(pattern) {
                    Ok(re) => {
                        let parts = re
                            .split(text)
                            .map(|s| Value::String(s.to_string()))
                            .collect();
                        Ok(Value::Array(parts))
                    }
                    Err(err) => Err(format!("regex.split error: {err}")),
                }
            }
            _ => Err("regex.split expects (string,string)".to_string()),
        },
        "glob.match" => match (values.get(0), values.get(1), values.get(2)) {
            (Some(Value::String(pattern)), Some(delims), Some(Value::String(text))) => {
                let delims = match delims {
                    Value::Array(items) => items
                        .iter()
                        .filter_map(|item| item.as_str())
                        .flat_map(|s| s.chars())
                        .collect::<Vec<char>>(),
                    Value::String(value) => value.chars().collect::<Vec<char>>(),
                    _ => Vec::new(),
                };
                let regex = glob_to_regex(pattern, &delims);
                match regex::Regex::new(&regex) {
                    Ok(re) => Ok(Value::Bool(re.is_match(text))),
                    Err(err) => Err(format!("glob.match error: {err}")),
                }
            }
            _ => Err("glob.match expects (string, array|string, string)".to_string()),
        },
        "glob.quote_meta" => match values.get(0) {
            Some(Value::String(text)) => Ok(Value::String(glob_quote_meta(text))),
            _ => Err("glob.quote_meta expects (string)".to_string()),
        },
        "contains" => match (values.get(0), values.get(1)) {
            (Some(Value::String(hay)), Some(Value::String(needle))) => {
                Ok(Value::Bool(hay.contains(needle)))
            }
            (Some(Value::Array(list)), Some(value)) => {
                Ok(Value::Bool(list.iter().any(|item| item == value)))
            }
            _ => Err("contains expects (string,string) or (array,any)".to_string()),
        },
        "split" | "strings.split" => match (values.get(0), values.get(1)) {
            (Some(Value::String(text)), Some(Value::String(sep))) => {
                let parts = text
                    .split(sep)
                    .map(|s| Value::String(s.to_string()))
                    .collect();
                Ok(Value::Array(parts))
            }
            _ => Err("split expects (string,string)".to_string()),
        },
        "concat" => match (values.get(0), values.get(1)) {
            (Some(Value::String(sep)), Some(Value::Array(items))) => {
                let mut out = Vec::new();
                let mut valid = true;
                for item in items {
                    match item {
                        Value::String(value) => out.push(value.clone()),
                        _ => {
                            valid = false;
                            break;
                        }
                    }
                }
                if !valid {
                    Err("concat expects array of strings".to_string())
                } else {
                    Ok(Value::String(out.join(sep)))
                }
            }
            _ => Err("concat expects (string,array)".to_string()),
        },
        "startswith" => match (values.get(0), values.get(1)) {
            (Some(Value::String(text)), Some(Value::String(prefix))) => {
                Ok(Value::Bool(text.starts_with(prefix)))
            }
            _ => Err("startswith expects (string,string)".to_string()),
        },
        "endswith" => match (values.get(0), values.get(1)) {
            (Some(Value::String(text)), Some(Value::String(suffix))) => {
                Ok(Value::Bool(text.ends_with(suffix)))
            }
            _ => Err("endswith expects (string,string)".to_string()),
        },
        "lower" => match values.get(0) {
            Some(Value::String(text)) => Ok(Value::String(text.to_lowercase())),
            _ => Err("lower expects (string)".to_string()),
        },
        "upper" => match values.get(0) {
            Some(Value::String(text)) => Ok(Value::String(text.to_uppercase())),
            _ => Err("upper expects (string)".to_string()),
        },
        "trim" => match values.get(0) {
            Some(Value::String(text)) => Ok(Value::String(text.trim().to_string())),
            _ => Err("trim expects (string)".to_string()),
        },
        "trim_left" => match values.get(0) {
            Some(Value::String(text)) => Ok(Value::String(text.trim_start().to_string())),
            _ => Err("trim_left expects (string)".to_string()),
        },
        "trim_right" => match values.get(0) {
            Some(Value::String(text)) => Ok(Value::String(text.trim_end().to_string())),
            _ => Err("trim_right expects (string)".to_string()),
        },
        "replace" => match (values.get(0), values.get(1), values.get(2)) {
            (Some(Value::String(text)), Some(Value::String(old)), Some(Value::String(new))) => {
                Ok(Value::String(text.replace(old, new)))
            }
            _ => Err("replace expects (string,string,string)".to_string()),
        },
        "substring" => {
            if values.len() < 2 {
                Err("substring expects (string,number[,number])".to_string())
            } else {
                match (values.get(0), values.get(1), values.get(2)) {
                    (
                        Some(Value::String(text)),
                        Some(Value::Number(start)),
                        Some(Value::Number(len)),
                    ) => match (start.as_i64(), len.as_i64()) {
                        (Some(start), Some(len)) => {
                            let chars: Vec<char> = text.chars().collect();
                            let start = start.max(0) as usize;
                            let end = (start + len.max(0) as usize).min(chars.len());
                            Ok(Value::String(chars[start..end].iter().collect()))
                        }
                        _ => Err("substring expects integer start/len".to_string()),
                    },
                    (Some(Value::String(text)), Some(Value::Number(start)), None) => {
                        if let Some(start) = start.as_i64() {
                            let chars: Vec<char> = text.chars().collect();
                            let start = start.max(0) as usize;
                            Ok(Value::String(chars[start..].iter().collect()))
                        } else {
                            Err("substring expects integer start".to_string())
                        }
                    }
                    _ => Err("substring expects (string,number[,number])".to_string()),
                }
            }
        }
        "indexof" => match (values.get(0), values.get(1)) {
            (Some(Value::String(text)), Some(Value::String(needle))) => Ok(Value::Number(
                (text.find(needle).map(|idx| idx as i64).unwrap_or(-1)).into(),
            )),
            _ => Err("indexof expects (string,string)".to_string()),
        },
        "array.length" => match values.get(0) {
            Some(Value::Array(items)) => Ok(Value::Number((items.len() as i64).into())),
            _ => Err("array.length expects (array)".to_string()),
        },
        "sprintf" => match (values.get(0), values.get(1)) {
            (Some(Value::String(fmt)), Some(Value::Array(args))) => sprintf_simple(fmt, args),
            _ => Err("sprintf expects (string,array)".to_string()),
        },
        "count" => match values.get(0) {
            Some(Value::Array(items)) => Ok(Value::Number((items.len() as i64).into())),
            Some(Value::Object(map)) => Ok(Value::Number((map.len() as i64).into())),
            Some(Value::String(text)) => Ok(Value::Number((text.chars().count() as i64).into())),
            _ => Err("count expects (array|object|string)".to_string()),
        },
        "sum" => sum_numbers(&values, "sum"),
        "product" => product_numbers(&values, "product"),
        "max" => max_numbers(&values, "max"),
        "min" => min_numbers(&values, "min"),
        "array.concat" => match (values.get(0), values.get(1)) {
            (Some(Value::Array(a)), Some(Value::Array(b))) => {
                let mut out = a.clone();
                out.extend(b.iter().cloned());
                Ok(Value::Array(out))
            }
            _ => Err("array.concat expects (array,array)".to_string()),
        },
        "array.slice" => match (values.get(0), values.get(1), values.get(2)) {
            (Some(Value::Array(items)), Some(Value::Number(start)), Some(Value::Number(end))) => {
                match (start.as_i64(), end.as_i64()) {
                    (Some(start), Some(end)) => {
                        let start = start.max(0) as usize;
                        let end = end.max(start as i64) as usize;
                        Ok(Value::Array(
                            items
                                .iter()
                                .skip(start)
                                .take(end.saturating_sub(start))
                                .cloned()
                                .collect(),
                        ))
                    }
                    _ => Err("array.slice expects integer start/end".to_string()),
                }
            }
            _ => Err("array.slice expects (array,number,number)".to_string()),
        },
        "array.reverse" => match values.get(0) {
            Some(Value::Array(items)) => {
                let mut out = items.clone();
                out.reverse();
                Ok(Value::Array(out))
            }
            _ => Err("array.reverse expects (array)".to_string()),
        },
        "array.append" => match (values.get(0), values.get(1)) {
            (Some(Value::Array(items)), Some(value)) => {
                let mut out = items.clone();
                out.push(value.clone());
                Ok(Value::Array(out))
            }
            _ => Err("array.append expects (array,any)".to_string()),
        },
        "array.contains" => match (values.get(0), values.get(1)) {
            (Some(Value::Array(items)), Some(value)) => {
                Ok(Value::Bool(items.iter().any(|item| item == value)))
            }
            _ => Err("array.contains expects (array,any)".to_string()),
        },
        "object.get" => match (values.get(0), values.get(1), values.get(2)) {
            (Some(Value::Object(map)), Some(Value::String(key)), Some(default)) => {
                Ok(map.get(key).cloned().unwrap_or_else(|| default.clone()))
            }
            (Some(Value::Object(map)), Some(Value::String(key)), None) => {
                Ok(map.get(key).cloned().unwrap_or(Value::Null))
            }
            _ => Err("object.get expects (object,string,any?)".to_string()),
        },
        "object.keys" => match values.get(0) {
            Some(Value::Object(map)) => Ok(Value::Array(
                map.keys().cloned().map(Value::String).collect(),
            )),
            _ => Err("object.keys expects (object)".to_string()),
        },
        "object.values" => match values.get(0) {
            Some(Value::Object(map)) => Ok(Value::Array(map.values().cloned().collect())),
            _ => Err("object.values expects (object)".to_string()),
        },
        "object.remove" => match (values.get(0), values.get(1)) {
            (Some(Value::Object(map)), Some(Value::String(key))) => {
                let mut out = map.clone();
                out.remove(key);
                Ok(Value::Object(out))
            }
            (Some(Value::Object(map)), Some(Value::Array(keys))) => {
                let mut out = map.clone();
                for key in keys {
                    if let Some(key) = key.as_str() {
                        out.remove(key);
                    }
                }
                Ok(Value::Object(out))
            }
            _ => Err("object.remove expects (object,string|array)".to_string()),
        },
        "object.union" => match (values.get(0), values.get(1)) {
            (Some(Value::Object(a)), Some(Value::Object(b))) => {
                let mut out = a.clone();
                for (k, v) in b {
                    out.insert(k.clone(), v.clone());
                }
                Ok(Value::Object(out))
            }
            _ => Err("object.union expects (object,object)".to_string()),
        },
        "object.subset" => match (values.get(0), values.get(1)) {
            (Some(Value::Object(a)), Some(Value::Object(b))) => {
                Ok(Value::Bool(a.iter().all(|(k, v)| b.get(k) == Some(v))))
            }
            _ => Err("object.subset expects (object,object)".to_string()),
        },
        "json.marshal" => match values.get(0) {
            Some(value) => match serde_json::to_string(value) {
                Ok(text) => Ok(Value::String(text)),
                Err(err) => Err(format!("json.marshal error: {err}")),
            },
            _ => Err("json.marshal expects (any)".to_string()),
        },
        "json.unmarshal" => match values.get(0) {
            Some(Value::String(text)) => match serde_json::from_str(text) {
                Ok(value) => Ok(value),
                Err(err) => Err(format!("json.unmarshal error: {err}")),
            },
            _ => Err("json.unmarshal expects (string)".to_string()),
        },
        "json.is_valid" => match values.get(0) {
            Some(Value::String(text)) => {
                Ok(Value::Bool(serde_json::from_str::<Value>(text).is_ok()))
            }
            _ => Err("json.is_valid expects (string)".to_string()),
        },
        "is_null" => Ok(Value::Bool(values.get(0).map_or(false, |v| v.is_null()))),
        "is_boolean" => Ok(Value::Bool(values.get(0).map_or(false, |v| v.is_boolean()))),
        "is_number" => Ok(Value::Bool(values.get(0).map_or(false, |v| v.is_number()))),
        "is_string" => Ok(Value::Bool(values.get(0).map_or(false, |v| v.is_string()))),
        "is_array" => Ok(Value::Bool(values.get(0).map_or(false, |v| v.is_array()))),
        "is_object" => Ok(Value::Bool(values.get(0).map_or(false, |v| v.is_object()))),
        "type_name" => match values.get(0) {
            Some(Value::Null) => Ok(Value::String("null".to_string())),
            Some(Value::Bool(_)) => Ok(Value::String("boolean".to_string())),
            Some(Value::Number(_)) => Ok(Value::String("number".to_string())),
            Some(Value::String(_)) => Ok(Value::String("string".to_string())),
            Some(Value::Array(_)) => Ok(Value::String("array".to_string())),
            Some(Value::Object(_)) => Ok(Value::String("object".to_string())),
            None => Err("type_name expects (any)".to_string()),
        },
        _ => Err(format!("builtin not implemented: {name}")),
    };

    match result {
        Ok(value) => json_to_opa_value(caller, &value).unwrap_or(0),
        Err(err) => {
            caller.data_mut().last_error = Some(err);
            0
        }
    }
}

fn normalize_builtin_name(name: &str) -> String {
    if let Some(stripped) = name.strip_prefix("strings.") {
        return match stripped {
            "contains" => "contains",
            "has_prefix" | "startswith" | "starts_with" => "startswith",
            "has_suffix" | "endswith" | "ends_with" => "endswith",
            "lower" | "to_lower" | "lowercase" => "lower",
            "upper" | "to_upper" | "uppercase" => "upper",
            "trim" | "trim_space" => "trim",
            "trim_left" | "trim_left_space" | "trim_start" => "trim_left",
            "trim_right" | "trim_right_space" | "trim_end" => "trim_right",
            "replace" => "replace",
            "substring" => "substring",
            "indexof" => "indexof",
            "split" => "strings.split",
            "concat" => "concat",
            _ => stripped,
        }
        .to_string();
    }
    name.to_string()
}

fn glob_to_regex(pattern: &str, delims: &[char]) -> String {
    let mut out = String::from("^");
    let mut chars = pattern.chars().peekable();
    let delim_class = if delims.is_empty() {
        String::new()
    } else {
        let mut class = String::new();
        for ch in delims {
            class.push_str(&regex::escape(&ch.to_string()));
        }
        class
    };
    while let Some(ch) = chars.next() {
        match ch {
            '*' => {
                if let Some('*') = chars.peek() {
                    chars.next();
                    out.push_str(".*");
                } else if delim_class.is_empty() {
                    out.push_str(".*");
                } else {
                    out.push_str(&format!("[^{}]*", delim_class));
                }
            }
            '?' => {
                if delim_class.is_empty() {
                    out.push('.');
                } else {
                    out.push_str(&format!("[^{}]", delim_class));
                }
            }
            _ => out.push_str(&regex::escape(&ch.to_string())),
        }
    }
    out.push('$');
    out
}

fn glob_quote_meta(text: &str) -> String {
    let mut out = String::new();
    for ch in text.chars() {
        if matches!(ch, '*' | '?' | '[' | ']' | '{' | '}' | '\\') {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

fn value_to_string(value: &Value) -> Result<String, String> {
    match value {
        Value::String(text) => Ok(text.clone()),
        other => serde_json::to_string(other).map_err(|err| format!("stringify error: {err}")),
    }
}

fn sprintf_simple(fmt: &str, args: &[Value]) -> Result<Value, String> {
    let mut out = String::new();
    let mut iter = fmt.chars().peekable();
    let mut index = 0usize;
    while let Some(ch) = iter.next() {
        if ch == '%' {
            if let Some('%') = iter.peek() {
                iter.next();
                out.push('%');
                continue;
            }
            let spec = iter.next().ok_or("sprintf missing format specifier")?;
            let value = args.get(index).ok_or("sprintf missing arg")?;
            index += 1;
            match spec {
                's' => {
                    let text = value.as_str().ok_or("sprintf %s expects string")?;
                    out.push_str(text);
                }
                'v' => {
                    out.push_str(&value_to_string(value)?);
                }
                'd' => {
                    let num = value.as_i64().ok_or("sprintf %d expects integer")?;
                    out.push_str(&num.to_string());
                }
                'f' => {
                    let num = value.as_f64().ok_or("sprintf %f expects number")?;
                    out.push_str(&num.to_string());
                }
                other => return Err(format!("sprintf unsupported format %{other}")),
            }
        } else {
            out.push(ch);
        }
    }
    Ok(Value::String(out))
}

fn sum_numbers(values: &[Value], name: &str) -> Result<Value, String> {
    let list = match values.get(0) {
        Some(Value::Array(items)) => items,
        _ => return Err(format!("{name} expects (array)")),
    };
    let mut sum = 0.0;
    for item in list {
        sum += item
            .as_f64()
            .ok_or_else(|| format!("{name} expects numeric array"))?;
    }
    Ok(Value::Number(
        serde_json::Number::from_f64(sum).ok_or_else(|| format!("{name} invalid number"))?,
    ))
}

fn product_numbers(values: &[Value], name: &str) -> Result<Value, String> {
    let list = match values.get(0) {
        Some(Value::Array(items)) => items,
        _ => return Err(format!("{name} expects (array)")),
    };
    let mut prod = 1.0;
    for item in list {
        prod *= item
            .as_f64()
            .ok_or_else(|| format!("{name} expects numeric array"))?;
    }
    Ok(Value::Number(
        serde_json::Number::from_f64(prod).ok_or_else(|| format!("{name} invalid number"))?,
    ))
}

fn max_numbers(values: &[Value], name: &str) -> Result<Value, String> {
    let list = match values.get(0) {
        Some(Value::Array(items)) => items,
        _ => return Err(format!("{name} expects (array)")),
    };
    let mut max = None::<f64>;
    for item in list {
        let num = item
            .as_f64()
            .ok_or_else(|| format!("{name} expects numeric array"))?;
        max = Some(max.map_or(num, |m| m.max(num)));
    }
    let max = max.ok_or_else(|| format!("{name} expects non-empty array"))?;
    Ok(Value::Number(
        serde_json::Number::from_f64(max).ok_or_else(|| format!("{name} invalid number"))?,
    ))
}

fn min_numbers(values: &[Value], name: &str) -> Result<Value, String> {
    let list = match values.get(0) {
        Some(Value::Array(items)) => items,
        _ => return Err(format!("{name} expects (array)")),
    };
    let mut min = None::<f64>;
    for item in list {
        let num = item
            .as_f64()
            .ok_or_else(|| format!("{name} expects numeric array"))?;
        min = Some(min.map_or(num, |m| m.min(num)));
    }
    let min = min.ok_or_else(|| format!("{name} expects non-empty array"))?;
    Ok(Value::Number(
        serde_json::Number::from_f64(min).ok_or_else(|| format!("{name} invalid number"))?,
    ))
}

fn dump_value(caller: &mut Caller<'_, OpaRuntimeState>, addr: i32) -> Result<Value, OpaError> {
    let memory = caller
        .get_export("memory")
        .and_then(|e| e.into_memory())
        .ok_or(OpaError::MissingExport("memory"))?;
    let dump_func = caller
        .get_export("opa_value_dump")
        .and_then(|e| e.into_func())
        .or_else(|| {
            caller
                .get_export("opa_json_dump")
                .and_then(|e| e.into_func())
        })
        .ok_or(OpaError::MissingExport("opa_value_dump/opa_json_dump"))?;
    let dump = dump_func.typed::<i32, i32>(&mut *caller)?;
    let ptr = dump.call(&mut *caller, addr)?;
    let json = read_cstr(&memory, &mut *caller, ptr);
    Ok(serde_json::from_str(&json)?)
}

fn json_to_opa_value(
    caller: &mut Caller<'_, OpaRuntimeState>,
    value: &Value,
) -> Result<i32, OpaError> {
    let memory = caller
        .get_export("memory")
        .and_then(|e| e.into_memory())
        .ok_or(OpaError::MissingExport("memory"))?;
    let malloc = caller
        .get_export("opa_malloc")
        .and_then(|e| e.into_func())
        .ok_or(OpaError::MissingExport("opa_malloc"))?;
    let parse = caller
        .get_export("opa_json_parse")
        .and_then(|e| e.into_func())
        .ok_or(OpaError::MissingExport("opa_json_parse"))?;
    let malloc = malloc.typed::<i32, i32>(&mut *caller)?;
    let parse = parse.typed::<(i32, i32), i32>(&mut *caller)?;
    let json = serde_json::to_string(value)?;
    let ptr = malloc.call(&mut *caller, json.len() as i32)?;
    if let Err(err) = memory.write(&mut *caller, ptr as usize, json.as_bytes()) {
        return Err(OpaError::Eval(err.to_string()));
    }
    Ok(parse.call(&mut *caller, (ptr, json.len() as i32))?)
}
