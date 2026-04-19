//! Request handling for the IPC server.

use serde_json::{Value, json};
use tracing::{debug, info};

use undertone_core::command::Command;
use undertone_core::state::StateSnapshot;
use undertone_ipc::messages::{ErrorInfo, Method};

/// Result of handling a request: response value and optional command.
pub struct HandleResult {
    pub response: Result<Value, ErrorInfo>,
    pub command: Option<Command>,
}

impl HandleResult {
    fn ok(value: Value) -> Self {
        Self { response: Ok(value), command: None }
    }

    fn ok_with_command(value: Value, command: Command) -> Self {
        Self { response: Ok(value), command: Some(command) }
    }

    fn err(error: ErrorInfo) -> Self {
        Self { response: Err(error), command: None }
    }

    fn channel_not_found(channel: &str) -> Self {
        Self::err(ErrorInfo::new(404, format!("Channel not found: {channel}")))
    }
}

/// Check if a channel exists in the state.
fn channel_exists(state: &StateSnapshot, channel: &str) -> bool {
    state.channels.iter().any(|c| c.config.name == channel)
}

/// Handle an IPC request and return a response value with optional command.
pub fn handle_request(method: &Method, state: &StateSnapshot) -> HandleResult {
    match method {
        Method::GetState => HandleResult::ok(serde_json::to_value(state).unwrap_or(json!({}))),

        Method::GetChannels => {
            HandleResult::ok(serde_json::to_value(&state.channels).unwrap_or(json!([])))
        }

        Method::GetChannel { name } => {
            if let Some(ch) = state.channels.iter().find(|c| &c.config.name == name) {
                HandleResult::ok(serde_json::to_value(ch).unwrap_or(json!({})))
            } else {
                HandleResult::err(ErrorInfo::new(404, format!("Channel not found: {name}")))
            }
        }

        Method::GetApps => {
            HandleResult::ok(serde_json::to_value(&state.app_routes).unwrap_or(json!([])))
        }

        Method::GetProfiles => {
            HandleResult::ok(serde_json::to_value(&state.profiles).unwrap_or(json!([])))
        }

        Method::GetProfile { name } => {
            if let Some(profile) = state.profiles.iter().find(|p| &p.name == name) {
                HandleResult::ok(serde_json::to_value(profile).unwrap_or(json!({})))
            } else {
                HandleResult::err(ErrorInfo::new(404, format!("Profile not found: {name}")))
            }
        }

        Method::GetDeviceStatus => HandleResult::ok(json!({
            "connected": state.device_connected,
            "serial": state.device_serial,
        })),

        Method::GetDiagnostics => HandleResult::ok(json!({
            "state": format!("{:?}", state.state),
            "created_nodes": state.created_nodes.len(),
            "created_links": state.created_links.len(),
        })),

        Method::SetChannelVolume { channel, mix, volume } => {
            if !channel_exists(state, channel) {
                return HandleResult::channel_not_found(channel);
            }
            let volume = volume.clamp(0.0, 1.0);
            debug!(?channel, ?mix, volume, "Setting channel volume");
            HandleResult::ok_with_command(
                json!({"success": true, "volume": volume}),
                Command::SetChannelVolume { channel: channel.clone(), mix: *mix, volume },
            )
        }

        Method::SetChannelMute { channel, mix, muted } => {
            if !channel_exists(state, channel) {
                return HandleResult::channel_not_found(channel);
            }
            debug!(?channel, ?mix, muted, "Setting channel mute");
            HandleResult::ok_with_command(
                json!({"success": true, "muted": muted}),
                Command::SetChannelMute { channel: channel.clone(), mix: *mix, muted: *muted },
            )
        }

        Method::SetMasterVolume { mix, volume } => {
            let volume = volume.clamp(0.0, 1.0);
            debug!(?mix, volume, "Setting master volume");
            HandleResult::ok_with_command(
                json!({"success": true, "volume": volume}),
                Command::SetMasterVolume { mix: *mix, volume },
            )
        }

        Method::SetMasterMute { mix, muted } => {
            debug!(?mix, muted, "Setting master mute");
            HandleResult::ok_with_command(
                json!({"success": true, "muted": muted}),
                Command::SetMasterMute { mix: *mix, muted: *muted },
            )
        }

        Method::SetAppRoute { app_pattern, channel } => {
            if !channel_exists(state, channel) {
                return HandleResult::channel_not_found(channel);
            }
            info!(?app_pattern, ?channel, "Setting app route");
            HandleResult::ok_with_command(
                json!({"success": true}),
                Command::SetAppRoute { app_pattern: app_pattern.clone(), channel: channel.clone() },
            )
        }

        Method::RemoveAppRoute { app_pattern } => {
            info!(?app_pattern, "Removing app route");
            HandleResult::ok_with_command(
                json!({"success": true}),
                Command::RemoveAppRoute { app_pattern: app_pattern.clone() },
            )
        }

        Method::SaveProfile { name } => {
            info!(?name, "Saving profile");
            HandleResult::ok_with_command(
                json!({"success": true}),
                Command::SaveProfile { name: name.clone() },
            )
        }

        Method::LoadProfile { name } => {
            info!(?name, "Loading profile");
            HandleResult::ok_with_command(
                json!({"success": true}),
                Command::LoadProfile { name: name.clone() },
            )
        }

        Method::DeleteProfile { name } => {
            info!(?name, "Deleting profile");
            HandleResult::ok_with_command(
                json!({"success": true}),
                Command::DeleteProfile { name: name.clone() },
            )
        }

        Method::SetMicGain { gain } => {
            let gain = gain.clamp(0.0, 1.0);
            debug!(gain, "Setting mic gain");
            HandleResult::ok_with_command(
                json!({"success": true, "gain": gain}),
                Command::SetMicGain { gain },
            )
        }

        Method::SetMicMute { muted } => {
            debug!(muted, "Setting mic mute");
            HandleResult::ok_with_command(
                json!({"success": true, "muted": muted}),
                Command::SetMicMute { muted: *muted },
            )
        }

        Method::SetHeadphoneVolume { volume } => {
            let volume = volume.clamp(0.0, 1.0);
            debug!(volume, "Setting headphone volume");
            HandleResult::ok_with_command(
                json!({"success": true, "volume": volume}),
                Command::SetHeadphoneVolume { volume },
            )
        }

        Method::GetMicChain => HandleResult::ok(
            serde_json::to_value(&state.mic_chain).unwrap_or(json!(null)),
        ),

        Method::SetEffectBypass { effect, bypassed } => {
            debug!(effect, bypassed, "Toggling effect bypass");
            HandleResult::ok_with_command(
                json!({"success": true, "effect": effect, "bypassed": bypassed}),
                Command::SetEffectBypass { effect: effect.clone(), bypassed: *bypassed },
            )
        }

        Method::SetEffectParam { effect, param, value } => {
            debug!(effect, param, value, "Setting effect param");
            HandleResult::ok_with_command(
                json!({"success": true, "effect": effect, "param": param, "value": value}),
                Command::SetEffectParam {
                    effect: effect.clone(),
                    param: param.clone(),
                    value: *value,
                },
            )
        }

        Method::LoadEffectPreset { name } => {
            info!(?name, "Loading effect preset");
            HandleResult::ok_with_command(
                json!({"success": true, "preset": name}),
                Command::LoadEffectPreset { name: name.clone() },
            )
        }

        Method::ResetEffectChain => {
            info!("Resetting effect chain to defaults");
            HandleResult::ok_with_command(json!({"success": true}), Command::ResetEffectChain)
        }

        Method::GetOutputDevices => {
            debug!("Getting output devices");
            HandleResult::ok(json!({
                "devices": state.output_devices,
                "current": state.monitor_output,
            }))
        }

        Method::SetMonitorOutput { device_name } => {
            // Validate device exists
            if !state.output_devices.iter().any(|d| d.name == *device_name) {
                return HandleResult::err(ErrorInfo::new(
                    404,
                    format!("Output device not found: {device_name}"),
                ));
            }
            info!(?device_name, "Setting monitor output device");
            HandleResult::ok_with_command(
                json!({"success": true, "device": device_name}),
                Command::SetMonitorOutput { device_name: device_name.clone() },
            )
        }

        Method::Subscribe { events } => {
            debug!(?events, "Client subscribing to events");
            // Subscription handling is done in the IPC server
            HandleResult::ok(json!({"success": true}))
        }

        Method::Unsubscribe { events } => {
            debug!(?events, "Client unsubscribing from events");
            // Unsubscription handling is done in the IPC server
            HandleResult::ok(json!({"success": true}))
        }

        Method::Shutdown => {
            info!("Shutdown requested via IPC");
            HandleResult::ok_with_command(json!({"success": true}), Command::Shutdown)
        }

        Method::Reconcile => {
            info!("Reconciliation requested via IPC");
            HandleResult::ok_with_command(json!({"success": true}), Command::Reconcile)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use undertone_effects::MicChain;

    fn empty_state() -> StateSnapshot {
        StateSnapshot::default()
    }

    fn state_with_chain() -> StateSnapshot {
        let mut s = StateSnapshot::default();
        s.mic_chain = Some(MicChain::default().snapshot());
        s
    }

    #[test]
    fn get_mic_chain_returns_snapshot_field_verbatim() {
        let result = handle_request(&Method::GetMicChain, &state_with_chain());
        let value = result.response.expect("GetMicChain should not error");
        // Snapshot has 4 effects.
        let effects = value.get("effects").and_then(|v| v.as_array()).expect("array");
        assert_eq!(effects.len(), 4);
        assert!(value.get("preset").is_some());
        assert!(result.command.is_none(), "GetMicChain is read-only");
    }

    #[test]
    fn get_mic_chain_returns_null_when_chain_absent() {
        let result = handle_request(&Method::GetMicChain, &empty_state());
        let value = result.response.expect("GetMicChain should not error");
        assert!(value.is_null(), "expected null, got {value:?}");
    }

    #[test]
    fn set_effect_bypass_emits_command_and_echoes_params() {
        let result = handle_request(
            &Method::SetEffectBypass {
                effect: "compressor".into(),
                bypassed: true,
            },
            &empty_state(),
        );
        let value = result.response.expect("ok");
        assert_eq!(value["success"], true);
        assert_eq!(value["effect"], "compressor");
        assert_eq!(value["bypassed"], true);
        match result.command {
            Some(Command::SetEffectBypass { effect, bypassed }) => {
                assert_eq!(effect, "compressor");
                assert!(bypassed);
            }
            other => panic!("expected SetEffectBypass command, got {other:?}"),
        }
    }

    #[test]
    fn set_effect_param_emits_command_with_full_payload() {
        let result = handle_request(
            &Method::SetEffectParam {
                effect: "gate".into(),
                param: "th".into(),
                value: -32.5,
            },
            &empty_state(),
        );
        assert!(result.response.is_ok());
        match result.command {
            Some(Command::SetEffectParam { effect, param, value }) => {
                assert_eq!(effect, "gate");
                assert_eq!(param, "th");
                assert!((value - -32.5).abs() < 1e-6);
            }
            other => panic!("expected SetEffectParam command, got {other:?}"),
        }
    }

    #[test]
    fn load_effect_preset_passes_name_through() {
        let result = handle_request(
            &Method::LoadEffectPreset { name: "Streaming".into() },
            &empty_state(),
        );
        let value = result.response.expect("ok");
        assert_eq!(value["preset"], "Streaming");
        match result.command {
            Some(Command::LoadEffectPreset { name }) => assert_eq!(name, "Streaming"),
            other => panic!("expected LoadEffectPreset command, got {other:?}"),
        }
    }

    #[test]
    fn reset_effect_chain_emits_unit_command() {
        let result = handle_request(&Method::ResetEffectChain, &empty_state());
        assert!(result.response.is_ok());
        assert!(matches!(result.command, Some(Command::ResetEffectChain)));
    }

    // Sanity check that the existing handlers we didn't touch still
    // route correctly — guards against an accidental match-arm
    // ordering bug in the file.
    #[test]
    fn set_mic_mute_still_routes_to_set_mic_mute_command() {
        let result = handle_request(
            &Method::SetMicMute { muted: true },
            &empty_state(),
        );
        assert!(result.response.is_ok());
        assert!(matches!(result.command, Some(Command::SetMicMute { muted: true })));
    }
}
