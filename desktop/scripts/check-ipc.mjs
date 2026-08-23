/**
 * Checks that every call the window makes actually exists on the Rust side.
 *
 * The boundary between the two halves of this app is a set of string names and
 * argument names. Rust checks its own half — `generate_handler!` will not
 * compile if a command is missing — and TypeScript checks its own, but nothing
 * checks that the two agree. A typo there compiles cleanly on both sides and
 * fails at runtime, in a dialog, in front of a user.
 *
 *     node scripts/check-ipc.mjs
 */

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const here = dirname(fileURLToPath(import.meta.url));
const rustSource = readFileSync(join(here, "..", "src-tauri", "src", "lib.rs"), "utf8");
const tsSource = readFileSync(join(here, "..", "src", "lib", "api.ts"), "utf8");

/** Tauri maps a JavaScript `someArg` onto a Rust `some_arg`. */
const toSnake = (name) => name.replace(/[A-Z]/g, (c) => `_${c.toLowerCase()}`);

/**
 * Splits an argument list on commas that are actually separators.
 *
 * A naive split mangles `state: State<'_, NodeState>` into two arguments,
 * because generic parameters contain commas of their own.
 */
function splitArgs(text) {
  const parts = [];
  let depth = 0;
  let current = "";
  for (const ch of text) {
    if (ch === "<" || ch === "(" || ch === "[") depth += 1;
    else if (ch === ">" || ch === ")" || ch === "]") depth -= 1;
    if (ch === "," && depth === 0) {
      parts.push(current);
      current = "";
    } else {
      current += ch;
    }
  }
  parts.push(current);
  return parts.map((part) => part.trim()).filter(Boolean);
}

// --- what Rust offers -------------------------------------------------------

const commands = new Map();
for (const match of rustSource.matchAll(
  /#\[tauri::command\]\s*(?:pub\s+)?async\s+fn\s+(\w+)\s*\(([^)]*)\)/g
)) {
  const [, name, rawArgs] = match;
  const args = splitArgs(rawArgs)
    .map((arg) => arg.split(":")[0].trim())
    // Injected by Tauri, never sent from the window.
    .filter((arg) => arg !== "state" && arg !== "app" && arg !== "window");
  commands.set(name, new Set(args));
}

// Names listed in generate_handler! are the ones actually reachable.
const handlerBlock = rustSource.match(/generate_handler!\[([\s\S]*?)\]/);
const registered = new Set(
  (handlerBlock?.[1] ?? "")
    .split(",")
    .map((name) => name.trim())
    .filter(Boolean)
);

// --- what the window calls --------------------------------------------------

const problems = [];

for (const match of tsSource.matchAll(/invoke<[^>]*>\(\s*"([^"]+)"(?:\s*,\s*\{([^}]*)\})?/g)) {
  const [, name, rawArgs = ""] = match;

  if (!commands.has(name)) {
    problems.push(`the window calls "${name}", which is not a #[tauri::command]`);
    continue;
  }
  if (!registered.has(name)) {
    problems.push(`"${name}" exists but is missing from generate_handler!, so it is unreachable`);
  }

  const expected = commands.get(name);
  const passed = splitArgs(rawArgs).map((arg) => arg.split(":")[0].trim());

  for (const arg of passed) {
    if (!expected.has(toSnake(arg))) {
      problems.push(
        `"${name}" is called with \`${arg}\`, but Rust expects (${[...expected].join(", ") || "no arguments"})`
      );
    }
  }
  for (const arg of expected) {
    if (!passed.some((p) => toSnake(p) === arg)) {
      problems.push(`"${name}" needs \`${arg}\`, which the window does not send`);
    }
  }
}

if (problems.length > 0) {
  console.error("The window and the Rust side disagree:\n");
  for (const problem of problems) console.error(`  ${problem}`);
  process.exit(1);
}

console.log(`ok - all ${commands.size} commands line up with the calls that use them`);
