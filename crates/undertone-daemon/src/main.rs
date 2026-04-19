//! Undertone Daemon - `PipeWire` audio control service.
//!
//! This is the main entry point for the Undertone daemon, which manages
//! `PipeWire` audio routing, persistence, and Elgato Wave hardware
//! integration via the `undertone-hid` [`Device`] trait.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::time::sleep;
use tracing::{debug, error, info, warn};
use tracing_subscriber::EnvFilter;

mod config;
mod server;
mod signals;

use undertone_core::channel::ChannelState;
use undertone_core::state::{DaemonState, StateSnapshot};
use undertone_db::{Database, DeviceSettings};
use undertone_effects::{EffectKind, MicChain, PresetName};
use undertone_hid::{Device, scan_devices};
use undertone_ipc::{
    AppDiscoveredData, ChannelMuteChangedData, ChannelVolumeChangedData, DeviceConnectedData,
    Event, EventType, IpcServer, socket_path,
};
use undertone_pipewire::filter_chain;
use undertone_pipewire::{GraphEvent, GraphManager, PipeWireRuntime};

/// Default channels to create
const DEFAULT_CHANNELS: &[&str] = &["system", "voice", "music", "browser", "game"];

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::from_default_env()
                .add_directive("undertone=info".parse()?)
                .add_directive("undertone_daemon=debug".parse()?)
                .add_directive("undertone_pipewire=debug".parse()?),
        )
        .init();

    info!(version = env!("CARGO_PKG_VERSION"), "Starting Undertone daemon");

    // Bail early if other undertone-daemon processes are already
    // running. Two daemons fighting over the IPC socket and the Wave
    // XLR's USB interface produces the famously confusing "sliders
    // disabled in the UI but `nc` shows good values" symptom — the
    // Tauri client ends up talking to whichever daemon is the loser
    // (no USB access). Logged loudly so it's obvious in journalctl.
    check_for_duplicate_daemons();

    // Load configuration
    let _config = config::load_config()?;
    info!("Configuration loaded");

    // Open database
    let db = Database::open().context("Failed to open database")?;
    info!("Database initialized");

    // Scan for supported Elgato devices. Handles are returned as
    // `Arc<dyn Device>` so new models plug in here without changes.
    // `devices` is mutable so the background re-scan branch below can
    // pick up a device that wasn't ready at startup (classic case:
    // daemon started on login before `snd_usb_audio` finished its
    // init transfers and `scan_devices()` came up empty).
    let mut devices: Vec<Arc<dyn Device>> = match scan_devices() {
        Ok(list) if list.is_empty() => {
            info!("No Elgato audio devices detected at startup; will retry in the background");
            Vec::new()
        }
        Ok(list) => {
            for d in &list {
                info!(
                    model = d.model().name(),
                    serial = d.serial(),
                    "Registered device"
                );
                apply_persisted_settings(d, &db);
            }
            list
        }
        Err(e) => {
            warn!(error = %e, "Device scan failed; continuing without mic control");
            Vec::new()
        }
    };
    let mut device_connected = !devices.is_empty();
    let mut device_serial = devices.first().map(|d| d.serial().to_string());

    // Load channels from database
    let mut channels: Vec<ChannelState> = db.load_channels().context("Failed to load channels")?;
    info!(count = channels.len(), "Loaded channels from database");

    // Load routing rules
    let mut routes = db.load_routes().context("Failed to load routes")?;
    info!(count = routes.len(), "Loaded routing rules");

    // Track active app routes
    let mut active_apps: Vec<undertone_core::routing::AppRoute> = vec![];

    // Mic effect chain. v1: lives in memory; defaults to "Off" (every
    // effect bypassed). The daemon writes a config drop-in for
    // PipeWire to pick up at its next start; runtime parameter
    // changes go through `pw-cli set-param` and don't persist across
    // PipeWire restarts (the drop-in defaults do).
    let mut mic_chain = MicChain::default();
    match filter_chain::install_config(&mic_chain.to_pipewire_config_drop_in()) {
        Ok(path) => info!(path = %path.display(), "Wrote mic-chain config drop-in"),
        Err(e) => warn!(error = %e, "Failed to write mic-chain config drop-in; effects panel will be inert"),
    }

    // Initialize PipeWire graph manager
    let graph = Arc::new(GraphManager::new());

    // Spawn PipeWire runtime
    info!("Starting PipeWire runtime...");
    let (pw_runtime, mut graph_event_rx) =
        PipeWireRuntime::spawn(Arc::clone(&graph)).context("Failed to spawn PipeWire runtime")?;

    // Wait for PipeWire connection
    info!("Waiting for PipeWire connection...");
    let mut connected = false;
    while let Some(event) = graph_event_rx.recv().await {
        if matches!(event, GraphEvent::Connected) {
            connected = true;
            info!("PipeWire connected!");
            break;
        }
    }

    if !connected {
        error!("Failed to connect to PipeWire");
        return Err(anyhow::anyhow!("PipeWire connection failed"));
    }

    // Give PipeWire a moment to enumerate existing nodes
    sleep(Duration::from_millis(500)).await;

    // Create virtual channel sinks
    info!("Creating virtual channel sinks...");
    match pw_runtime.create_channel_sinks(DEFAULT_CHANNELS) {
        Ok(created) => {
            info!(count = created.len(), "Created channel sinks");
            for node in &created {
                graph.record_created_node(node.name.clone(), node.id);
            }
        }
        Err(e) => {
            error!(error = %e, "Failed to create channel sinks");
            // Continue anyway - we might be able to recover
        }
    }

    // Create mix nodes
    info!("Creating mix nodes...");
    match pw_runtime.create_mix_nodes() {
        Ok(created) => {
            info!(count = created.len(), "Created mix nodes");
            for node in &created {
                graph.record_created_node(node.name.clone(), node.id);
            }
        }
        Err(e) => {
            error!(error = %e, "Failed to create mix nodes");
        }
    }

    // Create volume filter nodes for each channel
    info!("Creating volume filter nodes...");
    match pw_runtime.create_channel_volume_filters(DEFAULT_CHANNELS) {
        Ok(created) => {
            info!(count = created.len(), "Created volume filter nodes");
            for (name, id) in &created {
                graph.record_created_node(name.clone(), *id);
            }
        }
        Err(e) => {
            error!(error = %e, "Failed to create volume filter nodes");
        }
    }

    // Wait for nodes and ports to be fully registered in PipeWire before creating links.
    // The registry receives global events asynchronously, so we need to wait for our
    // newly created nodes to appear before we can look them up for linking.
    info!("Waiting for node and port discovery...");
    sleep(Duration::from_millis(1500)).await;

    // Create links from channels through volume filters to mix nodes
    info!("Creating channel-to-mix links with volume filters...");
    match pw_runtime.create_channel_to_mix_links_with_filters() {
        Ok(created) => {
            info!(count = created.len(), "Created channel-to-mix links with volume filters");
            for (description, id) in &created {
                graph.record_created_link(description.clone(), *id);
            }
        }
        Err(e) => {
            error!(error = %e, "Failed to create channel-to-mix links");
        }
    }

    // Link monitor-mix to headphones if Wave:3 is connected
    if let Some(wave3_sink) = graph.find_wave3_sink() {
        info!(sink_name = %wave3_sink.name, "Linking monitor-mix to Wave:3 headphones...");
        match pw_runtime.link_monitor_to_headphones() {
            Ok((left_id, right_id)) => {
                info!("Monitor-mix linked to headphones");
                graph.record_created_link("monitor-mix->wave3-sink:FL".to_string(), left_id);
                graph.record_created_link("monitor-mix->wave3-sink:FR".to_string(), right_id);
            }
            Err(e) => {
                warn!(error = %e, "Failed to link monitor-mix to headphones (Wave:3 may not be connected)");
            }
        }
    } else {
        info!("Wave:3 sink not found - skipping monitor-mix to headphones link");
    }

    // Start IPC server
    let socket = socket_path();
    info!(?socket, "Starting IPC server");
    let (ipc_server, mut request_rx) =
        IpcServer::bind(&socket).await.context("Failed to start IPC server")?;

    // Get event sender for broadcasting events to IPC clients
    let event_tx = ipc_server.event_sender();

    // Spawn IPC server task
    let ipc_handle = tokio::spawn(async move {
        ipc_server.run().await;
    });

    // Set up signal handling
    let mut shutdown_rx = signals::setup_signal_handlers()?;

    // Build initial state snapshot
    let mut state = DaemonState::Running;
    let mut active_profile = String::from("Default");
    let mut mixer = undertone_core::mixer::MixerState::default();
    // Track current monitor output device (defaults to Wave:3 headphones)
    let mut monitor_output = String::from("wave3-sink");

    // Load default profile on startup (apply channel states to PipeWire)
    if let Ok(Some(default_profile_name)) = db.get_default_profile()
        && let Ok(Some(profile)) = db.load_profile(&default_profile_name)
    {
        info!(name = %default_profile_name, "Loading default profile");
        active_profile = default_profile_name.clone();
        for profile_ch in &profile.channels {
            if let Some(ch) = channels.iter_mut().find(|c| c.config.name == profile_ch.name) {
                ch.stream_volume = profile_ch.stream_volume;
                ch.stream_muted = profile_ch.stream_muted;
                ch.monitor_volume = profile_ch.monitor_volume;
                ch.monitor_muted = profile_ch.monitor_muted;

                // Apply to PipeWire filter nodes
                use undertone_core::mixer::MixType;
                for (mix, volume, muted) in [
                    (MixType::Stream, ch.stream_volume, ch.stream_muted),
                    (MixType::Monitor, ch.monitor_volume, ch.monitor_muted),
                ] {
                    let filter_name = match mix {
                        MixType::Stream => format!("ut-ch-{}-stream-vol", ch.config.name),
                        MixType::Monitor => format!("ut-ch-{}-monitor-vol", ch.config.name),
                    };
                    if let Some(node_id) = graph.get_created_node_id(&filter_name) {
                        let _ = pw_runtime.set_node_volume(node_id, volume);
                        let _ = pw_runtime.set_node_mute(node_id, muted);
                    }
                }
            }
        }
        // Only replace routes if the profile has custom routes defined
        // Otherwise, keep the global routes from app_routes table
        if !profile.routes.is_empty() {
            routes = profile.routes;
        }
    }

    info!("Daemon running. Press Ctrl+C to exit.");

    // If `devices` is empty we keep retrying `scan_devices` every 5 s
    // in the background so a late-appearing Wave XLR / Wave:3 (classic
    // boot race: daemon started before snd_usb_audio finished init)
    // gets picked up without the user having to restart the daemon.
    // The check is O(1) when `devices` is non-empty, so this keeps
    // running harmlessly forever.
    let mut device_scan_retry = tokio::time::interval(Duration::from_secs(5));
    device_scan_retry.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // First tick fires immediately; skip it — we already scanned at startup.
    device_scan_retry.tick().await;

    // Main event loop
    loop {
        tokio::select! {
            // Handle PipeWire graph events
            Some(event) = graph_event_rx.recv() => {
                match event {
                    GraphEvent::Connected => {
                        info!("PipeWire reconnected");
                        state = DaemonState::Reconciling;
                        // TODO: Trigger reconciliation
                    }

                    GraphEvent::Disconnected => {
                        warn!("PipeWire disconnected");
                        state = DaemonState::Error("PipeWire disconnected".to_string());
                    }

                    GraphEvent::Wave3Detected { serial } => {
                        info!(serial = %serial, "Wave:3 detected");
                        device_connected = true;
                        device_serial = Some(serial.clone());
                        state = DaemonState::Running;

                        // Opportunistic re-scan: if startup missed the
                        // device (snd_usb_audio race), PipeWire telling
                        // us it's here now is the best trigger to try
                        // again — no need to wait for the 5 s retry.
                        if devices.is_empty()
                            && let Some((new_serial, _)) =
                                rescan_and_register(&mut devices, &event_tx, &db)
                        {
                            device_serial = Some(new_serial);
                        }

                        // Emit IPC event
                        let _ = event_tx.send(Event {
                            event: EventType::DeviceConnected,
                            data: serde_json::to_value(DeviceConnectedData {
                                serial: serial.clone(),
                            })
                            .unwrap_or_default(),
                        });

                        // Try to link monitor-mix to headphones now that Wave:3 is connected
                        if !graph.get_created_links().contains_key("monitor-mix->wave3-sink:FL") {
                            match pw_runtime.link_monitor_to_headphones() {
                                Ok((left_id, right_id)) => {
                                    info!("Monitor-mix linked to Wave:3 headphones");
                                    graph.record_created_link("monitor-mix->wave3-sink:FL".to_string(), left_id);
                                    graph.record_created_link("monitor-mix->wave3-sink:FR".to_string(), right_id);
                                }
                                Err(e) => {
                                    warn!(error = %e, "Failed to link monitor-mix to headphones");
                                }
                            }
                        }
                    }

                    GraphEvent::Wave3Removed => {
                        warn!("Wave:3 disconnected");
                        device_connected = false;
                        device_serial = None;
                        state = DaemonState::DeviceDisconnected;
                        // Drop the now-dangling device handle so the
                        // next `Wave3Detected` can create a fresh one
                        // with whatever's currently on the bus.
                        devices.clear();

                        // Emit IPC event
                        let _ = event_tx.send(Event {
                            event: EventType::DeviceDisconnected,
                            data: serde_json::json!({}),
                        });
                    }

                    GraphEvent::NodeAdded(node) => {
                        debug!(id = node.id, name = %node.name, "Node added to graph");
                        // Node is already added to graph by the PipeWire thread
                    }

                    GraphEvent::NodeRemoved { id, name } => {
                        debug!(id, name = %name, "Node removed from graph");
                        graph.remove_node(id);

                        // Check if one of our nodes was removed
                        if name.starts_with("ut-") {
                            warn!(name = %name, "Undertone node was removed - may need reconciliation");
                        }
                    }

                    GraphEvent::PortAdded(port) => {
                        debug!(id = port.id, name = %port.name, node_id = port.node_id, "Port added");
                    }

                    GraphEvent::PortRemoved { id } => {
                        debug!(id, "Port removed");
                        graph.remove_port(id);
                    }

                    GraphEvent::LinkCreated { id, output_node, input_node } => {
                        debug!(id, output_node, input_node, "Link created");
                    }

                    GraphEvent::LinkRemoved { id } => {
                        debug!(id, "Link removed");
                    }

                    GraphEvent::ClientAppeared { id, name, pid } => {
                        info!(id, name = %name, pid = ?pid, "Audio client appeared");

                        // Get the app's binary name from the graph if available
                        let binary_name = graph.get_node(id).and_then(|n| n.binary_name.clone());

                        // Find the target channel based on routing rules
                        let target_channel = undertone_core::routing::find_channel_for_app(
                            &name,
                            binary_name.as_deref(),
                            &routes,
                        );

                        info!(
                            app_id = id,
                            app_name = %name,
                            channel = %target_channel,
                            "Routing new app to channel"
                        );

                        // Check if this is a persistent (saved) route
                        let is_persistent = routes.iter().any(|r| {
                            r.matches(&name) || binary_name.as_ref().is_some_and(|b| r.matches(b))
                        });

                        // Route the app to the target channel
                        match pw_runtime.route_app_to_channel(id, &target_channel) {
                            Ok(link_ids) => {
                                debug!(
                                    app_id = id,
                                    links_created = link_ids.len(),
                                    "App routed successfully"
                                );
                            }
                            Err(e) => {
                                warn!(
                                    app_id = id,
                                    error = %e,
                                    "Failed to route app (may be transient)"
                                );
                            }
                        }

                        // Track this app route
                        active_apps.push(undertone_core::routing::AppRoute {
                            app_id: id,
                            app_name: name.clone(),
                            binary_name: binary_name.clone(),
                            pid,
                            channel: target_channel.clone(),
                            is_persistent,
                        });

                        // Emit IPC event
                        let _ = event_tx.send(Event {
                            event: EventType::AppDiscovered,
                            data: serde_json::to_value(AppDiscoveredData {
                                app_id: id,
                                name: name.clone(),
                                binary: binary_name,
                                pid,
                                channel: target_channel,
                            })
                            .unwrap_or_default(),
                        });
                    }

                    GraphEvent::ClientDisappeared { id } => {
                        debug!(id, "Audio client disappeared");

                        // Remove from active apps tracking
                        active_apps.retain(|app| app.app_id != id);

                        // Emit IPC event
                        let _ = event_tx.send(Event {
                            event: EventType::AppRemoved,
                            data: serde_json::json!({ "app_id": id }),
                        });
                    }
                }
            }

            // Handle IPC requests
            Some((client_id, request, response_tx)) = request_rx.recv() => {
                debug!(client_id, request_id = request.id, "Handling IPC request");

                // Build current state snapshot
                let profiles = db.list_profiles().unwrap_or_default();

                // Get available output devices from PipeWire
                use undertone_core::state::OutputDevice;
                let output_devices: Vec<OutputDevice> = graph
                    .get_audio_output_devices()
                    .into_iter()
                    .map(|n| OutputDevice {
                        name: n.name.clone(),
                        description: n.description.clone().unwrap_or_else(|| n.name.clone()),
                        node_id: n.id,
                    })
                    .collect();

                // Pull live mic state from the active device so the
                // snapshot reflects physical knob/tag-button changes
                // since the last command. ~5 ms over USB; fine at the
                // typical IPC poll cadence.
                let (mic_muted, mic_gain, headphone_volume, device_model) =
                    if let Some(d) = devices.first() {
                        let model = Some(d.model().name().to_string());
                        match d.get_state() {
                            Ok(s) => (
                                Some(s.mic_muted),
                                Some(s.mic_gain),
                                Some(s.headphone_volume),
                                model,
                            ),
                            Err(e) => {
                                debug!(error = %e, "device.get_state failed; snapshot omits device state");
                                (None, None, None, model)
                            }
                        }
                    } else {
                        (None, None, None, None)
                    };

                let default_sink = pactl_default("sink");
                let default_source = pactl_default("source");

                let snapshot = StateSnapshot {
                    state: state.clone(),
                    device_connected,
                    device_serial: device_serial.clone(),
                    channels: channels.clone(),
                    app_routes: active_apps.clone(),
                    mixer: mixer.clone(),
                    active_profile: active_profile.clone(),
                    profiles,
                    output_devices,
                    monitor_output: monitor_output.clone(),
                    created_nodes: graph.get_created_nodes(),
                    created_links: graph.get_created_links(),
                    mic_muted,
                    mic_gain,
                    headphone_volume,
                    device_model,
                    default_sink,
                    default_source,
                    mic_chain: Some(mic_chain.snapshot()),
                };

                let handle_result = server::handle_request(&request.method, &snapshot);
                let response = undertone_ipc::Response {
                    id: request.id,
                    result: handle_result.response,
                };
                let _ = response_tx.send(response).await;

                // Process command if one was returned
                if let Some(cmd) = handle_result.command {
                    use undertone_core::Command;
                    use undertone_core::mixer::MixType;

                    match cmd {
                        Command::SetChannelVolume { channel, mix, volume } => {
                            if let Some(ch) = channels.iter_mut().find(|c| c.config.name == channel) {
                                match mix {
                                    MixType::Stream => ch.stream_volume = volume,
                                    MixType::Monitor => ch.monitor_volume = volume,
                                }
                                info!(channel = %channel, ?mix, volume, "Channel volume updated");

                                // Apply to PipeWire volume filter node
                                let filter_name = match mix {
                                    MixType::Stream => format!("ut-ch-{channel}-stream-vol"),
                                    MixType::Monitor => format!("ut-ch-{channel}-monitor-vol"),
                                };
                                if let Some(node_id) = graph.get_created_node_id(&filter_name) {
                                    if let Err(e) = pw_runtime.set_node_volume(node_id, volume) {
                                        error!(error = %e, filter = %filter_name, "Failed to set volume on filter node");
                                    } else {
                                        debug!(filter = %filter_name, volume, "Volume applied to PipeWire");
                                    }
                                } else {
                                    warn!(filter = %filter_name, "Volume filter node not found");
                                }

                                // Emit event
                                let _ = event_tx.send(Event {
                                    event: EventType::ChannelVolumeChanged,
                                    data: serde_json::to_value(ChannelVolumeChangedData {
                                        channel: channel.clone(),
                                        mix,
                                        volume,
                                    }).unwrap_or_default(),
                                });
                            }
                        }

                        Command::SetChannelMute { channel, mix, muted } => {
                            if let Some(ch) = channels.iter_mut().find(|c| c.config.name == channel) {
                                match mix {
                                    MixType::Stream => ch.stream_muted = muted,
                                    MixType::Monitor => ch.monitor_muted = muted,
                                }
                                info!(channel = %channel, ?mix, muted, "Channel mute updated");

                                // Apply to PipeWire volume filter node
                                let filter_name = match mix {
                                    MixType::Stream => format!("ut-ch-{channel}-stream-vol"),
                                    MixType::Monitor => format!("ut-ch-{channel}-monitor-vol"),
                                };
                                if let Some(node_id) = graph.get_created_node_id(&filter_name) {
                                    if let Err(e) = pw_runtime.set_node_mute(node_id, muted) {
                                        error!(error = %e, filter = %filter_name, "Failed to set mute on filter node");
                                    } else {
                                        debug!(filter = %filter_name, muted, "Mute applied to PipeWire");
                                    }
                                } else {
                                    warn!(filter = %filter_name, "Volume filter node not found");
                                }

                                // Emit event
                                let _ = event_tx.send(Event {
                                    event: EventType::ChannelMuteChanged,
                                    data: serde_json::to_value(ChannelMuteChangedData {
                                        channel: channel.clone(),
                                        mix,
                                        muted,
                                    }).unwrap_or_default(),
                                });
                            }
                        }

                        Command::SetMasterVolume { mix, volume } => {
                            // Update mixer state
                            match mix {
                                MixType::Stream => mixer.stream_master_volume = volume,
                                MixType::Monitor => mixer.monitor_master_volume = volume,
                            }
                            info!(?mix, volume, "Master volume updated");

                            // Apply to the mix node in PipeWire
                            let mix_node_name = match mix {
                                MixType::Stream => "ut-stream-mix",
                                MixType::Monitor => "ut-monitor-mix",
                            };
                            if let Some(node_id) = graph.get_created_node_id(mix_node_name) {
                                if let Err(e) = pw_runtime.set_node_volume(node_id, volume) {
                                    error!(error = %e, node = %mix_node_name, "Failed to set master volume");
                                } else {
                                    debug!(node = %mix_node_name, volume, "Master volume applied to PipeWire");
                                }
                            } else {
                                warn!(node = %mix_node_name, "Mix node not found for master volume");
                            }
                        }

                        Command::SetMasterMute { mix, muted } => {
                            // Update mixer state
                            match mix {
                                MixType::Stream => mixer.stream_master_muted = muted,
                                MixType::Monitor => mixer.monitor_master_muted = muted,
                            }
                            info!(?mix, muted, "Master mute updated");

                            // Apply to the mix node in PipeWire
                            let mix_node_name = match mix {
                                MixType::Stream => "ut-stream-mix",
                                MixType::Monitor => "ut-monitor-mix",
                            };
                            if let Some(node_id) = graph.get_created_node_id(mix_node_name) {
                                if let Err(e) = pw_runtime.set_node_mute(node_id, muted) {
                                    error!(error = %e, node = %mix_node_name, "Failed to set master mute");
                                } else {
                                    debug!(node = %mix_node_name, muted, "Master mute applied to PipeWire");
                                }
                            } else {
                                warn!(node = %mix_node_name, "Mix node not found for master mute");
                            }
                        }

                        Command::SetAppRoute { app_pattern, channel } => {
                            use undertone_core::routing::{PatternType, RouteRule};

                            // Update in-memory routes
                            routes.retain(|r| r.pattern != app_pattern);
                            let rule = RouteRule::new(
                                app_pattern.clone(),
                                PatternType::Exact,
                                channel.clone(),
                                100,
                            );
                            routes.push(rule.clone());
                            info!(app_pattern = %app_pattern, channel = %channel, "App route set");

                            // Save to database
                            if let Err(e) = db.save_route(&rule) {
                                error!(error = %e, "Failed to save route to database");
                            }

                            // Apply routing to matching active apps
                            let audio_clients = pw_runtime.get_audio_clients();
                            for client in audio_clients {
                                // Check if this client matches the pattern
                                let matches = client.application_name.as_ref().is_some_and(|name| {
                                    rule.matches(name)
                                }) || client.binary_name.as_ref().is_some_and(|name| {
                                    rule.matches(name)
                                }) || rule.matches(&client.name);

                                if matches {
                                    info!(
                                        app_id = client.id,
                                        app_name = %client.name,
                                        channel = %channel,
                                        "Re-routing matching app"
                                    );
                                    match pw_runtime.route_app_to_channel(client.id, &channel) {
                                        Ok(link_ids) => {
                                            debug!(
                                                app_id = client.id,
                                                links_created = link_ids.len(),
                                                "App re-routed successfully"
                                            );

                                            // Update active_apps tracking
                                            if let Some(app) = active_apps.iter_mut().find(|a| a.app_id == client.id) {
                                                app.channel = channel.clone();
                                                app.is_persistent = true;
                                            }
                                        }
                                        Err(e) => {
                                            warn!(
                                                app_id = client.id,
                                                error = %e,
                                                "Failed to re-route app"
                                            );
                                        }
                                    }
                                }
                            }
                        }

                        Command::RemoveAppRoute { app_pattern } => {
                            routes.retain(|r| r.pattern != app_pattern);
                            info!(app_pattern = %app_pattern, "App route removed");

                            // Remove from database
                            if let Err(e) = db.delete_route(&app_pattern) {
                                error!(error = %e, "Failed to remove route from database");
                            }
                        }

                        Command::SaveProfile { name } => {
                            use undertone_core::profile::{Profile, ProfileChannel};

                            // Build profile from current state
                            let profile_channels: Vec<ProfileChannel> = channels
                                .iter()
                                .map(ProfileChannel::from)
                                .collect();

                            let profile = Profile {
                                name: name.clone(),
                                description: None,
                                is_default: name == "Default",
                                channels: profile_channels,
                                routes: routes.clone(),
                                mixer: mixer.clone(),
                            };

                            match db.save_profile(&profile) {
                                Ok(()) => {
                                    info!(name = %name, "Profile saved");
                                }
                                Err(e) => {
                                    error!(name = %name, error = %e, "Failed to save profile");
                                }
                            }
                        }

                        Command::LoadProfile { name } => {
                            match db.load_profile(&name) {
                                Ok(Some(profile)) => {
                                    info!(name = %name, "Loading profile");

                                    // Apply channel volumes
                                    for profile_ch in &profile.channels {
                                        if let Some(ch) = channels.iter_mut()
                                            .find(|c| c.config.name == profile_ch.name)
                                        {
                                            ch.stream_volume = profile_ch.stream_volume;
                                            ch.stream_muted = profile_ch.stream_muted;
                                            ch.monitor_volume = profile_ch.monitor_volume;
                                            ch.monitor_muted = profile_ch.monitor_muted;

                                            // Apply to PipeWire filter nodes
                                            use undertone_core::mixer::MixType;
                                            for (mix, volume, muted) in [
                                                (MixType::Stream, ch.stream_volume, ch.stream_muted),
                                                (MixType::Monitor, ch.monitor_volume, ch.monitor_muted),
                                            ] {
                                                let filter_name = match mix {
                                                    MixType::Stream => format!("ut-ch-{}-stream-vol", ch.config.name),
                                                    MixType::Monitor => format!("ut-ch-{}-monitor-vol", ch.config.name),
                                                };
                                                if let Some(node_id) = graph.get_created_node_id(&filter_name) {
                                                    let _ = pw_runtime.set_node_volume(node_id, volume);
                                                    let _ = pw_runtime.set_node_mute(node_id, muted);
                                                }
                                            }
                                        }
                                    }

                                    // Replace routes only if profile has custom routes
                                    if !profile.routes.is_empty() {
                                        routes = profile.routes;
                                    }

                                    // Apply mixer state (master volumes)
                                    mixer = profile.mixer.clone();

                                    // Apply master volumes to PipeWire mix nodes
                                    for (mix_type, volume, muted) in [
                                        (MixType::Stream, mixer.stream_master_volume, mixer.stream_master_muted),
                                        (MixType::Monitor, mixer.monitor_master_volume, mixer.monitor_master_muted),
                                    ] {
                                        let mix_node_name = match mix_type {
                                            MixType::Stream => "ut-stream-mix",
                                            MixType::Monitor => "ut-monitor-mix",
                                        };
                                        if let Some(node_id) = graph.get_created_node_id(mix_node_name) {
                                            let _ = pw_runtime.set_node_volume(node_id, volume);
                                            let _ = pw_runtime.set_node_mute(node_id, muted);
                                        }
                                    }

                                    // Update active profile name
                                    active_profile = name.clone();

                                    info!(name = %name, "Profile loaded and applied");
                                }
                                Ok(None) => {
                                    warn!(name = %name, "Profile not found");
                                }
                                Err(e) => {
                                    error!(name = %name, error = %e, "Failed to load profile");
                                }
                            }
                        }

                        Command::DeleteProfile { name } => {
                            match db.delete_profile(&name) {
                                Ok(true) => {
                                    info!(name = %name, "Profile deleted");
                                }
                                Ok(false) => {
                                    warn!(name = %name, "Cannot delete profile (may be default or not found)");
                                }
                                Err(e) => {
                                    error!(name = %name, error = %e, "Failed to delete profile");
                                }
                            }
                        }

                        Command::SetMicGain { gain } => {
                            if let Some(device) = devices.first() {
                                match device.set_gain(gain) {
                                    Ok(()) => {
                                        info!(
                                            model = device.model().name(),
                                            gain,
                                            "Mic gain set"
                                        );
                                        persist_device_settings(device, &db);
                                    }
                                    Err(e) => {
                                        error!(error = %e, "Failed to set mic gain");
                                    }
                                }
                            } else {
                                warn!("Mic control not available (no device)");
                            }
                        }

                        Command::SetMicMute { muted } => {
                            if let Some(device) = devices.first() {
                                match device.set_mute(muted) {
                                    Ok(()) => {
                                        info!(
                                            model = device.model().name(),
                                            muted,
                                            "Mic mute set"
                                        );
                                        persist_device_settings(device, &db);
                                    }
                                    Err(e) => {
                                        error!(error = %e, "Failed to set mic mute");
                                    }
                                }
                            } else {
                                warn!("Mic control not available (no device)");
                            }
                        }

                        Command::SetHeadphoneVolume { volume } => {
                            if let Some(device) = devices.first() {
                                match device.set_headphone_volume(volume) {
                                    Ok(()) => {
                                        info!(
                                            model = device.model().name(),
                                            volume,
                                            "Headphone volume set"
                                        );
                                        persist_device_settings(device, &db);
                                    }
                                    Err(e) => {
                                        error!(error = %e, "Failed to set headphone volume");
                                    }
                                }
                            } else {
                                warn!("Headphone control not available (no device)");
                            }
                        }

                        Command::SetEffectBypass { effect, bypassed } => {
                            apply_effect_change(&mut mic_chain, &effect, |inst| {
                                inst.bypassed = bypassed;
                            });
                            push_chain_to_pipewire(&mic_chain, &effect);
                            info!(effect = %effect, bypassed, "Effect bypass updated");
                        }

                        Command::SetEffectParam { effect, param, value } => {
                            apply_effect_change(&mut mic_chain, &effect, |inst| {
                                inst.params.insert(param.clone(), value);
                            });
                            push_chain_to_pipewire(&mic_chain, &effect);
                            // Tweaking a param invalidates the "matches a
                            // preset" claim — mark as custom so the UI's
                            // dropdown reflects reality.
                            mic_chain.preset = None;
                            debug!(effect = %effect, param = %param, value, "Effect param updated");
                        }

                        Command::LoadEffectPreset { name } => {
                            if let Some(preset) = PresetName::from_label(&name) {
                                mic_chain = preset.build_chain();
                                if let Err(e) = filter_chain::install_config(
                                    &mic_chain.to_pipewire_config_drop_in(),
                                ) {
                                    warn!(error = %e, "Failed to rewrite mic-chain config");
                                }
                                // Re-apply every effect's runtime state so the
                                // change shows up without a PipeWire restart.
                                for kind in EffectKind::all() {
                                    push_chain_to_pipewire(&mic_chain, kind.node_id());
                                }
                                info!(preset = %name, "Loaded effect preset");
                            } else {
                                warn!(preset = %name, "Unknown effect preset");
                            }
                        }

                        Command::ResetEffectChain => {
                            mic_chain = MicChain::default();
                            if let Err(e) = filter_chain::install_config(
                                &mic_chain.to_pipewire_config_drop_in(),
                            ) {
                                warn!(error = %e, "Failed to rewrite mic-chain config");
                            }
                            for kind in EffectKind::all() {
                                push_chain_to_pipewire(&mic_chain, kind.node_id());
                            }
                            info!("Reset effect chain to defaults");
                        }

                        Command::SetMonitorOutput { device_name } => {
                            info!(device = %device_name, "Switching monitor output");

                            // Unlink from current output device
                            if let Err(e) = pw_runtime.unlink_monitor_from_output(&monitor_output) {
                                warn!(error = %e, current = %monitor_output, "Failed to unlink from current output");
                            }

                            // Link to new output device
                            match pw_runtime.link_monitor_to_output(&device_name) {
                                Ok((left_id, right_id)) => {
                                    info!(device = %device_name, "Monitor output switched successfully");
                                    // Update tracked links
                                    graph.record_created_link(format!("monitor-mix->{device_name}:FL"), left_id);
                                    graph.record_created_link(format!("monitor-mix->{device_name}:FR"), right_id);
                                    // Update current monitor output
                                    monitor_output = device_name;
                                }
                                Err(e) => {
                                    error!(error = %e, device = %device_name, "Failed to link to new output device");
                                    // Try to restore connection to previous device
                                    if let Err(restore_err) = pw_runtime.link_monitor_to_output(&monitor_output) {
                                        error!(error = %restore_err, "Failed to restore previous output");
                                    }
                                }
                            }
                        }

                        Command::Reconcile => {
                            state = DaemonState::Reconciling;
                            // TODO: Implement full reconciliation
                            info!("Reconciliation triggered");
                            state = DaemonState::Running;
                        }

                        Command::Shutdown => {
                            info!("Shutdown command processed");
                            break;
                        }
                    }
                }
            }

            // Handle shutdown signal
            _ = shutdown_rx.recv() => {
                info!("Shutdown signal received");
                break;
            }

            // Background retry: if we came up before the device was
            // enumerable (snd_usb_audio race on login) try again every
            // 5 s. No-op when already populated.
            _ = device_scan_retry.tick() => {
                if devices.is_empty()
                    && let Some((new_serial, _)) =
                        rescan_and_register(&mut devices, &event_tx, &db)
                {
                    device_connected = true;
                    device_serial = Some(new_serial);
                    if matches!(state, DaemonState::DeviceDisconnected) {
                        state = DaemonState::Running;
                    }
                }
            }
        }
    }

    // Cleanup
    info!("Shutting down...");
    pw_runtime.shutdown();
    ipc_handle.abort();

    info!("Undertone daemon stopped");
    Ok(())
}

/// Call `scan_devices()` and, on success, swap the result into
/// `devices`. Emits a `DeviceConnected` IPC event for every newly
/// registered device, and returns `(first_serial, first_model)` for
/// the caller's convenience. Returns `None` when the scan still
/// turns up no devices.
fn rescan_and_register(
    devices: &mut Vec<Arc<dyn Device>>,
    event_tx: &tokio::sync::broadcast::Sender<Event>,
    db: &Database,
) -> Option<(String, String)> {
    match scan_devices() {
        Ok(list) if list.is_empty() => None,
        Ok(list) => {
            let mut first: Option<(String, String)> = None;
            for d in &list {
                let serial = d.serial().to_string();
                let model = d.model().name().to_string();
                info!(
                    model = %model,
                    serial = %serial,
                    "Re-scan picked up device"
                );
                apply_persisted_settings(d, db);
                if first.is_none() {
                    first = Some((serial.clone(), model.clone()));
                }
                let _ = event_tx.send(Event {
                    event: EventType::DeviceConnected,
                    data: serde_json::to_value(DeviceConnectedData { serial })
                        .unwrap_or_default(),
                });
            }
            *devices = list;
            first
        }
        Err(e) => {
            debug!(error = %e, "Device re-scan errored");
            None
        }
    }
}

/// On startup / hotplug, push any previously-persisted firmware values
/// to the device so it returns to the state the user left it in,
/// regardless of what the device firmware currently holds. No row yet
/// → no-op (respect the firmware's current values on first-ever connect).
fn apply_persisted_settings(device: &Arc<dyn Device>, db: &Database) {
    let serial = device.serial();
    match db.load_device_settings(serial) {
        Ok(Some(s)) => {
            if let Err(e) = device.set_gain(s.mic_gain) {
                warn!(error = %e, serial, "Failed to restore mic gain from db");
            }
            if let Err(e) = device.set_mute(s.mic_muted) {
                warn!(error = %e, serial, "Failed to restore mic mute from db");
            }
            if let Err(e) = device.set_headphone_volume(s.headphone_volume) {
                warn!(error = %e, serial, "Failed to restore headphone volume from db");
            }
            info!(
                serial,
                mic_gain = s.mic_gain,
                mic_muted = s.mic_muted,
                headphone_volume = s.headphone_volume,
                "Restored persisted device settings"
            );
        }
        Ok(None) => {
            debug!(serial, "No persisted settings for this device yet");
        }
        Err(e) => {
            warn!(error = %e, serial, "Failed to load persisted device settings");
        }
    }
}

/// Snapshot the device's current state and write the full triple to
/// the db. Called after every successful Set* so a crash or daemon
/// restart can restore the exact same values.
fn persist_device_settings(device: &Arc<dyn Device>, db: &Database) {
    let serial = device.serial();
    let state = match device.get_state() {
        Ok(s) => s,
        Err(e) => {
            warn!(error = %e, serial, "Failed to read device state for persistence");
            return;
        }
    };
    let settings = DeviceSettings {
        mic_gain: state.mic_gain,
        mic_muted: state.mic_muted,
        headphone_volume: state.headphone_volume,
    };
    if let Err(e) = db.save_device_settings(serial, &settings) {
        warn!(error = %e, serial, "Failed to persist device settings");
    }
}

/// Locate one effect by its kind-string (`noise_suppression`, `gate`,
/// `compressor`, `equalizer`) and apply a mutation. Logs on miss.
fn apply_effect_change<F: FnOnce(&mut undertone_effects::EffectInstance)>(
    chain: &mut MicChain,
    effect: &str,
    mutate: F,
) {
    match parse_effect_kind(effect) {
        Some(kind) => {
            if let Some(inst) = chain.effect_mut(kind) {
                mutate(inst);
            }
        }
        None => warn!(effect, "Unknown effect kind"),
    }
}

/// Push the current state of `kind`'s effect to PipeWire via
/// `pw-cli set-param`. Best-effort: if the chain isn't loaded yet
/// (user hasn't restarted PipeWire), the lookup fails and we just
/// log at debug level — the parameter is still cached in
/// `mic_chain` and will apply at the next `PipeWire` start via the
/// drop-in config.
///
/// `kind_or_node_name` accepts either an `EffectKind` string
/// (preferred) or a raw filter-graph node identifier.
fn push_chain_to_pipewire(chain: &MicChain, kind_or_node_name: &str) {
    let Some(kind) = parse_effect_kind(kind_or_node_name)
        .or_else(|| node_id_to_kind(kind_or_node_name))
    else {
        debug!(name = kind_or_node_name, "push_chain_to_pipewire: unknown effect");
        return;
    };
    let Some(inst) = chain.effect(kind) else {
        return;
    };
    let node_id = match filter_chain::lookup_node_id(undertone_effects::chain::PROCESSED_SOURCE_NAME) {
        Ok(id) => id,
        Err(e) => {
            debug!(error = %e, "filter chain not loaded yet; skipping live param update");
            return;
        }
    };
    // Bypass first (LSP plugins respect `bp`; RNNoise has no port —
    // the chain config emitter zeroes its threshold instead).
    let bp_value = if inst.bypassed { 1.0 } else { 0.0 };
    if !matches!(kind, EffectKind::NoiseSuppression) {
        let _ = filter_chain::set_control(node_id, "bp", bp_value);
    }
    for (control, value) in &inst.params {
        if let Err(e) = filter_chain::set_control(node_id, control, *value) {
            debug!(control = %control, error = %e, "set_control failed");
        }
    }
}

fn parse_effect_kind(s: &str) -> Option<EffectKind> {
    match s {
        "noise_suppression" | "NoiseSuppression" => Some(EffectKind::NoiseSuppression),
        "gate" | "Gate" => Some(EffectKind::Gate),
        "compressor" | "Compressor" => Some(EffectKind::Compressor),
        "equalizer" | "Equalizer" => Some(EffectKind::Equalizer),
        _ => None,
    }
}

fn node_id_to_kind(node_id: &str) -> Option<EffectKind> {
    EffectKind::all()
        .iter()
        .copied()
        .find(|k| k.node_id() == node_id)
}

/// Warn loudly if other `undertone-daemon` processes are already
/// running. We don't kill them — that'd be presumptuous — but we
/// surface them in the log so the user (or a future session of me)
/// notices before the IPC traffic is split between daemons.
///
/// Implementation walks `/proc` rather than shelling out to `pgrep`
/// because the kernel's `comm` field truncates to 15 chars, which
/// turns `undertone-daemon` into `undertone-daemo` and breaks
/// `pgrep -x`. `/proc/<pid>/exe` resolves to the full binary path,
/// which we compare against our own.
fn check_for_duplicate_daemons() {
    let our_pid = std::process::id();
    let our_exe = match std::fs::read_link(format!("/proc/{our_pid}/exe")) {
        Ok(p) => p,
        Err(e) => {
            debug!(error = %e, "couldn't read own /proc exe; skipping duplicate check");
            return;
        }
    };

    let entries = match std::fs::read_dir("/proc") {
        Ok(it) => it,
        Err(_) => return,
    };

    let mut others: Vec<u32> = Vec::new();
    for entry in entries.flatten() {
        let Some(name) = entry.file_name().to_str().and_then(|s| s.parse::<u32>().ok()) else {
            continue;
        };
        if name == our_pid {
            continue;
        }
        let exe_link = format!("/proc/{name}/exe");
        if let Ok(exe) = std::fs::read_link(&exe_link)
            && exe == our_exe
        {
            others.push(name);
        }
    }

    if !others.is_empty() {
        others.sort_unstable();
        let pids = others
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(" ");
        warn!(
            our_pid,
            others = %pids,
            "Detected {} other undertone-daemon process(es) running. \
             Only one daemon should be alive at a time — multiples \
             fight over the IPC socket and the Wave XLR USB interface, \
             which presents as null mic state in the Tauri UI. \
             Kill the orphans with: kill -9 {pids}",
            others.len(),
        );
    }
}

/// Query PipeWire (via `pactl`) for the current default sink or source
/// node name. Returns `None` if the subprocess fails or the output
/// can't be parsed.
fn pactl_default(kind: &str) -> Option<String> {
    let arg = format!("get-default-{kind}");
    let output = std::process::Command::new("pactl").arg(&arg).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let s = String::from_utf8(output.stdout).ok()?;
    let trimmed = s.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_effect_kind_accepts_snake_and_camel() {
        assert_eq!(parse_effect_kind("noise_suppression"), Some(EffectKind::NoiseSuppression));
        assert_eq!(parse_effect_kind("NoiseSuppression"), Some(EffectKind::NoiseSuppression));
        assert_eq!(parse_effect_kind("gate"), Some(EffectKind::Gate));
        assert_eq!(parse_effect_kind("Gate"), Some(EffectKind::Gate));
        assert_eq!(parse_effect_kind("compressor"), Some(EffectKind::Compressor));
        assert_eq!(parse_effect_kind("Compressor"), Some(EffectKind::Compressor));
        assert_eq!(parse_effect_kind("equalizer"), Some(EffectKind::Equalizer));
        assert_eq!(parse_effect_kind("Equalizer"), Some(EffectKind::Equalizer));
    }

    #[test]
    fn parse_effect_kind_rejects_unknown() {
        assert_eq!(parse_effect_kind(""), None);
        assert_eq!(parse_effect_kind("eq"), None); // node_id, not kind name
        assert_eq!(parse_effect_kind("limiter"), None);
        assert_eq!(parse_effect_kind("NOISESUPPRESSION"), None);
    }

    #[test]
    fn node_id_to_kind_round_trips_with_effect_kind_node_id() {
        for &kind in EffectKind::all() {
            assert_eq!(node_id_to_kind(kind.node_id()), Some(kind));
        }
    }

    #[test]
    fn node_id_to_kind_rejects_unknown_ids() {
        assert_eq!(node_id_to_kind("foobar"), None);
        assert_eq!(node_id_to_kind(""), None);
        assert_eq!(node_id_to_kind("Compressor"), None); // kind name, not node_id
    }

    #[test]
    fn apply_effect_change_mutates_the_named_effect_only() {
        let mut chain = MicChain::default();
        // Sanity: gate starts bypassed.
        assert!(chain.effect(EffectKind::Gate).unwrap().bypassed);
        apply_effect_change(&mut chain, "gate", |inst| {
            inst.bypassed = false;
        });
        assert!(!chain.effect(EffectKind::Gate).unwrap().bypassed);
        // Other effects untouched.
        assert!(chain.effect(EffectKind::Compressor).unwrap().bypassed);
        assert!(chain.effect(EffectKind::Equalizer).unwrap().bypassed);
    }

    #[test]
    fn apply_effect_change_is_a_noop_for_unknown_kind() {
        let mut chain = MicChain::default();
        let before = chain
            .effects
            .iter()
            .map(|e| e.bypassed)
            .collect::<Vec<_>>();
        apply_effect_change(&mut chain, "not_a_real_effect", |inst| {
            inst.bypassed = false;
        });
        let after = chain
            .effects
            .iter()
            .map(|e| e.bypassed)
            .collect::<Vec<_>>();
        assert_eq!(before, after, "unknown kind should leave the chain alone");
    }

    #[test]
    fn apply_effect_change_persists_param_writes() {
        let mut chain = MicChain::default();
        apply_effect_change(&mut chain, "compressor", |inst| {
            inst.params.insert("th".into(), -25.0);
        });
        let v = chain
            .effect(EffectKind::Compressor)
            .unwrap()
            .params
            .get("th")
            .copied();
        assert_eq!(v, Some(-25.0));
    }
}
