//! Group `endpoints` in the agent: the endpoint watch (stopped with the
//! connection) and the app list, with the direct Windows calls

use serde_json::Value;

use crate::host::endpoints::{App, EndpointsHost, EndpointsWatch, Event, GROUP, Request};
use crate::wire::{WireError, decode, reply, to_body};
use crate::{AgentContext, Handler, LocalHost};

pub(crate) fn new(ctx: AgentContext) -> Box<dyn Handler> {
    // A core that watches does so through this agent from now on
    ctx.event(GROUP, to_body(&Event::Ready));
    Box::new(Endpoints { ctx, watch: None })
}

struct Endpoints {
    ctx: AgentContext,
    watch: Option<EndpointsWatch>,
}

impl Handler for Endpoints {
    fn request(&mut self, body: Value) -> Result<Value, WireError> {
        match decode(body)? {
            Request::Watch => {
                if self.watch.is_none() {
                    let ctx = self.ctx.clone();
                    let changed = Box::new(move || {
                        ctx.event(GROUP, to_body(&Event::Changed));
                    });
                    match LocalHost.watch_endpoints(changed) {
                        Ok(w) => {
                            tracing::info!("audio endpoints: watching");
                            self.watch = Some(w);
                        }
                        Err(e) => {
                            tracing::warn!("audio endpoints: not watched: {e}");
                            return Err(WireError::new(crate::ErrorCode::Failed, "watch"));
                        }
                    }
                }
                reply(())
            }
            Request::Unwatch => {
                if self.watch.take().is_some() {
                    tracing::info!("audio endpoints: no longer watched");
                }
                reply(())
            }
            Request::AudioApps => {
                let list: Vec<App> = LocalHost.audio_apps().into_iter().map(App::from).collect();
                reply(list)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ErrorCode;
    use serde_json::json;

    /// Read-only: the watch registers and stops, the list is read
    #[test]
    fn watches_and_lists() {
        let mut h = new(AgentContext::detached());
        assert_eq!(h.request(json!({"op": "watch"})).unwrap(), Value::Null);
        assert_eq!(h.request(json!({"op": "watch"})).unwrap(), Value::Null);
        assert_eq!(h.request(json!({"op": "unwatch"})).unwrap(), Value::Null);
        h.request(json!({"op": "watch"})).unwrap();
        let list: Vec<App> = serde_json::from_value(h.request(json!({"op": "audio_apps"})).unwrap()).unwrap();
        assert!(list.iter().all(|a| a.process_id != std::process::id()));
        assert_eq!(h.request(json!({"op": "nonsense"})).unwrap_err().code, ErrorCode::BadRequest);
        // Dropped with the watch running: it stops
        drop(h);
    }
}
