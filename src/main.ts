import { invoke } from "@tauri-apps/api/core";

interface ToneProfile {
  apps: string[];
  tone: string;
}

interface PolishConfig {
  enabled: boolean;
  base_url: string;
  model: string;
  api_key_env: string;
  per_app_tone: boolean;
  tone_profiles: ToneProfile[];
  default_tone: string;
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
const toneRows = document.getElementById("tone_rows") as HTMLDivElement;
const addToneBtn = document.getElementById("add_tone") as HTMLButtonElement;
const saveKeyBtn = document.getElementById("save_key") as HTMLButtonElement;
const keyStatus = document.getElementById("key_status") as HTMLSpanElement;
const autostart = document.getElementById("autostart") as HTMLInputElement;
const apiKeyInput = () => form.elements.namedItem("polish_api_key") as HTMLInputElement;
const baseUrlInput = () => form.elements.namedItem("polish_base_url") as HTMLInputElement;

function setStatus(msg: string, ok = true) {
  status.textContent = msg;
  status.style.color = ok ? "" : "#d33";
}

function addToneRow(profile: ToneProfile = { apps: [], tone: "" }) {
  const row = document.createElement("div");
  row.className = "tone-row";

  const apps = document.createElement("input");
  apps.type = "text";
  apps.className = "apps";
  apps.placeholder = "slack, discord";
  apps.value = profile.apps.join(", ");

  const tone = document.createElement("input");
  tone.type = "text";
  tone.className = "tone";
  tone.placeholder = "casual chat";
  tone.value = profile.tone;

  const remove = document.createElement("button");
  remove.type = "button";
  remove.textContent = "×";
  remove.addEventListener("click", () => row.remove());

  row.append(apps, tone, remove);
  toneRows.append(row);
}

function renderToneProfiles(profiles: ToneProfile[]) {
  toneRows.replaceChildren();
  profiles.forEach((p) => addToneRow(p));
}

// A rule needs a tone and at least one app token; blank rows are dropped.
function collectToneProfiles(): ToneProfile[] {
  return [...toneRows.querySelectorAll(".tone-row")]
    .map((row) => {
      const apps = (row.querySelector(".apps") as HTMLInputElement).value
        .split(",")
        .map((s) => s.trim())
        .filter(Boolean);
      const tone = (row.querySelector(".tone") as HTMLInputElement).value.trim();
      return { apps, tone };
    })
    .filter((p) => p.tone && p.apps.length > 0);
}

addToneBtn.addEventListener("click", () => addToneRow());

// Show only whether a key is stored for the current endpoint, never its value.
async function refreshKeyStatus() {
  const baseUrl = baseUrlInput().value.trim();
  if (!baseUrl) {
    keyStatus.textContent = "";
    return;
  }
  try {
    const stored = await invoke<boolean>("has_polish_api_key", { baseUrl });
    keyStatus.textContent = stored ? "key stored" : "no key stored";
    keyStatus.style.color = stored ? "" : "#888";
  } catch {
    keyStatus.textContent = "";
  }
}

saveKeyBtn.addEventListener("click", async () => {
  const baseUrl = baseUrlInput().value.trim();
  if (!baseUrl) {
    setStatus("Set a base URL before saving a key.", false);
    return;
  }
  const key = apiKeyInput().value;
  try {
    await invoke("set_polish_api_key", { baseUrl, key });
    apiKeyInput().value = "";
    setStatus(key ? "Key saved to keyring." : "Key cleared from keyring.");
    await refreshKeyStatus();
  } catch (err) {
    setStatus(`Key save failed: ${err}`, false);
  }
});

// A key is scoped to its base_url, so re-check when the endpoint changes.
form.addEventListener("input", (e) => {
  if ((e.target as HTMLElement)?.getAttribute("name") === "polish_base_url") {
    refreshKeyStatus();
  }
});

// Autostart is OS state (login item), not part of config.json, so apply it
// immediately and revert the checkbox if the OS call fails.
autostart.addEventListener("change", async () => {
  try {
    await invoke("set_autostart", { enabled: autostart.checked });
    setStatus(autostart.checked ? "Autostart enabled." : "Autostart disabled.");
  } catch (err) {
    autostart.checked = !autostart.checked;
    setStatus(`Autostart change failed: ${err}`, false);
  }
});

function fill(cfg: Config) {
  (form.elements.namedItem("aavaaz_url") as HTMLInputElement).value = cfg.aavaaz_url;
  (form.elements.namedItem("model") as HTMLSelectElement).value = cfg.model;
  (form.elements.namedItem("language") as HTMLInputElement).value = cfg.language ?? "";
  (form.elements.namedItem("hotkey") as HTMLInputElement).value = cfg.hotkey;
  (form.elements.namedItem("polish_enabled") as HTMLInputElement).checked = cfg.polish.enabled;
  (form.elements.namedItem("polish_base_url") as HTMLInputElement).value = cfg.polish.base_url;
  (form.elements.namedItem("polish_model") as HTMLInputElement).value = cfg.polish.model;
  (form.elements.namedItem("polish_api_key_env") as HTMLInputElement).value = cfg.polish.api_key_env;
  (form.elements.namedItem("per_app_tone") as HTMLInputElement).checked = cfg.polish.per_app_tone;
  (form.elements.namedItem("default_tone") as HTMLInputElement).value = cfg.polish.default_tone;
  renderToneProfiles(cfg.polish.tone_profiles);
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
      base_url: data.get("polish_base_url") as string,
      model: data.get("polish_model") as string,
      api_key_env: data.get("polish_api_key_env") as string,
      per_app_tone: data.get("per_app_tone") === "on",
      tone_profiles: collectToneProfiles(),
      default_tone: (data.get("default_tone") as string).trim() || "natural prose",
    },
  };
}

let current: Config;

(async () => {
  current = await invoke<Config>("get_config");
  fill(current);
  await refreshKeyStatus();
  autostart.checked = await invoke<boolean>("get_autostart");
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
