<script lang="ts">
  import Modal from "./Modal.svelte";
  import { kahui } from "../state.svelte";
  import { errorText } from "../api";

  interface Props {
    onclose: () => void;
  }
  let { onclose }: Props = $props();

  let name = $state("");
  let working = $state(false);
  let problem = $state("");

  async function go() {
    if (working || !name.trim()) return;
    working = true;
    problem = "";
    try {
      await kahui.createChannel(name);
      onclose();
    } catch (err) {
      problem = errorText(err);
    } finally {
      working = false;
    }
  }
</script>

<Modal
  title="New channel"
  subtitle="Channel names are lowercased, and their identifier is derived from the name. Two members who create the same channel independently end up in the same one."
  {onclose}
>
  <div class="row">
    <span class="hash">#</span>
    <input
      class="field"
      bind:value={name}
      placeholder="random"
      maxlength="64"
      onkeydown={(e) => e.key === "Enter" && go()}
    />
  </div>

  {#if problem}<p class="problem">{problem}</p>{/if}

  <div class="actions">
    <button class="btn" onclick={onclose}>Cancel</button>
    <button class="btn primary" onclick={go} disabled={working || !name.trim()}>
      {working ? "Creating…" : "Create"}
    </button>
  </div>
</Modal>

<style>
  .row {
    display: flex;
    align-items: center;
    gap: 0.5rem;
  }
  .hash {
    color: var(--faint);
    font-size: 1.2rem;
    font-weight: 600;
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
