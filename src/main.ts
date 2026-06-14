import { invoke } from "@tauri-apps/api/core";

interface PolishConfig {
  enabled: boolean;
  provider: string;
  model: string;
  api_key_env: string;
  per_app_tone: boolean;
}

interface Config {
  aavaaz_url: string;
  model: string;
  language: string | null;
  hotkey: string;
  polish: PolishConfig;
  hotwords: string[];
  inject_method: "type" | "paste";
}

const form = document.getElementById("settings") as HTMLFormElement;
const status = document.getElementById("status") as HTMLParagraphElement;
const testBtn = document.getElementById("test") as HTMLButtonElement;

function setStatus(msg: string, ok = true) {
  status.textContent = msg;
  status.style.color = ok ? "" : "#d33";
}

function fill(cfg: Config) {
  (form.elements.namedItem("aavaaz_url") as HTMLInputElement).value = cfg.aavaaz_url;
  (form.elements.namedItem("model") as HTMLSelectElement).value = cfg.model;
  (form.elements.namedItem("language") as HTMLInputElement).value = cfg.language ?? "";
  (form.elements.namedItem("hotkey") as HTMLInputElement).value = cfg.hotkey;
  (form.elements.namedItem("polish_enabled") as HTMLInputElement).checked = cfg.polish.enabled;
  (form.elements.namedItem("polish_model") as HTMLInputElement).value = cfg.polish.model;
  (form.elements.namedItem("per_app_tone") as HTMLInputElement).checked = cfg.polish.per_app_tone;
  (form.elements.namedItem("hotwords") as HTMLInputElement).value = cfg.hotwords.join(", ");
}

function collect(prev: Config): Config {
  const data = new FormData(form);
  const lang = (data.get("language") as string).trim();
  const hotwords = (data.get("hotwords") as string)
    .split(",")
    .map((s) => s.trim())
    .filter(Boolean);
  return {
    ...prev,
    aavaaz_url: data.get("aavaaz_url") as string,
    model: data.get("model") as string,
    language: lang ? lang : null,
    hotkey: data.get("hotkey") as string,
    hotwords,
    polish: {
      ...prev.polish,
      enabled: data.get("polish_enabled") === "on",
      model: data.get("polish_model") as string,
      per_app_tone: data.get("per_app_tone") === "on",
    },
  };
}

let current: Config;

(async () => {
  current = await invoke<Config>("get_config");
  fill(current);
})();

form.addEventListener("submit", async (e) => {
  e.preventDefault();
  const updated = collect(current);
  try {
    await invoke("save_config", { newCfg: updated });
    current = updated;
    setStatus("Saved. Restart qol to apply the new hotkey.");
  } catch (err) {
    setStatus(`Save failed: ${err}`, false);
  }
});

testBtn.addEventListener("click", async () => {
  const url = (form.elements.namedItem("aavaaz_url") as HTMLInputElement).value;
  setStatus("Connecting…");
  try {
    const msg = await invoke<string>("test_aavaaz", { url });
    setStatus(msg);
  } catch (err) {
    setStatus(`${err}`, false);
  }
});
