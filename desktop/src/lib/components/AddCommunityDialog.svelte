<script lang="ts">
  import Modal from "./Modal.svelte";
  import { kahui } from "../state.svelte";
  import { errorText } from "../api";

  interface Props {
    onclose: () => void;
    /// An invite that arrived by link, ready to confirm.
    prefill?: string;
  }
  let { onclose, prefill = "" }: Props = $props();

  let tab = $state<"join" | "create">("join");
  let name = $state("");
  let invite = $state("");

  // Filled in when a kahui:// link opened this dialog.
  $effect(() => {
    if (prefill) invite = prefill;
  });
  let working = $state(false);
  let problem = $state("");

  async function go() {
    if (working) return;
    problem = "";
    working = true;
    try {
      if (tab === "create") {
        await kahui.createCommunity(name);
      } else {
        await kahui.joinCommunity(invite);
      }
      onclose();
    } catch (err) {
      problem = errorText(err);
    } finally {
      working = false;
    }
  }
</script>

<Modal
  title={tab === "join" ? "Join a community" : "Create a community"}
  subtitle={tab === "join" ? "Paste a code or link." : "You will be its first member."}
  {onclose}
>
  <div class="tabs">
    <button class:on={tab === "join"} onclick={() => (tab = "join")}>Join</button>
    <button class:on={tab === "create"} onclick={() => (tab = "create")}>Create</button>
  </div>

  {#if tab === "join"}
    <textarea
      class="field invite mono"
      rows="4"
      bind:value={invite}
      placeholder="kahui1…"
      spellcheck="false"
    ></textarea>
  {:else}
    <input
      class="field"
      bind:value={name}
      placeholder="Community name"
      maxlength="64"
      onkeydown={(e) => e.key === "Enter" && go()}
    />
    <p class="hint faint">Starts with a <strong>#general</strong> channel.</p>
  {/if}

  {#if problem}<p class="problem">{problem}</p>{/if}

  <div class="actions">
    <button class="btn" onclick={onclose}>Cancel</button>
    <button
      class="btn primary"
      onclick={go}
      disabled={working || (tab === "create" ? !name.trim() : !invite.trim())}
    >
      {#if working}
        {tab === "create" ? "Creating…" : "Joining…"}
      {:else}
        {tab === "create" ? "Create" : "Join"}
      {/if}
    </button>
  </div>
</Modal>

<style>
  .tabs {
    display: flex;
    gap: 0.25rem;
    padding: 0.25rem;
    background: var(--rail);
    border-radius: var(--radius);
    margin-bottom: 0.9rem;
  }
  .tabs button {
    flex: 1;
    padding: 0.45rem;
    border-radius: 6px;
    color: var(--dim);
    font-weight: 550;
  }
  .tabs button.on {
    background: var(--raised);
    color: var(--text);
  }

  .invite {
    resize: none;
    font-size: 0.8rem;
    line-height: 1.5;
    word-break: break-all;
  }

  .hint {
    font-size: 0.83rem;
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
