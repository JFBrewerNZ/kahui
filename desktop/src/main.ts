import "./app.css";
import { mount } from "svelte";
import App from "./App.svelte";

// WebView2 offers Edge's own context menu, complete with Reload and View
// Source. It gives the game away and none of it applies here. Text fields keep
// theirs, so right-click paste still works.
document.addEventListener("contextmenu", (event) => {
  if ((event.target as HTMLElement | null)?.closest("input, textarea")) return;
  event.preventDefault();
});

// Same reasoning for reloading: in an app it looks like a crash, not a refresh.
if (!import.meta.env.DEV) {
  document.addEventListener("keydown", (event) => {
    const reload =
      event.key === "F5" || ((event.ctrlKey || event.metaKey) && event.key === "r");
    if (reload) event.preventDefault();
  });
}

export default mount(App, { target: document.getElementById("app")! });
