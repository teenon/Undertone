import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import "./App.css";
import EffectsPanel, { MicChainSnapshot } from "./components/EffectsPanel";

interface Snapshot {
  device_connected: boolean;
  device_serial: string | null;
  device_model: string | null;
  mic_muted: boolean | null;
  mic_gain: number | null;
  headphone_volume?: number | null;
  default_sink?: string | null;
  default_source?: string | null;
  mic_chain?: MicChainSnapshot | null;
  state: unknown;
  channels?: unknown[];
  app_routes?: unknown[];
}

/// Map a PipeWire node name to a short, human-readable label.
/// Falls back to the raw name when no rule matches.
function friendlyDeviceName(node: string | null | undefined): string {
  if (!node) return "—";
  if (/Wave_XLR/i.test(node)) return "Elgato Wave XLR";
  if (/Wave_3/i.test(node)) return "Elgato Wave:3";
  if (/Wave_1/i.test(node)) return "Elgato Wave:1";
  if (/XLR_Dock/i.test(node)) return "Elgato XLR Dock";
  if (node.startsWith("alsa_output.pci-") || node.startsWith("alsa_input.pci-"))
    return "Built-in Audio";
  if (node.startsWith("bluez_")) return "Bluetooth";
  return node;
}

type ConnectionState =
  | { kind: "connecting" }
  | { kind: "connected"; snapshot: Snapshot }
  | { kind: "error"; message: string };

const POLL_INTERVAL_MS = 500;

export default function App() {
  const [connection, setConnection] = useState<ConnectionState>({
    kind: "connecting",
  });
  // Optimistic local overrides — used between user input and the next
  // snapshot poll so sliders/buttons feel instantaneous. Cleared once
  // the snapshot catches up.
  const [pendingMute, setPendingMute] = useState<boolean | null>(null);
  const [pendingGain, setPendingGain] = useState<number | null>(null);
  const [pendingHpVol, setPendingHpVol] = useState<number | null>(null);

  const refresh = useCallback(async () => {
    try {
      const snapshot = await invoke<Snapshot>("get_state_snapshot");
      setConnection({ kind: "connected", snapshot });
      // Drop optimistic overrides once the daemon agrees.
      setPendingMute((p) => (p === null || p === snapshot.mic_muted ? null : p));
      setPendingGain((p) =>
        p === null || (snapshot.mic_gain !== null && Math.abs(p - snapshot.mic_gain) < 0.01)
          ? null
          : p,
      );
      setPendingHpVol((p) =>
        p === null ||
        (snapshot.headphone_volume != null &&
          Math.abs(p - snapshot.headphone_volume) < 0.02)
          ? null
          : p,
      );
    } catch (e) {
      setConnection({ kind: "error", message: String(e) });
    }
  }, []);

  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        await invoke("connect_daemon");
        if (cancelled) return;
        await refresh();
      } catch (e) {
        if (!cancelled)
          setConnection({ kind: "error", message: String(e) });
      }
    })();
    const id = window.setInterval(() => {
      void refresh();
    }, POLL_INTERVAL_MS);

    // WebKit pauses / throttles `setInterval` when the window is
    // hidden or unfocused, which means a daemon restart that happens
    // while the window is in the tray (or even just behind another
    // window) never gets noticed by the auto-reconnect path until the
    // user looks at the app. Force an immediate refresh on every
    // focus / visibility transition so the UI heals as soon as the
    // user brings it forward.
    const wake = () => {
      void refresh();
    };
    window.addEventListener("focus", wake);
    document.addEventListener("visibilitychange", () => {
      if (!document.hidden) wake();
    });

    return () => {
      cancelled = true;
      window.clearInterval(id);
      window.removeEventListener("focus", wake);
      document.removeEventListener("visibilitychange", wake);
    };
  }, [refresh]);

  const snapshot =
    connection.kind === "connected" ? connection.snapshot : null;
  const connected = snapshot?.device_connected ?? false;
  const serial = snapshot?.device_serial ?? null;
  const deviceModel = snapshot?.device_model ?? null;
  const muted = pendingMute ?? snapshot?.mic_muted ?? false;
  // `undefined` here means "no value yet — the first snapshot poll
  // hasn't returned". The slider renders disabled with `—` in that
  // case so we don't show a misleading 0% before the daemon's real
  // value arrives.
  const gain: number | undefined =
    pendingGain ?? snapshot?.mic_gain ?? undefined;
  const headphoneVolume: number | undefined =
    pendingHpVol ?? snapshot?.headphone_volume ?? undefined;

  const toggleMute = async () => {
    const next = !muted;
    setPendingMute(next);
    try {
      await invoke("set_mic_mute", { muted: next });
    } catch (e) {
      setPendingMute(null);
      setConnection({ kind: "error", message: String(e) });
    }
  };

  const updateGain = async (value: number) => {
    setPendingGain(value);
    try {
      await invoke("set_mic_gain", { gain: value });
    } catch (e) {
      setPendingGain(null);
      setConnection({ kind: "error", message: String(e) });
    }
  };

  const updateHeadphoneVolume = async (value: number) => {
    setPendingHpVol(value);
    try {
      await invoke("set_headphone_volume", { volume: value });
    } catch (e) {
      setPendingHpVol(null);
      setConnection({ kind: "error", message: String(e) });
    }
  };

  return (
    <div className="min-h-screen bg-zinc-950 text-zinc-100">
      <header className="border-b border-zinc-800/60 px-8 py-4">
        <div className="flex items-center justify-between">
          <div className="flex items-center gap-3">
            <div className="h-7 w-7 rounded-md bg-gradient-to-br from-emerald-400 to-emerald-600" />
            <h1 className="text-lg font-semibold tracking-tight">Undertone</h1>
          </div>
          <StatusPill state={connection} />
        </div>
      </header>

      <main className="mx-auto max-w-3xl px-8 py-8 space-y-6">
        {connection.kind === "error" && (
          <div className="rounded-lg border border-red-800/60 bg-red-900/20 p-4 text-sm text-red-200">
            <div className="font-medium mb-1">Can't talk to the daemon.</div>
            <div className="font-mono text-xs text-red-300/80 break-all">
              {connection.message}
            </div>
            <div className="mt-2 text-xs text-red-300/70">
              Is <code>undertone-daemon</code> running?
            </div>
          </div>
        )}

        <section className="rounded-2xl border border-zinc-800/60 bg-zinc-900/50 p-6 backdrop-blur">
          <div className="flex items-start justify-between gap-4">
            <div>
              <div className="text-xs uppercase tracking-widest text-zinc-500">
                Device
              </div>
              <div className="mt-1 text-xl font-medium">
                {connected
                  ? deviceModel ?? "Connected device"
                  : "No device detected"}
              </div>
              {serial && (
                <div className="mt-0.5 font-mono text-xs text-zinc-500">
                  {serial}
                </div>
              )}
            </div>
            <span
              className={`mt-2 inline-flex h-2.5 w-2.5 rounded-full ${
                connected
                  ? "bg-emerald-500 shadow-[0_0_8px_rgba(16,185,129,0.6)]"
                  : "bg-zinc-600"
              }`}
            />
          </div>

          <div className="mt-8 space-y-6">
            <div>
              <div className="mb-2 flex items-baseline justify-between">
                <label className="text-sm font-medium text-zinc-300">
                  Mic Gain
                </label>
                <span className="font-mono text-sm tabular-nums text-zinc-400">
                  {gain === undefined ? "—" : `${Math.round(gain * 100)}%`}
                </span>
              </div>
              <input
                type="range"
                min={0}
                max={1}
                step={0.01}
                value={gain ?? 0}
                onChange={(e) => void updateGain(parseFloat(e.target.value))}
                disabled={!connected || gain === undefined}
                className="h-1.5 w-full cursor-pointer appearance-none rounded-full bg-zinc-800 accent-emerald-500 disabled:cursor-not-allowed disabled:opacity-40"
              />
            </div>

            <div>
              <div className="mb-2 flex items-baseline justify-between">
                <label className="text-sm font-medium text-zinc-300">
                  Headphone Volume
                </label>
                <span className="font-mono text-sm tabular-nums text-zinc-400">
                  {headphoneVolume === undefined
                    ? "—"
                    : `${Math.round(headphoneVolume * 100)}%`}
                </span>
              </div>
              <input
                type="range"
                min={0}
                max={1}
                step={0.01}
                value={headphoneVolume ?? 0}
                onChange={(e) =>
                  void updateHeadphoneVolume(parseFloat(e.target.value))
                }
                disabled={!connected || headphoneVolume === undefined}
                className="h-1.5 w-full cursor-pointer appearance-none rounded-full bg-zinc-800 accent-sky-500 disabled:cursor-not-allowed disabled:opacity-40"
              />
            </div>

            <button
              type="button"
              onClick={() => void toggleMute()}
              disabled={!connected}
              className={`w-full rounded-xl px-4 py-3 text-sm font-semibold transition-colors disabled:cursor-not-allowed disabled:bg-zinc-800 disabled:text-zinc-500 ${
                muted
                  ? "bg-red-600 text-white hover:bg-red-500"
                  : "bg-emerald-600 text-white hover:bg-emerald-500"
              }`}
            >
              {muted ? "Unmute microphone" : "Mute microphone"}
            </button>
          </div>
        </section>

        <EffectsPanel chain={snapshot?.mic_chain ?? null} />

        <section className="rounded-2xl border border-zinc-800/60 bg-zinc-900/50 p-6">
          <div className="text-xs uppercase tracking-widest text-zinc-500">
            System default audio
          </div>
          <dl className="mt-3 grid grid-cols-[6rem_1fr] gap-x-4 gap-y-2 text-sm">
            <dt className="text-zinc-500">Output</dt>
            <dd
              className="truncate text-zinc-200"
              title={snapshot?.default_sink ?? ""}
            >
              {friendlyDeviceName(snapshot?.default_sink)}
            </dd>
            <dt className="text-zinc-500">Input</dt>
            <dd
              className="truncate text-zinc-200"
              title={snapshot?.default_source ?? ""}
            >
              {friendlyDeviceName(snapshot?.default_source)}
            </dd>
          </dl>
          <p className="mt-3 text-xs text-zinc-500">
            Change these in your system Sound settings. Hover for the raw
            PipeWire node name.
          </p>
        </section>

        <section className="rounded-2xl border border-zinc-800/60 bg-zinc-900/50 p-6">
          <div className="text-xs uppercase tracking-widest text-zinc-500">
            Channels
          </div>
          <div className="mt-1 text-sm text-zinc-400">
            {connection.kind === "connected"
              ? `${connection.snapshot.channels?.length ?? 0} channel${
                  (connection.snapshot.channels?.length ?? 0) === 1 ? "" : "s"
                } loaded`
              : "—"}
          </div>
          <p className="mt-4 text-xs text-zinc-500">
            Mixer strips land in a later iteration. For now this confirms the
            Tauri ↔ daemon link is alive.
          </p>
        </section>
      </main>
    </div>
  );
}

function StatusPill({ state }: { state: ConnectionState }) {
  if (state.kind === "connecting") {
    return (
      <span className="inline-flex items-center gap-1.5 rounded-full border border-zinc-700 px-2.5 py-0.5 text-xs text-zinc-400">
        <span className="h-1.5 w-1.5 animate-pulse rounded-full bg-zinc-500" />
        Connecting
      </span>
    );
  }
  if (state.kind === "error") {
    return (
      <span className="inline-flex items-center gap-1.5 rounded-full border border-red-800 bg-red-950/40 px-2.5 py-0.5 text-xs text-red-300">
        <span className="h-1.5 w-1.5 rounded-full bg-red-500" />
        Disconnected
      </span>
    );
  }
  return (
    <span className="inline-flex items-center gap-1.5 rounded-full border border-emerald-800 bg-emerald-950/40 px-2.5 py-0.5 text-xs text-emerald-300">
      <span className="h-1.5 w-1.5 rounded-full bg-emerald-400" />
      Connected
    </span>
  );
}
