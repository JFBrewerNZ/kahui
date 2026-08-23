<script lang="ts">
  import Modal from "./Modal.svelte";
  import { kahui } from "../state.svelte";
  import { api, errorText } from "../api";

  interface Props {
    onclose: () => void;
  }
  let { onclose }: Props = $props();

  let name = $state(kahui.status?.display_name ?? "");
  let working = $state(false);
  let problem = $state("");
  let syncNote = $state("");

  const changed = $derived(name.trim() !== (kahui.status?.display_name ?? "") && !!name.trim());

  async function save() {
    if (!changed || working) return;
    working = true;
    problem = "";
    try {
      await kahui.rename(name);
      onclose();
    } catch (err) {
      problem = errorText(err);
    } finally {
      working = false;
    }
  }

  async function sync() {
    syncNote = "Asking…";
    try {
      const asked = await api.syncNow();
      syncNote = asked === 0 ? "No peers connected right now." : `Asked ${asked} peer${asked === 1 ? "" : "s"}.`;
    } catch (err) {
      syncNote = errorText(err);
    }
  }
</script>

<Modal
  title="You"
  subtitle="Your identity is a keypair on this machine. There is no account and no password — copying the data directory copies the identity with it."
  {onclose}
>
  <label class="label" for="display-name">Display name</label>
  <input
    id="display-name"
    class="field"
    bind:value={name}
    maxlength="64"
    onkeydown={(e) => e.key === "Enter" && save()}
  />
  <p class="hint faint">
    Changing it writes a signed event, so every member sees the new name once it reaches them.
  </p>

  <dl>
    <dt>Your id</dt>
    <dd class="mono break">{kahui.status?.user ?? "—"}</dd>

    <dt>Network id</dt>
    <dd class="mono break">{kahui.status?.peer_id ?? "—"}</dd>

    <dt>Addresses</dt>
    <dd class="mono break">
      {#each kahui.status?.listen_addrs ?? [] as addr}
        <div>{addr}</div>
      {:else}
        <div class="faint">none yet</div>
      {/each}
    </dd>

    <dt>Data</dt>
    <dd class="mono break">{kahui.dataDir}</dd>

    <dt>Peers</dt>
    <dd>
      {kahui.peerCount} connected
      <button class="link" onclick={sync}>sync now</button>
      {#if syncNote}<span class="faint"> — {syncNote}</span>{/if}
    </dd>
  </dl>

  {#if problem}<p class="problem">{problem}</p>{/if}

  <div class="actions">
    <button class="btn" onclick={onclose}>Close</button>
    <button class="btn primary" onclick={save} disabled={!changed || working}>
      {working ? "Saving…" : "Save name"}
    </button>
  </div>
</Modal>

<style>
  .label {
    display: block;
    font-size: 0.72rem;
    font-weight: 700;
    letter-spacing: 0.08em;
    text-transform: uppercase;
    color: var(--faint);
    margin-bottom: 0.35rem;
  }

  .hint {
    font-size: 0.82rem;
    margin: 0.5rem 0 0;
    line-height: 1.5;
  }

  dl {
    margin: 1.2rem 0 0;
    display: grid;
    grid-template-columns: 6.5rem 1fr;
    gap: 0.45rem 0.8rem;
    font-size: 0.8rem;
    padding-top: 1rem;
    border-top: 1px solid var(--line);
  }
  dt {
    color: var(--faint);
    font-weight: 600;
  }
  dd {
    margin: 0;
    color: var(--dim);
  }
  .break {
    overflow-wrap: anywhere;
    user-select: text;
  }

  .link {
    color: var(--star);
    text-decoration: underline;
    font-size: 0.8rem;
  }

  .problem {
    margin: 0.8rem 0 0;
    font-size: 0.85rem;
    color: var(--danger);
  }

  .actions {
    display: flex;
    justify-content: flex-end;
    gap: 0.5rem;
    margin-top: 1.2rem;
  }
</style>
