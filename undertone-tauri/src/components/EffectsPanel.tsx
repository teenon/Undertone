import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

export type EffectKind = "noise_suppression" | "gate" | "compressor" | "equalizer";

export interface OwnedParamDescriptor {
  name: string;
  label: string;
  min: number;
  max: number;
  default: number;
  step: number;
  unit: string;
}

export interface EffectSnapshot {
  kind: EffectKind;
  bypassed: boolean;
  params: Record<string, number>;
  descriptors: OwnedParamDescriptor[];
}

export interface MicChainSnapshot {
  effects: EffectSnapshot[];
  preset: string | null;
}

interface Props {
  chain: MicChainSnapshot | null | undefined;
}

const PRESETS = ["Off", "Voice", "Streaming", "Singing"];

const KIND_LABELS: Record<EffectKind, string> = {
  noise_suppression: "Noise Suppression",
  gate: "Noise Gate",
  compressor: "Compressor",
  equalizer: "Equalizer",
};

function paramKey(effect: EffectKind, name: string): string {
  return `${effect}.${name}`;
}

export default function EffectsPanel({ chain }: Props) {
  // Optimistic overrides keyed by `<effect>.<param>` (and
  // `<effect>.bypass` for the toggle). Cleared whenever the snapshot
  // catches up with our last write.
  const [pending, setPending] = useState<Record<string, number | boolean>>({});
  const [open, setOpen] = useState<Record<EffectKind, boolean>>({
    noise_suppression: false,
    gate: false,
    compressor: false,
    equalizer: false,
  });
  const lastWriteAt = useRef<Record<string, number>>({});

  // Reconverge: drop pending entries that match the snapshot value
  // (within an epsilon for floats) once a poll cycle later.
  useEffect(() => {
    if (!chain) return;
    setPending((prev) => {
      const next = { ...prev };
      let changed = false;
      for (const eff of chain.effects) {
        const bypassKey = paramKey(eff.kind, "bypass");
        if (bypassKey in next && next[bypassKey] === eff.bypassed) {
          delete next[bypassKey];
          changed = true;
        }
        for (const [name, value] of Object.entries(eff.params)) {
          const key = paramKey(eff.kind, name);
          if (key in next) {
            const v = next[key];
            if (typeof v === "number" && Math.abs(v - value) < 0.01) {
              delete next[key];
              changed = true;
            }
          }
        }
      }
      return changed ? next : prev;
    });
  }, [chain]);

  const toggleBypass = useCallback(
    async (kind: EffectKind, currentlyBypassed: boolean) => {
      const next = !currentlyBypassed;
      const key = paramKey(kind, "bypass");
      setPending((p) => ({ ...p, [key]: next }));
      lastWriteAt.current[key] = Date.now();
      try {
        await invoke("set_effect_bypass", { effect: kind, bypassed: next });
      } catch (e) {
        console.error("set_effect_bypass failed", e);
        setPending((p) => {
          const { [key]: _drop, ...rest } = p;
          return rest;
        });
      }
    },
    [],
  );

  const setParam = useCallback(
    async (kind: EffectKind, name: string, value: number) => {
      const key = paramKey(kind, name);
      setPending((p) => ({ ...p, [key]: value }));
      lastWriteAt.current[key] = Date.now();
      try {
        await invoke("set_effect_param", {
          effect: kind,
          param: name,
          value,
        });
      } catch (e) {
        console.error("set_effect_param failed", e);
      }
    },
    [],
  );

  const loadPreset = useCallback(async (name: string) => {
    try {
      await invoke("load_effect_preset", { name });
      // Clear all overrides — server is authoritative now.
      setPending({});
    } catch (e) {
      console.error("load_effect_preset failed", e);
    }
  }, []);

  if (!chain) {
    return (
      <section className="rounded-2xl border border-zinc-800/60 bg-zinc-900/50 p-6">
        <div className="text-xs uppercase tracking-widest text-zinc-500">
          Effects
        </div>
        <p className="mt-2 text-sm text-zinc-400">
          Loading effect chain…
        </p>
      </section>
    );
  }

  return (
    <section className="rounded-2xl border border-zinc-800/60 bg-zinc-900/50 p-6">
      <div className="flex items-center justify-between gap-4">
        <div>
          <div className="text-xs uppercase tracking-widest text-zinc-500">
            Effects
          </div>
          <div className="mt-1 text-sm text-zinc-400">
            {chain.preset
              ? `Preset: ${chain.preset}`
              : "Custom (any saved preset will override your tweaks)"}
          </div>
        </div>
        <select
          value={chain.preset ?? ""}
          onChange={(e) => void loadPreset(e.target.value)}
          className="rounded-md border border-zinc-700 bg-zinc-900 px-3 py-1.5 text-sm text-zinc-200 focus:border-emerald-600 focus:outline-none"
        >
          <option value="" disabled hidden>
            Custom
          </option>
          {PRESETS.map((p) => (
            <option key={p} value={p}>
              {p}
            </option>
          ))}
        </select>
      </div>

      <div className="mt-5 space-y-3">
        {chain.effects.map((eff) => {
          const bypassed =
            (pending[paramKey(eff.kind, "bypass")] as boolean | undefined) ??
            eff.bypassed;
          const isOpen = open[eff.kind];
          return (
            <div
              key={eff.kind}
              className={`rounded-xl border ${
                bypassed
                  ? "border-zinc-800 bg-zinc-900/30"
                  : "border-emerald-800/60 bg-emerald-950/10"
              }`}
            >
              <div className="flex items-center gap-3 px-4 py-3">
                <button
                  type="button"
                  className="text-zinc-400 hover:text-zinc-200"
                  onClick={() =>
                    setOpen((o) => ({ ...o, [eff.kind]: !o[eff.kind] }))
                  }
                  aria-label={isOpen ? "Collapse" : "Expand"}
                >
                  {isOpen ? "▾" : "▸"}
                </button>
                <span
                  className={`text-sm font-medium ${
                    bypassed ? "text-zinc-400" : "text-zinc-100"
                  }`}
                >
                  {KIND_LABELS[eff.kind]}
                </span>
                <span className="ml-auto" />
                <button
                  type="button"
                  onClick={() => void toggleBypass(eff.kind, bypassed)}
                  className={`rounded-full px-3 py-1 text-xs font-semibold transition-colors ${
                    bypassed
                      ? "bg-zinc-800 text-zinc-400 hover:bg-zinc-700"
                      : "bg-emerald-600 text-white hover:bg-emerald-500"
                  }`}
                >
                  {bypassed ? "Off" : "On"}
                </button>
              </div>
              {isOpen && (
                <div className="grid grid-cols-1 gap-x-6 gap-y-3 border-t border-zinc-800/60 px-4 py-4 sm:grid-cols-2">
                  {eff.descriptors.map((d) => {
                    const value =
                      (pending[paramKey(eff.kind, d.name)] as
                        | number
                        | undefined) ??
                      eff.params[d.name] ??
                      d.default;
                    return (
                      <label key={d.name} className="block text-xs">
                        <div className="mb-1 flex justify-between text-zinc-400">
                          <span>{d.label}</span>
                          <span className="font-mono tabular-nums text-zinc-300">
                            {value.toFixed(d.step >= 1 ? 0 : 2)} {d.unit}
                          </span>
                        </div>
                        <input
                          type="range"
                          min={d.min}
                          max={d.max}
                          step={d.step}
                          value={value}
                          disabled={bypassed}
                          onChange={(e) =>
                            void setParam(
                              eff.kind,
                              d.name,
                              parseFloat(e.target.value),
                            )
                          }
                          className="h-1.5 w-full cursor-pointer appearance-none rounded-full bg-zinc-800 accent-emerald-500 disabled:cursor-not-allowed disabled:opacity-40"
                        />
                      </label>
                    );
                  })}
                </div>
              )}
            </div>
          );
        })}
      </div>

      <p className="mt-4 text-xs text-zinc-500">
        First time using effects? Restart PipeWire once so the chain
        loads:{" "}
        <code className="rounded bg-zinc-800 px-1.5 py-0.5">
          systemctl --user restart pipewire wireplumber
        </code>
      </p>
    </section>
  );
}
