use std::path::PathBuf;

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
    policy_path: PathBuf,
    policy_loaded: bool,
    policy_version: u64,
    opa_load_status: u8,
    logged_missing: bool,
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
    pub fn new(policy_path: PathBuf) -> Self {
        Self {
            policy_path,
            policy_loaded: false,
            policy_version: 0,
            opa_load_status: OPA_STATUS_ERROR,
            logged_missing: false,
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
            tracing::warn!(
                path = %self.policy_path.display(),
                "opa policy not loaded; resolver disabled"
            );
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

    pub fn reload(&mut self, version: u64) -> Result<(), OpaError> {
        let prev_module = self.module.take();
        let prev_wasm = self.wasm.take();
        let prev_version = self.policy_version;
        let prev_loaded = self.policy_loaded;

        self.opa_load_status = OPA_STATUS_LOADING;
        let result = (|| {
            let bytes = std::fs::read(&self.policy_path)?;
            let module = Module::from_binary(&self.engine, &bytes)?;
            let wasm = OpaWasm::instantiate(&self.engine, &module)?;
            self.module = Some(module);
            self.wasm = Some(wasm);
            self.policy_loaded = true;
            self.policy_version = version;
            self.opa_load_status = OPA_STATUS_OK;
            self.logged_missing = false;
            tracing::info!(
                path = %self.policy_path.display(),
                version = version,
                "opa policy loaded"
            );
            Ok(())
        })();
        if let Err(err) = result {
            self.module = prev_module;
            self.wasm = prev_wasm;
            self.policy_loaded = prev_loaded;
            self.policy_version = prev_version;
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
            set_entry.call(&mut wasm.store, (ctx, 0))?;
        }
        wasm.opa_eval_ctx_set_input.call(&mut wasm.store, (ctx, input_val))?;

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

        linker.func_wrap("env", "opa_abort", move |mut caller: Caller<'_, OpaRuntimeState>, ptr: i32| {
            if let Some(memory) = caller.get_export("memory").and_then(|e| e.into_memory()) {
                let msg = read_cstr(&memory, &mut caller, ptr);
                caller.data_mut().last_error = Some(msg);
            } else {
                caller.data_mut().last_error = Some("opa_abort".to_string());
            }
        })?;
        linker.func_wrap("env", "opa_println", move |mut caller: Caller<'_, OpaRuntimeState>, ptr: i32| {
            if let Some(memory) = caller.get_export("memory").and_then(|e| e.into_memory()) {
                let msg = read_cstr(&memory, &mut caller, ptr);
                tracing::debug!("opa println: {msg}");
            }
        })?;

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
                        |mut caller: Caller<'_, OpaRuntimeState>, _ctx: i32, builtin: i32, a0: i32| -> i32 {
                            eval_builtin(&mut caller, builtin, &[a0])
                        },
                    )?;
                }
                2 => {
                    linker.func_wrap(
                        "env",
                        name,
                        |mut caller: Caller<'_, OpaRuntimeState>, _ctx: i32, builtin: i32, a0: i32, a1: i32| -> i32 {
                            eval_builtin(&mut caller, builtin, &[a0, a1])
                        },
                    )?;
                }
                3 => {
                    linker.func_wrap(
                        "env",
                        name,
                        |mut caller: Caller<'_, OpaRuntimeState>, _ctx: i32, builtin: i32, a0: i32, a1: i32, a2: i32| -> i32 {
                            eval_builtin(&mut caller, builtin, &[a0, a1, a2])
                        },
                    )?;
                }
                4 => {
                    linker.func_wrap(
                        "env",
                        name,
                        |mut caller: Caller<'_, OpaRuntimeState>, _ctx: i32, builtin: i32, a0: i32, a1: i32, a2: i32, a3: i32| -> i32 {
                            eval_builtin(&mut caller, builtin, &[a0, a1, a2, a3])
                        },
                    )?;
                }
                5 => {
                    linker.func_wrap(
                        "env",
                        name,
                        |mut caller: Caller<'_, OpaRuntimeState>, _ctx: i32, builtin: i32, a0: i32, a1: i32, a2: i32, a3: i32, a4: i32| -> i32 {
                            eval_builtin(&mut caller, builtin, &[a0, a1, a2, a3, a4])
                        },
                    )?;
                }
                _ => {
                    linker.func_wrap(
                        "env",
                        name,
                        |mut caller: Caller<'_, OpaRuntimeState>, _ctx: i32, builtin: i32, a0: i32, a1: i32, a2: i32, a3: i32, a4: i32, a5: i32| -> i32 {
                            eval_builtin(&mut caller, builtin, &[a0, a1, a2, a3, a4, a5])
                        },
                    )?;
                }
            }
        }

        let instance = linker.instantiate(&mut store, module)?;
        let memory = instance
            .get_memory(&mut store, "memory")
            .ok_or(OpaError::MissingExport("memory"))?;
        if let Some(builtins) = instance.get_typed_func::<(), i32>(&mut store, "builtins").ok() {
            if let Ok(ptr) = builtins.call(&mut store, ()) {
                let json = read_cstr_store(&memory, &mut store, ptr);
                store.data_mut().builtin_names = parse_builtins(&json);
            }
        } else {
            tracing::warn!("opa wasm missing builtins export; builtin map unavailable");
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
        let opa_eval = instance
            .get_typed_func::<i32, i32>(&mut store, "opa_eval")
            .map_err(|_| OpaError::MissingExport("opa_eval"))?;

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

fn eval_builtin(
    caller: &mut Caller<'_, OpaRuntimeState>,
    builtin_id: i32,
    args: &[i32],
) -> i32 {
    let name = caller
        .data()
        .builtin_names
        .get(builtin_id as usize)
        .cloned()
        .unwrap_or_else(|| format!("builtin#{builtin_id}"));
    let values: Result<Vec<Value>, _> =
        args.iter().map(|addr| dump_value(caller, *addr)).collect();
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
        "contains" => match (values.get(0), values.get(1)) {
            (Some(Value::String(hay)), Some(Value::String(needle))) => {
                Ok(Value::Bool(hay.contains(needle)))
            }
            (Some(Value::Array(list)), Some(value)) => {
                Ok(Value::Bool(list.iter().any(|item| item == value)))
            }
            _ => Err("contains expects (string,string) or (array,any)".to_string()),
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

fn dump_value(caller: &mut Caller<'_, OpaRuntimeState>, addr: i32) -> Result<Value, OpaError> {
    let memory = caller
        .get_export("memory")
        .and_then(|e| e.into_memory())
        .ok_or(OpaError::MissingExport("memory"))?;
    let dump_func = caller
        .get_export("opa_value_dump")
        .and_then(|e| e.into_func())
        .or_else(|| caller.get_export("opa_json_dump").and_then(|e| e.into_func()))
        .ok_or(OpaError::MissingExport("opa_value_dump/opa_json_dump"))?;
    let dump = dump_func.typed::<i32, i32>(caller)?;
    let ptr = dump.call(caller, addr)?;
    let json = read_cstr(&memory, caller, ptr);
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
    let malloc = malloc.typed::<i32, i32>(caller)?;
    let parse = parse.typed::<(i32, i32), i32>(caller)?;
    let json = serde_json::to_string(value)?;
    let ptr = malloc.call(caller, json.len() as i32)?;
    if let Err(err) = memory.write(caller, ptr as usize, json.as_bytes()) {
        return Err(OpaError::Eval(err.to_string()));
    }
    Ok(parse.call(caller, (ptr, json.len() as i32))?)
}
