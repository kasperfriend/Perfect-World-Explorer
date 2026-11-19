//! Restore the current map and view in a fresh process (reload or device loss).
//! Do not attempt to reuse a lost device, its SDK contexts or stale ray AS.
use super::LighterSettings;
use std::{
    process::Command,
    sync::{Arc, Mutex},
};

const VIEW_ENV: &str = "PW_GPU_RECOVERY_VIEW";
const LIGHTER_ENV: &str = "PW_GPU_RECOVERY_LIGHTER";
const RELOAD_ENV: &str = "PWE_WORLD_RELOAD";

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum RestoreKind {
    Reload,
    DeviceRecovery,
}

impl RestoreKind {
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Reload => "World reload",
            Self::DeviceRecovery => "Graphics recovery",
        }
    }
}

pub(super) fn kind() -> RestoreKind {
    if std::env::var_os(RELOAD_ENV).is_some() {
        RestoreKind::Reload
    } else {
        RestoreKind::DeviceRecovery
    }
}

pub(super) fn is_child() -> bool {
    crate::world_handoff::is_handoff_child() && std::env::var_os(VIEW_ENV).is_some()
}

#[derive(Clone, Default)]
pub(super) struct Loss(Arc<Mutex<Option<String>>>);
impl Loss {
    pub(super) fn register(&self, device: &wgpu::Device, notify: impl Fn() + Send + 'static) {
        let loss = self.clone();
        device.set_device_lost_callback(move |reason, message| {
            // The viewer never destroys a live device on a resize or mode
            // change. Dropping it, or replacing the callback, isn't a loss.
            if matches!(
                reason,
                wgpu::DeviceLostReason::Unknown | wgpu::DeviceLostReason::Destroyed
            ) {
                loss.report(message);
                notify();
            }
        });
    }
    /// wgpu 0.19 sometimes panics instead of delivering DeviceLost from
    /// poll/submit. Only take over recognized loss reports; other panics
    /// still follow the normal error path.
    pub(super) fn guard_frame(&self, frame: impl FnOnce()) {
        if let Err(payload) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(frame)) {
            let message = payload
                .downcast_ref::<String>()
                .map(String::as_str)
                .or_else(|| payload.downcast_ref::<&str>().copied());
            if let Some(message) = message.filter(|m| lost_error(m)) {
                self.report(message.to_string());
            } else {
                std::panic::resume_unwind(payload);
            }
        }
    }

    pub(super) fn report(&self, message: String) {
        self.0.lock().unwrap().get_or_insert(message);
    }
    pub(super) fn pending(&self) -> bool {
        self.0.lock().unwrap().is_some()
    }
    pub(super) fn take(&self) -> Option<String> {
        self.0.lock().unwrap().take()
    }
}

pub(crate) fn lost_error(message: &str) -> bool {
    message.contains("ERROR_DEVICE_LOST") || message.contains("Parent device is lost")
}

pub(super) struct Snapshot {
    pub(super) kind: RestoreKind,
    pub(super) map: String,
    pub(super) view: [f32; 6], // x y z yaw pitch hour
    pub(super) lighter: LighterSettings,
}
impl Snapshot {
    fn encode_view(&self) -> String {
        std::iter::once(self.map.clone())
            .chain(self.view.iter().map(ToString::to_string))
            .collect::<Vec<_>>()
            .join(" ")
    }
    fn decode(kind: RestoreKind, map: &str, view: &str, lighter: &str) -> Result<Self, String> {
        let operation = kind.label();
        let mut parts = view.split_ascii_whitespace();
        if !parts.next().is_some_and(|m| m.eq_ignore_ascii_case(map)) {
            return Err(format!("{operation} refused a fallback or different map"));
        }
        let values: Vec<f32> = parts
            .map(str::parse::<f32>)
            .collect::<Result<_, _>>()
            .map_err(|_| format!("Invalid camera for {operation}"))?;
        let view: [f32; 6] = values
            .try_into()
            .map_err(|_| format!("Invalid camera extent for {operation}"))?;
        if !view.iter().all(|v| v.is_finite())
            || view[..3].iter().any(|v| v.abs() > 1.0e7)
            || view[4].abs() > 1.57
            || !(0. ..=24.).contains(&view[5])
        {
            return Err(format!("Invalid camera range for {operation}"));
        }
        Ok(Self {
            kind,
            map: map.to_string(),
            view,
            lighter: LighterSettings::decode(lighter),
        })
    }
    pub(super) fn from_child(map: &str) -> Result<Option<Self>, String> {
        if !crate::world_handoff::is_handoff_child() {
            return Ok(None);
        }
        let kind = kind();
        match std::env::var(VIEW_ENV) {
            Ok(view) => Self::decode(
                kind,
                map,
                &view,
                &std::env::var(LIGHTER_ENV)
                    .map_err(|_| format!("Missing renderer settings for {}", kind.label()))?,
            )
            .map(Some),
            Err(std::env::VarError::NotPresent) => Ok(None),
            Err(_) => Err(format!("Invalid {} metadata", kind.label())),
        }
    }
    pub(super) fn configure(command: &mut Command, snapshot: Option<&Self>) {
        command
            .env_remove(VIEW_ENV)
            .env_remove(LIGHTER_ENV)
            .env_remove(RELOAD_ENV);
        if let Some(s) = snapshot {
            command
                .env(VIEW_ENV, s.encode_view())
                .env(LIGHTER_ENV, s.lighter.encode());
            if s.kind == RestoreKind::Reload {
                command.env(RELOAD_ENV, "1");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reload_metadata_is_explicit_and_not_inherited_by_later_handoffs() {
        let snapshot = Snapshot {
            kind: RestoreKind::Reload,
            map: "a07".into(),
            view: [16., 332., 84., 0., -0.6, 1.5],
            lighter: LighterSettings::default(),
        };
        let mut command = Command::new("viewer");
        Snapshot::configure(&mut command, Some(&snapshot));
        assert!(command
            .get_envs()
            .any(|(key, value)| key == RELOAD_ENV && value.is_some()));
        let wrong_map = Snapshot::decode(snapshot.kind, "world", &snapshot.encode_view(), "").err();
        assert!(wrong_map.unwrap().starts_with("World reload"));
        Snapshot::configure(&mut command, None);
        assert!(command.get_envs().all(|(_, value)| value.is_none()));
    }

    #[test]
    fn restart_preserves_world_view_and_renderer_and_rejects_wrong_map() {
        let s = Snapshot {
            kind: RestoreKind::DeviceRecovery,
            map: "a07".into(),
            view: [16., 332., 84., 0., -0.6, 1.5],
            lighter: LighterSettings::decode("mode=hardware\nreference=true"),
        };
        let restored =
            Snapshot::decode(s.kind, "A07", &s.encode_view(), &s.lighter.encode()).unwrap();
        assert_eq!(restored.view, s.view);
        assert_eq!(restored.lighter, s.lighter);
        assert!(Snapshot::decode(s.kind, "world", &s.encode_view(), "").is_err());
        assert!(Snapshot::decode(s.kind, "a07", "a07 0 NaN 0 0 0 0", "").is_err());
        assert!(lost_error("Error in Device::poll: Parent device is lost"));
        assert!(lost_error(
            "Lighter Vulkan submit ray commands: ERROR_DEVICE_LOST"
        ));
        assert!(!lost_error("Surface is outdated"));
        let loss = Loss::default();
        loss.report("first".into());
        loss.clone().report("second".into());
        assert_eq!(loss.take().as_deref(), Some("first"));
        assert!(!loss.pending());
    }
}
