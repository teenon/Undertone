import { useEffect, useState } from "react";
import {
  disable,
  enable,
  isEnabled,
} from "@tauri-apps/plugin-autostart";

type ToggleState = "loading" | "on" | "off" | "error";

export default function SettingsPanel() {
  const [state, setState] = useState<ToggleState>("loading");
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    isEnabled()
      .then((on) => {
        if (!cancelled) setState(on ? "on" : "off");
      })
      .catch((e) => {
        if (!cancelled) {
          setState("error");
          setError(String(e));
        }
      });
    return () => {
      cancelled = true;
    };
  }, []);

  const toggle = async () => {
    const currentlyOn = state === "on";
    const prev = state;
    setState(currentlyOn ? "off" : "on");
    setError(null);
    try {
      if (currentlyOn) {
        await disable();
      } else {
        await enable();
      }
    } catch (e) {
      setState(prev);
      setError(String(e));
    }
  };

  const isOn = state === "on";
  const loading = state === "loading";

  return (
    <section className="rounded-2xl border border-zinc-800/60 bg-zinc-900/50 p-6 backdrop-blur">
      <div className="text-xs uppercase tracking-widest text-zinc-500">
        Startup
      </div>
      <div className="mt-4 flex items-start justify-between gap-6">
        <div>
          <div className="text-sm font-medium text-zinc-200">
            Start Undertone on login
          </div>
          <p className="mt-1 text-xs leading-relaxed text-zinc-500">
            Launches the mixer window when you log in. The audio daemon
            already starts automatically via its systemd user unit.
          </p>
        </div>
        <button
          type="button"
          role="switch"
          aria-checked={isOn}
          aria-label="Start Undertone on login"
          disabled={loading}
          onClick={() => void toggle()}
          className={`relative inline-flex h-6 w-11 shrink-0 items-center rounded-full border transition-colors disabled:cursor-not-allowed disabled:opacity-50 ${
            isOn
              ? "border-emerald-400/60 bg-emerald-500/70"
              : "border-zinc-600 bg-zinc-800"
          }`}
        >
          <span
            className={`inline-block h-4 w-4 transform rounded-full bg-zinc-50 shadow transition-transform ${
              isOn ? "translate-x-6" : "translate-x-1"
            }`}
          />
        </button>
      </div>
      {error && (
        <div className="mt-3 rounded-md border border-red-800/60 bg-red-900/20 px-3 py-2 text-xs text-red-200">
          {error}
        </div>
      )}
    </section>
  );
}
