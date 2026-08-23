<script lang="ts">
  import type { Snippet } from "svelte";

  interface Props {
    title: string;
    subtitle?: string;
    onclose: () => void;
    children: Snippet;
  }

  let { title, subtitle = "", onclose, children }: Props = $props();

  let card = $state<HTMLDivElement | null>(null);

  // Move focus into the dialog so Escape and Tab do what a keyboard user
  // expects, rather than acting on whatever was behind it.
  $effect(() => {
    card?.focus();
  });

  function onkeydown(event: KeyboardEvent) {
    if (event.key === "Escape") onclose();
  }
</script>

<svelte:window {onkeydown} />

<div class="backdrop">
  <!-- A real button rather than a click handler on the backdrop: clicking away
       to dismiss is an action, and actions should be reachable by keyboard. -->
  <button class="scrim" aria-label="Close {title}" onclick={onclose}></button>

  <div
    class="card"
    bind:this={card}
    role="dialog"
    aria-modal="true"
    aria-label={title}
    tabindex="-1"
  >
    <header>
      <h2>{title}</h2>
      {#if subtitle}<p class="muted">{subtitle}</p>{/if}
    </header>
    {@render children()}
  </div>
</div>

<style>
  .backdrop {
    position: fixed;
    inset: 0;
    display: grid;
    place-items: center;
    z-index: 50;
    animation: fade 0.12s ease-out;
  }

  .scrim {
    position: absolute;
    inset: 0;
    background: rgba(4, 7, 13, 0.68);
    cursor: default;
  }

  .card {
    position: relative;
    width: min(30rem, calc(100vw - 3rem));
    max-height: calc(100vh - 3rem);
    overflow-y: auto;
    background: var(--chat);
    border: 1px solid var(--line);
    border-radius: 12px;
    padding: 1.4rem;
    box-shadow: 0 24px 60px rgba(0, 0, 0, 0.55);
    animation: rise 0.14s ease-out;
    outline: none;
  }

  header {
    margin-bottom: 1.1rem;
  }

  h2 {
    margin: 0;
    font-size: 1.15rem;
    font-weight: 620;
  }

  p {
    margin: 0.35rem 0 0;
    font-size: 0.86rem;
    line-height: 1.55;
  }

  @keyframes fade {
    from {
      opacity: 0;
    }
  }
  @keyframes rise {
    from {
      opacity: 0;
      transform: translateY(6px);
    }
  }
</style>
