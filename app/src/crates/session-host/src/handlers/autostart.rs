//! Group `autostart` in the agent: the user's Run values, through a
//! [`RunStore`]: Windows' registry ([`crate::LocalHost`]), or for tests a
//! store in memory ([`use_memory_store`]) so that no test writes the real
//! entry.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use serde_json::Value;

use crate::host::autostart::{AutostartHost, Request, core_command, known_name};
use crate::wire::{ErrorCode, WireError, decode, reply};
use crate::{AgentContext, Handler, LocalHost};

/// Where the agent keeps the Run values
pub trait RunStore: Send + Sync {
    fn get(&self, name: &str) -> Result<Option<String>, String>;
    fn set(&self, name: &str, command: &str) -> Result<(), String>;
    fn remove(&self, name: &str) -> Result<bool, String>;
    fn carry_startup_approved(&self, from: &str, to: &str) -> Result<bool, String>;
}

impl RunStore for LocalHost {
    fn get(&self, name: &str) -> Result<Option<String>, String> {
        self.autostart_get(name)
    }
    fn set(&self, name: &str, command: &str) -> Result<(), String> {
        self.autostart_set(name, command)
    }
    fn remove(&self, name: &str) -> Result<bool, String> {
        self.autostart_remove(name)
    }
    fn carry_startup_approved(&self, from: &str, to: &str) -> Result<bool, String> {
        self.autostart_carry_startup_approved(from, to)
    }
}

/// Run values in memory, empty at start
#[derive(Debug, Default)]
pub struct MemoryRunStore(Mutex<HashMap<String, String>>);

impl MemoryRunStore {
    fn values(&self) -> std::sync::MutexGuard<'_, HashMap<String, String>> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl RunStore for MemoryRunStore {
    fn get(&self, name: &str) -> Result<Option<String>, String> {
        Ok(self.values().get(name).cloned())
    }
    fn set(&self, name: &str, command: &str) -> Result<(), String> {
        self.values().insert(name.into(), command.into());
        Ok(())
    }
    fn remove(&self, name: &str) -> Result<bool, String> {
        Ok(self.values().remove(name).is_some())
    }
    fn carry_startup_approved(&self, _from: &str, _to: &str) -> Result<bool, String> {
        Ok(false)
    }
}

static STORE: OnceLock<Box<dyn RunStore>> = OnceLock::new();

/// This agent keeps the Run values in memory (tests); once, before
/// [`crate::run_agent`]
pub fn use_memory_store() -> bool {
    STORE.set(Box::new(MemoryRunStore::default())).is_ok()
}

fn store() -> &'static dyn RunStore {
    STORE.get_or_init(|| Box::new(LocalHost)).as_ref()
}

pub(crate) fn new(_ctx: AgentContext) -> Box<dyn Handler> {
    Box::new(Autostart { store: store() })
}

struct Autostart {
    store: &'static dyn RunStore,
}

fn failed(what: &str, e: String) -> WireError {
    tracing::warn!("autostart: {what} failed");
    WireError::failed(what, &e)
}

fn refused(why: &str) -> WireError {
    tracing::warn!("autostart: a request refused ({why})");
    WireError::new(ErrorCode::BadRequest, why)
}

impl Handler for Autostart {
    fn request(&mut self, body: Value) -> Result<Value, WireError> {
        let request: Request = decode(body)?;
        let names_ok = match &request {
            Request::Get { name } | Request::Set { name, .. } | Request::Remove { name } => known_name(name),
            Request::CarryStartupApproved { from, to } => known_name(from) && known_name(to),
        };
        if !names_ok {
            return Err(refused("not a value of BlueCaster"));
        }
        match request {
            Request::Get { name } => reply(self.store.get(&name).map_err(|e| failed("read", e))?),
            Request::Set { name, command } => {
                if !core_command(&command) {
                    return Err(refused("not a command line of the core"));
                }
                self.store.set(&name, &command).map_err(|e| failed("write", e))?;
                tracing::info!("autostart: entry written");
                reply(())
            }
            Request::Remove { name } => {
                let was = self.store.remove(&name).map_err(|e| failed("remove", e))?;
                tracing::info!("autostart: entry removed (it was there: {was})");
                reply(was)
            }
            Request::CarryStartupApproved { from, to } => {
                reply(self.store.carry_startup_approved(&from, &to).map_err(|e| failed("startup state", e))?)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn handler() -> Autostart {
        // A store of its own: never the user's registry
        Autostart { store: Box::leak(Box::new(MemoryRunStore::default())) }
    }

    #[test]
    fn reads_and_writes_the_store() {
        let mut h = handler();
        let core = r#""C:\BlueCaster\bluecaster-core.exe" --background"#;
        let get = serde_json::json!({"op": "get", "name": "BlueCaster"});
        assert_eq!(h.request(get.clone()).unwrap(), Value::Null);
        h.request(serde_json::json!({"op": "set", "name": "BlueCaster", "command": core})).unwrap();
        assert_eq!(h.request(get.clone()).unwrap(), Value::from(core));
        assert_eq!(h.request(serde_json::json!({"op": "remove", "name": "BlueCaster"})).unwrap(), Value::Bool(true));
        assert_eq!(h.request(get).unwrap(), Value::Null);
        let carry = serde_json::json!({"op": "carry_startup_approved", "from": "A2DPWB", "to": "BlueCaster"});
        assert_eq!(h.request(carry).unwrap(), Value::Bool(false));
    }

    #[test]
    fn other_values_and_programs_are_refused() {
        let mut h = handler();
        let other = serde_json::json!({"op": "get", "name": "OneDrive"});
        assert_eq!(h.request(other).unwrap_err().code, ErrorCode::BadRequest);
        let cmd = serde_json::json!({"op": "set", "name": "BlueCaster", "command": r#""C:\x\evil.exe""#});
        assert_eq!(h.request(cmd).unwrap_err().code, ErrorCode::BadRequest);
        let carry = serde_json::json!({"op": "carry_startup_approved", "from": "X", "to": "BlueCaster"});
        assert_eq!(h.request(carry).unwrap_err().code, ErrorCode::BadRequest);
        assert_eq!(h.request(serde_json::json!({"op": "nonsense"})).unwrap_err().code, ErrorCode::BadRequest);
    }
}
