import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import "./App.css";

interface Snapshot {
  device_connected: boolean;
  device_serial: string | null;
  state: unknown;
  channels?: unknown[];
  app_routes?: unknown[];
}

type ConnectionState =
  | { kind: "connecting" }
  | { kind: "connected"; snapshot: Snapshot }
  | { kind: "error"; message: string };

const POLL_INTERVAL_MS = 1000;

export default function App() {
  const [connection, setConnection] = useState<ConnectionState>({
    kind: "connecting",
  });
  const [muted, setMuted] = useState(false);
  const [gain, setGain] = useState(0.5);
  const suppressPoll = useRef(false);

  const refresh = useCallback(async () => {
    if (suppressPoll.current) return;
    try {
      const snapshot = await invoke<Snapshot>("get_state_snapshot");
      setConnection({ kind: "connected", snapshot });
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
    return () => {
      cancelled = true;
      window.clearInterval(id);
    };
  }, [refresh]);

  const toggleMute = async () => {
    suppressPoll.current = true;
    try {
      const next = !muted;
      await invoke("set_mic_mute", { muted: next });
      setMuted(next);
    } catch (e) {
      setConnection({ kind: "error", message: String(e) });
    } finally {
      suppressPoll.current = false;
    }
  };

  const updateGain = async (value: number) => {
    setGain(value);
    suppressPoll.current = true;
    try {
      await invoke("set_mic_gain", { gain: value });
    } catch (e) {
      setConnection({ kind: "error", message: String(e) });
    } finally {
      suppressPoll.current = false;
    }
  };

  const connected =
    connection.kind === "connected" && connection.snapshot.device_connected;
  const serial =
    connection.kind === "connected"
      ? connection.snapshot.device_serial
      : null;

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
                {connected ? "Elgato Wave XLR" : "No device detected"}
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
                  {Math.round(gain * 100)}%
                </span>
              </div>
              <input
                type="range"
                min={0}
                max={1}
                step={0.01}
                value={gain}
                onChange={(e) => void updateGain(parseFloat(e.target.value))}
                disabled={!connected}
                className="h-1.5 w-full cursor-pointer appearance-none rounded-full bg-zinc-800 accent-emerald-500 disabled:cursor-not-allowed disabled:opacity-40"
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
