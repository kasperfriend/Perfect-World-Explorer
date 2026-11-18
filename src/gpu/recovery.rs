//! Recover by opening a fresh device/world in the existing handoff process.
//! Do not attempt to reuse a lost device, its SDK contexts or stale ray AS.
use super::LighterSettings;
use std::{
    process::Command,
    sync::{Arc, Mutex},
};

const VIEW_ENV: &str = "PW_GPU_RECOVERY_VIEW";
const LIGHTER_ENV: &str = "PW_GPU_RECOVERY_LIGHTER";

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

pub(super) fn lost_error(message: &str) -> bool {
    message.contains("ERROR_DEVICE_LOST") || message.contains("Parent device is lost")
}

pub(super) struct Snapshot {
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
    fn decode(map: &str, view: &str, lighter: &str) -> Result<Self, String> {
        let mut parts = view.split_ascii_whitespace();
        if !parts.next().is_some_and(|m| m.eq_ignore_ascii_case(map)) {
            return Err("Device recovery refused a fallback or different map".into());
        }
        let values: Vec<f32> = parts
            .map(str::parse::<f32>)
            .collect::<Result<_, _>>()
            .map_err(|_| "Invalid camera for device recovery")?;
        let view: [f32; 6] = values
            .try_into()
            .map_err(|_| "Invalid camera extent for device recovery")?;
        if !view.iter().all(|v| v.is_finite())
            || view[..3].iter().any(|v| v.abs() > 1.0e7)
            || view[4].abs() > 1.57
            || !(0. ..=24.).contains(&view[5])
        {
            return Err("Invalid camera range for device recovery".into());
        }
        Ok(Self {
            map: map.to_string(),
            view,
            lighter: LighterSettings::decode(lighter),
        })
    }
    pub(super) fn from_child(map: &str) -> Result<Option<Self>, String> {
        if !crate::world_handoff::is_handoff_child() {
            return Ok(None);
        }
        match std::env::var(VIEW_ENV) {
            Ok(view) => Self::decode(
                map,
                &view,
                &std::env::var(LIGHTER_ENV)
                    .map_err(|_| "Missing renderer settings for device recovery")?,
            )
            .map(Some),
            Err(std::env::VarError::NotPresent) => Ok(None),
            Err(_) => Err("Invalid device recovery metadata".into()),
        }
    }
    pub(super) fn configure(command: &mut Command, snapshot: Option<&Self>) {
        command.env_remove(VIEW_ENV).env_remove(LIGHTER_ENV);
        if let Some(s) = snapshot {
            command
                .env(VIEW_ENV, s.encode_view())
                .env(LIGHTER_ENV, s.lighter.encode());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn restart_preserves_world_view_and_renderer_and_rejects_wrong_map() {
        let s = Snapshot {
            map: "a07".into(),
            view: [16., 332., 84., 0., -0.6, 1.5],
            lighter: LighterSettings::decode("mode=hardware\nreference=true"),
        };
        let restored = Snapshot::decode("A07", &s.encode_view(), &s.lighter.encode()).unwrap();
        assert_eq!(restored.view, s.view);
        assert_eq!(restored.lighter, s.lighter);
        assert!(Snapshot::decode("world", &s.encode_view(), "").is_err());
        assert!(Snapshot::decode("a07", "a07 0 NaN 0 0 0 0", "").is_err());
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
