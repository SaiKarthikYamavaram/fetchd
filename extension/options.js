const savedEl = document.getElementById("saved");
let saveTimer;

function flashSaved() {
  savedEl.textContent = "Saved";
  clearTimeout(saveTimer);
  saveTimer = setTimeout(() => (savedEl.textContent = ""), 1200);
}

async function load() {
  const s = await getSettings();
  document.getElementById("enabled").checked = s.enabled;
  document.getElementById("grabMedia").checked = s.grabMedia;
  document.getElementById("intercept").checked = s.intercept;
  document.getElementById("minSizeKb").value = s.minSizeKb;
  document.getElementById("excludeDomains").value = s.excludeDomains.join("\n");
  for (const box of document.querySelectorAll("[data-type]")) {
    box.checked = !!s.types[box.dataset.type];
  }
}

async function save() {
  const types = {};
  for (const box of document.querySelectorAll("[data-type]")) types[box.dataset.type] = box.checked;
  const excludeDomains = document
    .getElementById("excludeDomains").value
    .split("\n").map((l) => l.trim()).filter(Boolean);

  await setSettings({
    enabled: document.getElementById("enabled").checked,
    grabMedia: document.getElementById("grabMedia").checked,
    intercept: document.getElementById("intercept").checked,
    minSizeKb: Math.max(0, Number(document.getElementById("minSizeKb").value) || 0),
    types,
    excludeDomains,
  });
  flashSaved();
}

// Save on any change — no explicit save button needed.
document.addEventListener("change", save);
document.getElementById("excludeDomains").addEventListener("input", () => {
  clearTimeout(saveTimer);
  saveTimer = setTimeout(save, 500);
});

load();
