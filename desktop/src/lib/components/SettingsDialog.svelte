<script lang="ts">
  import Modal from "./Modal.svelte";
  import { kahui } from "../state.svelte";
  import { api, errorText, type NetworkCheck } from "../api";

  interface Props {
    onclose: () => void;
  }
  let { onclose }: Props = $props();

  let name = $state(kahui.status?.display_name ?? "");
  let working = $state(false);
  let problem = $state("");
  let syncNote = $state("");
  let key = $state("");
  let keyShown = $state(false);
  let peer = $state("");
  let peerNote = $state("");
  let check = $state<NetworkCheck | null>(null);
  let checking = $state(false);

  async function runCheck() {
    if (checking) return;
    checking = true;
    check = null;
    try {
      check = await api.checkNetwork();
    } catch (err) {
      problem = errorText(err);
    } finally {
      checking = false;
    }
  }

  async function revealKey() {
    try {
      key = await kahui.backupPhrase();
      keyShown = true;
    } catch (err) {
      problem = errorText(err);
    }
  }

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

  async function connect() {
    if (!peer.trim()) return;
    peerNote = "Connecting…";
    try {
      await api.dial(peer.trim());
      peerNote = "Dialling.";
      peer = "";
    } catch (err) {
      peerNote = errorText(err);
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
  subtitle="No account. Your identity is a key on this device."
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

    <dt>Reachable</dt>
    <dd>
      <span class:good={kahui.standing.good}>{kahui.standing.label}</span>
      <div class="faint explain">{kahui.standing.detail}</div>
    </dd>
  </dl>

  <!-- Whether people can reach this machine is the single question that decides
       if you can host, and until now the only way to find out was to try. -->
  <section class="peer">
    <h3>Can people reach you?</h3>
    {#if check}
      <div class="check">
        {#each check.rows as row}
          <span class="ck-label">{row.label}</span>
          <span class="ck-value" class:bad={row.ok === false} class:good={row.ok === true}>
            {row.value}
          </span>
        {/each}
      </div>
      <p class="note" class:warn={!check.reachable}>{check.advice}</p>
    {/if}
    <button class="btn" onclick={runCheck} disabled={checking}>
      {checking ? "Checking…" : check ? "Check again" : "Check"}
    </button>
  </section>

  <!-- The escape hatch for the one case the protocol cannot bootstrap itself:
       nobody can reach you, and you know nobody yet, so there is no member to
       relay for you. Dial one by hand and the rest follows. -->
  <section class="peer">
    <h3>Connect to a peer</h3>
    <div class="row">
      <input
        class="field mono"
        bind:value={peer}
        placeholder="/ip4/203.0.113.4/tcp/4001/p2p/12D3Koo…"
        spellcheck="false"
        onkeydown={(e) => e.key === "Enter" && connect()}
      />
      <button class="btn" onclick={connect} disabled={!peer.trim()}>Connect</button>
    </div>
    {#if peerNote}<p class="faint note">{peerNote}</p>{/if}
  </section>

  <!-- The one piece of genuinely unrecoverable state. Worth a section rather
       than a line, and worth hiding until asked for. -->
  <section class="key">
    <h3>Your key</h3>
    {#if keyShown}
      <textarea class="field code mono" readonly rows="2" value={key} onclick={(e) => e.currentTarget.select()}
      ></textarea>
      <p class="warn">Anyone with this can post as you. Keep it private.</p>
    {:else}
      <p class="faint keyhint">Back this up. Lose it and the identity is gone for good.</p>
      <button class="btn" onclick={revealKey}>Show my key</button>
    {/if}
  </section>

  {#if problem}<p class="problem">{problem}</p>{/if}

  <div class="actions">
    <button class="btn" onclick={onclose}>Close</button>
    <button class="btn primary" onclick={save} disabled={!changed || working}>
      {working ? "Saving…" : "Save name"}
    </button>
  </div>
</Modal>

<style>
  .check {
    display: grid;
    grid-template-columns: 5.5rem 1fr;
    gap: 0.3rem 0.8rem;
    font-size: 0.78rem;
    margin-bottom: 0.7rem;
  }
  .ck-label {
    color: var(--faint);
  }
  .ck-value {
    color: var(--dim);
    word-break: break-word;
  }
  .ck-value.good {
    color: var(--ok, #6fcf97);
  }
  .ck-value.bad {
    color: var(--dim);
  }
  .note.warn {
    color: var(--danger);
  }

  .label {
    display: block;
    font-size: 0.72rem;
    font-weight: 700;
    letter-spacing: 0.08em;
    text-transform: uppercase;
    color: var(--faint);
    margin-bottom: 0.35rem;
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

  .good {
    color: var(--online);
  }

  .explain {
    margin-top: 0.15rem;
    line-height: 1.45;
  }

  .peer,
  .key {
    margin-top: 1.2rem;
    padding-top: 1rem;
    border-top: 1px solid var(--line);
  }
  .row {
    display: flex;
    gap: 0.5rem;
  }
  .row .field {
    font-size: 0.78rem;
  }
  .note {
    font-size: 0.8rem;
    margin: 0.5rem 0 0;
  }
  .peer h3,
  .key h3 {
    margin: 0 0 0.4rem;
    font-size: 0.72rem;
    font-weight: 700;
    letter-spacing: 0.08em;
    text-transform: uppercase;
    color: var(--faint);
  }
  .keyhint {
    font-size: 0.82rem;
    line-height: 1.55;
    margin: 0 0 0.7rem;
  }
  .code {
    resize: none;
    font-size: 0.78rem;
    line-height: 1.5;
    word-break: break-all;
    color: var(--star);
  }
  .warn {
    font-size: 0.8rem;
    line-height: 1.5;
    color: var(--danger);
    margin: 0.6rem 0 0;
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
