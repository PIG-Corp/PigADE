//! Local host lifecycle receipts. These contain no terminal model or input.
use serde::{Deserialize, Serialize};
use std::{ffi::OsStr, path::Path, time::Duration};
use uuid::Uuid;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct ImageIdentity {
    pub dev: u64,
    pub ino: u64,
    pub len: u64,
    pub modified_secs: i64,
    pub modified_nanos: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct SocketIdentity {
    pub dev: u64,
    pub ino: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Status {
    pub schema: u16,
    pub incarnation: Uuid,
    pub version: String,
    pub pid: u32,
    pub active_sessions: usize,
    pub unresolved_launches: usize,
    pub git_busy: bool,
    pub resume_busy: bool,
    pub can_shutdown: bool,
    pub blockers: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Inspection {
    pub schema: u16,
    pub peer: pigade_storage::ProcessIdentity,
    pub socket_identity: SocketIdentity,
    pub installed_image: ImageIdentity,
    pub same_binary: bool,
    pub legacy: bool,
    pub status: Option<Status>,
    pub active_sessions: Option<usize>,
    pub blockers: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
struct ShutdownResult {
    stopped: bool,
    #[serde(default)]
    definitive_refusal: bool,
    message: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Operation {
    Inspect,
    Update,
    Reconcile,
}

#[derive(Default)]
pub(crate) struct Model {
    pub managed: bool,
    pub inspection: Option<Inspection>,
    pub pending: Option<(Uuid, Operation)>,
    /// Retained until a definitive refusal or proof that this exact host exited.
    pub uncertain: Option<Inspection>,
    pub message: Option<String>,
    pub error: Option<String>,
}

impl Model {
    pub fn mutations_frozen(&self) -> bool {
        self.uncertain.is_some()
            || self
                .pending
                .is_some_and(|(_, operation)| operation != Operation::Inspect)
    }

    pub fn outdated(&self) -> bool {
        self.inspection
            .as_ref()
            .is_some_and(|value| !value.same_binary)
    }

    pub fn can_update(&self) -> bool {
        self.managed
            && self.pending.is_none()
            && self.uncertain.is_none()
            && self.inspection.as_ref().is_some_and(|value| {
                value.schema == 1
                    && !value.same_binary
                    && !value.legacy
                    && value.status.as_ref().is_some_and(|status| {
                        status.schema == 1
                            && status.can_shutdown
                            && status.active_sessions == 0
                            && status.unresolved_launches == 0
                            && !status.git_busy
                            && !status.resume_busy
                            && status.blockers.is_empty()
                    })
            })
    }
}

pub(crate) enum Outcome {
    Inspected(Inspection),
    Restarted(Inspection),
    NotStopped { message: String, definitive: bool },
}

/// Runs on the app background executor. The helper authenticates the peer and
/// owns OS identity checks; the app never signals a service or writes its DB.
pub(crate) fn perform(
    socket: &Path,
    operation: Operation,
    expected: Option<&Inspection>,
) -> Result<Outcome, String> {
    let binary = match crate::startup::resolve_host_binary() {
        Ok(binary) => binary,
        Err(message) if operation == Operation::Update => {
            return Ok(Outcome::NotStopped {
                message,
                definitive: true,
            });
        }
        Err(message) => return Err(message),
    };
    if operation == Operation::Inspect {
        let value = crate::provider_discovery::run_json::<Inspection>(
            &binary,
            &[OsStr::new("host-inspect"), socket.as_os_str()],
            Duration::from_secs(12),
        )?;
        return Ok(Outcome::Inspected(value));
    }
    let preflight = (|| -> Result<(&Inspection, String), String> {
        if !crate::startup::is_default_socket(socket)? {
            return Err(
                "Явный или изменившийся адрес host: обновление из этого окна недоступно".into(),
            );
        }
        let expected = expected.ok_or("Нет проверенного снимка службы")?;
        let encoded = serde_json::to_string(expected).map_err(|error| error.to_string())?;
        Ok((expected, encoded))
    })();
    let (expected, encoded) = match preflight {
        Ok(value) => value,
        Err(message) if operation == Operation::Update => {
            return Ok(Outcome::NotStopped {
                message,
                definitive: true,
            });
        }
        Err(message) => return Err(message),
    };
    let command = match operation {
        Operation::Update => "host-shutdown-idle",
        Operation::Reconcile => "host-reconcile",
        Operation::Inspect => unreachable!(),
    };
    let result = crate::provider_discovery::run_json_tracked::<ShutdownResult>(
        &binary,
        &[
            OsStr::new(command),
            socket.as_os_str(),
            OsStr::new(&encoded),
        ],
        Duration::from_secs(12),
    );
    let result = match result {
        Ok(result) => result,
        Err(error) if !error.started && operation == Operation::Update => {
            return Ok(Outcome::NotStopped {
                message: error.message,
                definitive: true,
            });
        }
        Err(error) => return Err(error.message),
    };
    if !result.stopped {
        return Ok(Outcome::NotStopped {
            message: result.message,
            definitive: result.definitive_refusal,
        });
    }
    // Proof of the old process's exit precedes the normal lock-mediated startup.
    // Startup launches only the host, never a CLI or a previous task.
    let started = crate::startup::resolve_socket(None)?;
    if started != socket {
        return Err("Изменился адрес службы. Откройте PigADE заново".into());
    }
    let inspection = crate::provider_discovery::run_json::<Inspection>(
        &binary,
        &[OsStr::new("host-inspect"), socket.as_os_str()],
        Duration::from_secs(12),
    )?;
    if inspection.peer == expected.peer {
        return Err("Замена host не подтверждена; повторная остановка не выполнялась".into());
    }
    Ok(Outcome::Restarted(inspection))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ready() -> Model {
        let inspection = serde_json::from_value(serde_json::json!({
            "schema":1, "peer":{"boot_id":"private-boot", "pid":123, "start_ticks":44},
            "socket_identity":{"dev":1,"ino":2},
            "installed_image":{"dev":1,"ino":3,"len":9,"modified_secs":8,"modified_nanos":0},
            "same_binary":false,"legacy":false,"active_sessions":0,"blockers":[],
            "status":{"schema":1,"incarnation":Uuid::new_v4(),"version":"0.0.1-dev","pid":123,
                "active_sessions":0,"unresolved_launches":0,"git_busy":false,"resume_busy":false,
                "can_shutdown":true,"blockers":[]}
        }))
        .unwrap();
        Model {
            managed: true,
            inspection: Some(inspection),
            ..Default::default()
        }
    }

    #[test]
    fn update_requires_owned_endpoint_new_build_and_confirmed_idle() {
        assert!(ready().can_update());
        let mut model = ready();
        model.managed = false;
        assert!(!model.can_update());
        model = ready();
        model.inspection.as_mut().unwrap().same_binary = true;
        assert!(!model.can_update());
        model = ready();
        model.inspection.as_mut().unwrap().legacy = true;
        assert!(!model.can_update());
        model = ready();
        model.inspection.as_mut().unwrap().status = None;
        assert!(!model.can_update());
    }

    #[test]
    fn active_work_and_contradictory_idle_receipts_never_enable_update() {
        for case in 0..6 {
            let mut model = ready();
            let status = model.inspection.as_mut().unwrap().status.as_mut().unwrap();
            match case {
                0 => status.active_sessions = 1,
                1 => status.unresolved_launches = 1,
                2 => status.git_busy = true,
                3 => status.resume_busy = true,
                4 => status.blockers.push("storage is unavailable".into()),
                _ => status.can_shutdown = false,
            }
            assert!(!model.can_update(), "case {case} enabled update");
        }
    }

    #[test]
    fn pending_or_unknown_shutdown_cannot_be_repeated() {
        let mut model = ready();
        model.pending = Some((Uuid::new_v4(), Operation::Update));
        assert!(!model.can_update());
        assert!(model.mutations_frozen());
        model.pending = None;
        model.uncertain = model.inspection.clone();
        assert!(!model.can_update());
        assert!(model.mutations_frozen());
        // A read-only refresh does not erase the earlier operation's frozen peer.
        model.inspection = ready().inspection;
        assert!(!model.can_update());
    }

    #[test]
    fn frozen_inspection_round_trip_preserves_shutdown_identity() {
        let receipt = ready().inspection.unwrap();
        let value = serde_json::to_value(&receipt).unwrap();
        let decoded: Inspection = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(decoded).unwrap(), value);
        assert_eq!(receipt.peer.pid, 123);
        assert_eq!(receipt.peer.start_ticks, 44);
    }
}
