<script lang="ts">
  import Modal from "./Modal.svelte";
  import { kahui } from "../state.svelte";
  import { api, errorText, type InviteText } from "../api";

  interface Props {
    onclose: () => void;
  }
  let { onclose }: Props = $props();

  let invite = $state<InviteText | null>(null);
  let problem = $state("");
  let copied = $state(false);
  let box = $state<HTMLTextAreaElement | null>(null);

  $effect(() => {
    const community = kahui.communityId;
    if (!community) return;
    api
      .makeInvite(community)
      .then((result) => (invite = result))
      .catch((err) => (problem = errorText(err)));
  });

  async function copy() {
    if (!invite) return;
    try {
      await navigator.clipboard.writeText(invite.link);
      copied = true;
      setTimeout(() => (copied = false), 1800);
    } catch {
      // Clipboard access can be refused. Select it so Ctrl-C still works.
      box?.select();
      problem = "Copy it yourself — it is selected.";
    }
  }
</script>

<Modal title="Invite to {invite?.communityName ?? kahui.community?.name ?? 'this community'}" {onclose}>
  {#if invite}
    <textarea
      class="field link mono"
      bind:this={box}
      readonly
      rows="3"
      value={invite.link}
      onclick={() => box?.select()}
    ></textarea>
  {:else if problem}
    <p class="problem">{problem}</p>
  {:else}
    <p class="muted">Building an invite…</p>
  {/if}

  <div class="actions">
    <button class="btn" onclick={onclose}>Done</button>
    <button class="btn primary" onclick={copy} disabled={!invite}>
      {copied ? "Copied" : "Copy"}
    </button>
  </div>
</Modal>

<style>
  .link {
    resize: none;
    font-size: 0.78rem;
    line-height: 1.55;
    word-break: break-all;
    color: var(--star);
  }
  .problem {
    margin: 0;
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
