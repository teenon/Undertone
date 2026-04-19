import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
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

function MuteIconButton({
  muted,
  disabled,
  onClick,
}: {
  muted: boolean;
  disabled: boolean;
  onClick: () => void;
}) {
  const label = muted ? "Unmute microphone" : "Mute microphone";
  const stateClasses = disabled
    ? "border-zinc-700 text-zinc-600 cursor-not-allowed"
    : muted
    ? "border-red-500/70 bg-red-500/10 text-red-400 hover:bg-red-500/20"
    : "border-emerald-500/60 text-emerald-400 hover:bg-emerald-500/10";
  return (
    <button
      type="button"
      onClick={onClick}
      disabled={disabled}
      aria-pressed={muted}
      aria-label={label}
      title={label}
      className={`flex h-9 w-9 shrink-0 items-center justify-center rounded-lg border transition-colors ${stateClasses}`}
    >
      <svg
        viewBox="0 0 24 24"
        fill="none"
        stroke="currentColor"
        strokeWidth="2"
        strokeLinecap="round"
        strokeLinejoin="round"
        aria-hidden="true"
        className="h-[18px] w-[18px]"
      >
        <rect x="9" y="2" width="6" height="12" rx="3" />
        <path d="M5 10v2a7 7 0 0 0 14 0v-2" />
        <line x1="12" y1="19" x2="12" y2="22" />
        <line x1="8" y1="22" x2="16" y2="22" />
        {muted && <line x1="4" y1="4" x2="20" y2="20" />}
      </svg>
    </button>
  );
}

function DeafenIconButton({
  deafened,
  disabled,
  onClick,
}: {
  deafened: boolean;
  disabled: boolean;
  onClick: () => void;
}) {
  const label = deafened ? "Undeafen" : "Deafen";
  const stateClasses = disabled
    ? "border-zinc-700 text-zinc-600 cursor-not-allowed"
    : deafened
    ? "border-red-500/70 bg-red-500/10 text-red-400 hover:bg-red-500/20"
    : "border-sky-500/60 text-sky-400 hover:bg-sky-500/10";
  return (
    <button
      type="button"
      onClick={onClick}
      disabled={disabled}
      aria-pressed={deafened}
      aria-label={label}
      title={label}
      className={`flex h-9 w-9 shrink-0 items-center justify-center rounded-lg border transition-colors ${stateClasses}`}
    >
      <svg
        viewBox="0 0 24 24"
        fill="none"
        stroke="currentColor"
        strokeWidth="2"
        strokeLinecap="round"
        strokeLinejoin="round"
        aria-hidden="true"
        className="h-[18px] w-[18px]"
      >
        <path d="M3 14v-2a9 9 0 0 1 18 0v2" />
        <path d="M3 14h4v6H5a2 2 0 0 1-2-2v-4z" />
        <path d="M21 14h-4v6h2a2 2 0 0 0 2-2v-4z" />
        {deafened && <line x1="4" y1="4" x2="20" y2="20" />}
      </svg>
    </button>
  );
}

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
  // Deafen is UI-only: when non-null, holds the volume to restore on undeafen.
  // Daemon just sees a set_headphone_volume(0) → set_headphone_volume(prev).
  const [savedVolume, setSavedVolume] = useState<number | null>(null);

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

    // Recursive setTimeout instead of setInterval. setInterval gets
    // aggressively throttled / paused by webkit2gtk on minimized or
    // unfocused windows, which is why the UI was getting stuck on
    // stale state. setTimeout chained per-call always fires the next
    // tick on the runtime's first opportunity once the page is alive
    // again, so we resume polling immediately on window-show.
    const tick = async () => {
      if (cancelled) return;
      try {
        await refresh();
      } catch {
        // refresh sets its own error state; just keep the loop alive
      }
      if (cancelled) return;
      window.setTimeout(() => void tick(), POLL_INTERVAL_MS);
    };

    (async () => {
      try {
        await invoke("connect_daemon");
        if (cancelled) return;
        void tick();
      } catch (e) {
        if (!cancelled)
          setConnection({ kind: "error", message: String(e) });
      }
    })();

    // Belt-and-braces: every wake-up path forces an immediate
    // refresh so the UI snaps back the moment the user looks at it.
    // - JS focus event (browser-level)
    // - JS visibilitychange (HTML5)
    // - Tauri native window-focus (works even when webkit ignored
    //   the JS event)
    // - Pointer entering the window
    const wake = () => void refresh();
    window.addEventListener("focus", wake);
    document.addEventListener("visibilitychange", wake);
    window.addEventListener("pointerenter", wake);

    // Tauri 2's `onFocusChanged` fires from the Rust side and isn't
    // throttled by webkit2gtk's sleep-the-JS-runtime behaviour, so
    // it's the most reliable wake signal we have.
    let unlistenTauriFocus: (() => void) | undefined;
    void getCurrentWindow()
      .onFocusChanged(({ payload: focused }) => {
        if (focused) wake();
      })
      .then((un) => {
        if (cancelled) un();
        else unlistenTauriFocus = un;
      });

    return () => {
      cancelled = true;
      window.removeEventListener("focus", wake);
      document.removeEventListener("visibilitychange", wake);
      window.removeEventListener("pointerenter", wake);
      unlistenTauriFocus?.();
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

  const toggleDeafen = async () => {
    const target = savedVolume !== null ? savedVolume : 0;
    const nextSaved =
      savedVolume !== null ? null : headphoneVolume ?? 0;
    setPendingHpVol(target);
    try {
      await invoke("set_headphone_volume", { volume: target });
      setSavedVolume(nextSaved);
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
              <div className="flex items-center gap-3">
                <MuteIconButton
                  muted={muted}
                  disabled={!connected}
                  onClick={() => void toggleMute()}
                />
                <input
                  type="range"
                  min={0}
                  max={1}
                  step={0.01}
                  value={gain ?? 0}
                  onChange={(e) => void updateGain(parseFloat(e.target.value))}
                  disabled={!connected || gain === undefined}
                  className="h-1.5 flex-1 cursor-pointer appearance-none rounded-full bg-zinc-800 accent-emerald-500 disabled:cursor-not-allowed disabled:opacity-40"
                />
              </div>
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
              <div className="flex items-center gap-3">
                <DeafenIconButton
                  deafened={savedVolume !== null}
                  disabled={!connected || headphoneVolume === undefined}
                  onClick={() => void toggleDeafen()}
                />
                <input
                  type="range"
                  min={0}
                  max={1}
                  step={0.01}
                  value={headphoneVolume ?? 0}
                  onChange={(e) =>
                    void updateHeadphoneVolume(parseFloat(e.target.value))
                  }
                  disabled={
                    !connected ||
                    headphoneVolume === undefined ||
                    savedVolume !== null
                  }
                  className="h-1.5 flex-1 cursor-pointer appearance-none rounded-full bg-zinc-800 accent-sky-500 disabled:cursor-not-allowed disabled:opacity-40"
                />
              </div>
            </div>

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
